//! Sampled backend states with client-side time bounds, not inferred durations.
use crate::Result;
use serde_json::{json, Value};
use std::collections::BTreeMap;

#[derive(Debug)]
pub struct ActivityObservation {
    pub pid: i32,
    pub state: Option<String>,
    pub wait_type: Option<String>,
    pub wait_event: Option<String>,
}

#[derive(Debug)]
pub struct ActivitySample {
    pub sent_us: u64,
    pub received_us: u64,
    pub observations: Vec<ActivityObservation>,
}

pub struct WaitSamples {
    samples: Vec<ActivitySample>,
    skipped_ticks: u64,
    limit: usize,
}

impl WaitSamples {
    pub fn new(limit: usize) -> Result<Self> {
        if !(1..=7000).contains(&limit) {
            return Err("invalid wait-sample capacity".into());
        }
        Ok(Self {
            samples: Vec::new(),
            skipped_ticks: 0,
            limit,
        })
    }
    pub fn record(&mut self, sample: ActivitySample) -> Result<()> {
        if sample.received_us < sample.sent_us || sample.observations.len() > 16 {
            return Err("invalid backend activity sample".into());
        }
        if self.samples.len() == self.limit {
            self.skipped_ticks += 1;
            return Ok(());
        }
        self.samples.push(sample);
        Ok(())
    }
    pub fn skip_ticks(&mut self, count: u64) {
        self.skipped_ticks += count;
    }

    /// Retain overlapping query-roundtrip intervals, with one sample interval
    /// of context. The exact instant of the server observation is unknown.
    pub fn report(&self, diagnostics: &Value, interval_ms: u64) -> Result<Value> {
        if interval_ms == 0 {
            return Err("sampling interval must be positive".into());
        }
        let margin = interval_ms
            .checked_mul(1000)
            .ok_or("sampling interval overflow")?;
        let mut windows = Vec::new();
        for category in ["slowest_dispatch", "slowest_request", "slowest_scheduled"] {
            let traces = diagnostics[category]
                .as_array()
                .ok_or("missing request traces")?;
            for trace in traces {
                let pid = trace["backend_pid"].as_i64().ok_or("missing trace PID")?;
                let start = trace["scheduled_us"]
                    .as_u64()
                    .ok_or("missing scheduled time")?
                    .saturating_sub(margin);
                let end = trace["completed_us"]
                    .as_u64()
                    .ok_or("missing completion time")?
                    .saturating_add(margin);
                windows.push((pid, start, end));
            }
        }
        let mut counts = BTreeMap::<String, u64>::new();
        let mut retained = Vec::new();
        let mut observations = 0u64;
        for sample in &self.samples {
            for row in &sample.observations {
                observations += 1;
                let key = format!(
                    "{}:{}:{}",
                    row.state.as_deref().unwrap_or("unknown"),
                    row.wait_type.as_deref().unwrap_or("none"),
                    row.wait_event.as_deref().unwrap_or("none")
                );
                *counts.entry(key).or_default() += 1;
                if windows.iter().any(|(pid, start, end)| {
                    *pid == i64::from(row.pid)
                        && sample.sent_us <= *end
                        && sample.received_us >= *start
                }) {
                    retained.push(json!({ "sent_us": sample.sent_us, "received_us": sample.received_us,
                        "pid": row.pid, "state": row.state, "wait_type": row.wait_type, "wait_event": row.wait_event }));
                }
            }
        }
        Ok(
            json!({ "interval_ms": interval_ms, "query_samples": self.samples.len(), "backend_observations": observations,
            "skipped_ticks_or_capacity_drops": self.skipped_ticks, "state_observation_counts": counts,
            "samples_near_retained_requests": retained,
            "scope": "point samples bounded by client query roundtrip; counts are not wait durations; observer adds load" }),
        )
    }
}
