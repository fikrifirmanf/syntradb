//! Bounded correlated request traces; no random sampling and no SQL dependencies.
use crate::{LatencyHistogram, Result};
use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestTiming {
    pub worker: usize,
    pub arrival_index: u64,
    pub backend_pid: i32,
    pub scheduled_us: u64,
    pub ready_us: u64,
    pub dispatched_us: u64,
    pub completed_us: u64,
    pub previous_completed_us: Option<u64>,
    pub sqlstate: Option<String>,
}

impl RequestTiming {
    fn validate(&self) -> Result<()> {
        if self.backend_pid <= 0
            || self.worker >= 16
            || self.dispatched_us < self.scheduled_us
            || self.dispatched_us < self.ready_us
            || self.completed_us < self.dispatched_us
            || self
                .previous_completed_us
                .is_some_and(|previous| previous > self.ready_us)
            || self.scheduled_to_commit_us() > 120_000_000
        {
            return Err("invalid correlated request timing".into());
        }
        Ok(())
    }
    pub fn dispatch_lag_us(&self) -> u64 {
        self.dispatched_us.saturating_sub(self.scheduled_us)
    }
    pub fn backlog_us(&self) -> u64 {
        self.ready_us.saturating_sub(self.scheduled_us)
    }
    pub fn after_ready_us(&self) -> u64 {
        self.dispatched_us
            .saturating_sub(self.ready_us.max(self.scheduled_us))
    }
    pub fn request_to_commit_us(&self) -> u64 {
        self.completed_us.saturating_sub(self.dispatched_us)
    }
    pub fn scheduled_to_commit_us(&self) -> u64 {
        self.completed_us.saturating_sub(self.scheduled_us)
    }
    fn identity(&self) -> (usize, u64) {
        (self.worker, self.arrival_index)
    }
    fn report(&self) -> Value {
        json!({ "worker": self.worker, "arrival_index": self.arrival_index, "backend_pid": self.backend_pid,
            "scheduled_us": self.scheduled_us, "ready_us": self.ready_us,
            "dispatched_us": self.dispatched_us, "completed_us": self.completed_us,
            "previous_completed_us": self.previous_completed_us, "sqlstate": self.sqlstate,
            "dispatch_lag_us": self.dispatch_lag_us(), "backlog_us": self.backlog_us(),
            "after_ready_us": self.after_ready_us(), "request_to_commit_us": self.request_to_commit_us(),
            "scheduled_to_commit_us": self.scheduled_to_commit_us() })
    }
}

type Score = fn(&RequestTiming) -> u64;

pub struct DispatchDiagnostics {
    limit: usize,
    attempts: u64,
    over_1ms: u64,
    over_5ms: u64,
    over_10ms: u64,
    lag: LatencyHistogram,
    backlog: LatencyHistogram,
    after_ready: LatencyHistogram,
    tails: [Vec<RequestTiming>; 3],
}

impl DispatchDiagnostics {
    pub fn new(limit: usize) -> Result<Self> {
        if !(1..=64).contains(&limit) {
            return Err("trace capacity must be between 1 and 64".into());
        }
        Ok(Self {
            limit,
            attempts: 0,
            over_1ms: 0,
            over_5ms: 0,
            over_10ms: 0,
            lag: LatencyHistogram::new()?,
            backlog: LatencyHistogram::new()?,
            after_ready: LatencyHistogram::new()?,
            tails: std::array::from_fn(|_| Vec::with_capacity(limit)),
        })
    }

    fn scores() -> [Score; 3] {
        [
            RequestTiming::dispatch_lag_us,
            RequestTiming::request_to_commit_us,
            RequestTiming::scheduled_to_commit_us,
        ]
    }

    fn retain(&mut self, event: &RequestTiming) {
        for (bucket, score) in self.tails.iter_mut().zip(Self::scores()) {
            if bucket.iter().any(|old| old.identity() == event.identity()) {
                continue;
            }
            if bucket.len() < self.limit {
                bucket.push(event.clone());
                continue;
            }
            if let Some((index, smallest)) = bucket
                .iter()
                .enumerate()
                .min_by_key(|(_, old)| (score(old), old.identity()))
            {
                if (score(event), event.identity()) > (score(smallest), smallest.identity()) {
                    bucket[index] = event.clone();
                }
            }
        }
    }

    pub fn record(&mut self, event: RequestTiming) -> Result<()> {
        event.validate()?;
        self.lag.record_us(event.dispatch_lag_us())?;
        self.backlog.record_us(event.backlog_us())?;
        self.after_ready.record_us(event.after_ready_us())?;
        self.attempts += 1;
        self.over_1ms += u64::from(event.dispatch_lag_us() >= 1000);
        self.over_5ms += u64::from(event.dispatch_lag_us() >= 5000);
        self.over_10ms += u64::from(event.dispatch_lag_us() >= 10000);
        self.retain(&event);
        Ok(())
    }

    pub fn merge(&mut self, other: &Self) -> Result<()> {
        if self.limit != other.limit {
            return Err("incompatible trace capacities".into());
        }
        self.lag.merge(&other.lag)?;
        self.backlog.merge(&other.backlog)?;
        self.after_ready.merge(&other.after_ready)?;
        self.attempts += other.attempts;
        self.over_1ms += other.over_1ms;
        self.over_5ms += other.over_5ms;
        self.over_10ms += other.over_10ms;
        // Each global top-K event must occur in its worker's top-K bucket.
        // retain deduplicates identities shared by the three views.
        for event in other.tails.iter().flatten() {
            self.retain(event);
        }
        Ok(())
    }

    pub fn report(&self) -> Value {
        let views: Vec<_> = self
            .tails
            .iter()
            .zip(Self::scores())
            .map(|(events, score)| {
                let mut events: Vec<_> = events.iter().collect();
                events.sort_by_key(|event| std::cmp::Reverse((score(event), event.identity())));
                events
                    .into_iter()
                    .map(RequestTiming::report)
                    .collect::<Vec<_>>()
            })
            .collect();
        json!({ "attempts_seen": self.attempts, "retained_per_category": self.limit,
            "offset_unit": "microseconds from phase start; readiness before start is clamped to zero",
            "selection": "largest observed timings per category, not a representative sample",
            "dispatch_lag": self.lag.report(), "ready_after_due": self.backlog.report(),
            "after_ready_dispatch": self.after_ready.report(),
            "dispatch_lag_at_least_1ms": self.over_1ms, "dispatch_lag_at_least_5ms": self.over_5ms,
            "dispatch_lag_at_least_10ms": self.over_10ms,
            "slowest_dispatch": views[0], "slowest_request": views[1], "slowest_scheduled": views[2] })
    }
}
