//! Paced transactions plus analytical clients on one disposable PostgreSQL node.
//! This is a transfer microbenchmark, not a TPC-C/CH-benCHmark implementation.
use crate::Cluster;
use postgres::{Client, Statement};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{Arc, Condvar, Mutex},
    thread,
    time::{Duration, Instant},
};
use syntra_bench::{
    arrival_indices, arrival_offset, DispatchDiagnostics, LatencyHistogram, Options, RequestTiming,
    Result,
};

const TRANSFER: &str = "UPDATE mixed_accounts SET balance = balance + CASE WHEN id = $1 THEN -1 ELSE 1 END WHERE id IN ($1, $2)";
const FRESH_HEAP: &str =
    "SELECT count(*), sum(balance), min(balance), max(balance) FROM mixed_accounts";
const FIXED: &str = "SELECT s.rows, s.sum, s.min, s.max FROM mixed_projection AS p, LATERAL syntra_segment_summary(p.segment) AS s WHERE p.name = 'fixed' LIMIT 1";
const FRESH_CAPTURE: &str = "SELECT s.rows, s.sum, s.min, s.max FROM syntra_capture_int4('mixed_accounts'::regclass, 'balance') AS c, LATERAL syntra_segment_summary(c.segment) AS s LIMIT 1";

#[derive(Clone, Copy)]
enum Analytics {
    None,
    Heap,
    Fixed,
    Capture,
    RustHeap,
}

#[derive(Clone, Copy)]
struct Phase {
    name: &'static str,
    enabled: bool,
    analytics: Analytics,
}

const PHASES: [Phase; 7] = [
    Phase {
        name: "postgresql_oltp_only",
        enabled: false,
        analytics: Analytics::None,
    },
    Phase {
        name: "enabled_oltp_only",
        enabled: true,
        analytics: Analytics::None,
    },
    Phase {
        name: "postgresql_fresh_heap",
        enabled: false,
        analytics: Analytics::Heap,
    },
    Phase {
        name: "enabled_fresh_heap",
        enabled: true,
        analytics: Analytics::Heap,
    },
    Phase {
        name: "enabled_frozen_projection",
        enabled: true,
        analytics: Analytics::Fixed,
    },
    Phase {
        name: "enabled_fresh_capture",
        enabled: true,
        analytics: Analytics::Capture,
    },
    Phase {
        name: "enabled_fresh_rust_heap",
        enabled: true,
        analytics: Analytics::RustHeap,
    },
];

struct GateState {
    start: Option<Instant>,
    cancelled: bool,
}
pub(super) struct Gate {
    state: Mutex<GateState>,
    changed: Condvar,
}

impl Gate {
    fn new() -> Self {
        Self {
            state: Mutex::new(GateState {
                start: None,
                cancelled: false,
            }),
            changed: Condvar::new(),
        }
    }
    pub(super) fn wait(&self) -> Result<Option<Instant>> {
        let mut state = self.state.lock().map_err(|_| "start gate poisoned")?;
        while state.start.is_none() && !state.cancelled {
            state = self
                .changed
                .wait(state)
                .map_err(|_| "start gate poisoned")?;
        }
        Ok(if state.cancelled { None } else { state.start })
    }
    fn open(&self, start: Instant) -> Result<()> {
        self.state.lock().map_err(|_| "start gate poisoned")?.start = Some(start);
        self.changed.notify_all();
        Ok(())
    }
    fn cancel(&self) {
        // Wake even a poisoned gate during startup cleanup; no work is published.
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.cancelled = true;
        self.changed.notify_all();
    }
}

struct Metrics {
    attempted: u64,
    succeeded: u64,
    completed_in_window: u64,
    errors: BTreeMap<String, u64>,
    service: LatencyHistogram,
    scheduled: LatencyHistogram,
    dispatch: Option<DispatchDiagnostics>,
}

impl Metrics {
    fn new(oltp: bool) -> Result<Self> {
        Ok(Self {
            attempted: 0,
            succeeded: 0,
            completed_in_window: 0,
            errors: BTreeMap::new(),
            service: LatencyHistogram::new()?,
            scheduled: LatencyHistogram::new()?,
            dispatch: if oltp {
                Some(DispatchDiagnostics::new(16)?)
            } else {
                None
            },
        })
    }
    fn error(&mut self, error: postgres::Error) {
        let code = error
            .as_db_error()
            .map(|e| e.code().code())
            .unwrap_or("client_error")
            .to_owned();
        *self.errors.entry(code).or_default() += 1;
    }
    fn merge(&mut self, other: &Self) -> Result<()> {
        self.attempted += other.attempted;
        self.succeeded += other.succeeded;
        self.completed_in_window += other.completed_in_window;
        for (code, count) in &other.errors {
            *self.errors.entry(code.clone()).or_default() += count;
        }
        self.service.merge(&other.service)?;
        self.scheduled.merge(&other.scheduled)?;
        if let (Some(target), Some(source)) = (&mut self.dispatch, &other.dispatch) {
            target.merge(source)?;
        }
        Ok(())
    }
}

enum Job {
    Oltp {
        client: Client,
        statement: Statement,
        worker: usize,
        backend_pid: i32,
    },
    Olap {
        client: Client,
        statement: Statement,
    },
}

struct WorkerReport {
    oltp: bool,
    metrics: Metrics,
}

#[derive(Clone, Copy)]
struct Workload {
    seconds: u64,
    rate: u64,
    clients: usize,
    rows: i32,
}

fn micros(duration: Duration) -> u64 {
    duration.as_micros().min(u128::from(u64::MAX)) as u64
}

fn run_job(job: Job, gate: &Gate, work: Workload) -> Result<WorkerReport> {
    let mut metrics = Metrics::new(matches!(&job, Job::Oltp { .. }))?;
    let start = gate.wait()?.ok_or("mixed workload startup cancelled")?;
    let end = start + Duration::from_secs(work.seconds);
    match job {
        Job::Oltp {
            mut client,
            statement,
            worker,
            backend_pid,
        } => {
            let offered = work.rate * work.seconds;
            let pairs = u64::try_from(work.rows / 2)?;
            let mut previous_completed_us = None;
            // unslop-ignore N_PLUS_ONE_IN_LOOP -- Each iteration is an independently paced benchmark transaction; batching changes the workload.
            for index in arrival_indices(worker, work.clients, offered)? {
                let ready = Instant::now();
                let due = start + arrival_offset(index, work.rate)?;
                if let Some(delay) = due.checked_duration_since(Instant::now()) {
                    thread::sleep(delay);
                }
                if Instant::now() >= end {
                    break;
                }
                let pair = index % pairs;
                let first = i32::try_from(pair * 2 + 1)?;
                let second = first + 1;
                let began = Instant::now();
                metrics.attempted += 1;
                let result = client.execute(&statement, &[&first, &second]);
                let completed = Instant::now();
                let timing = RequestTiming {
                    worker,
                    arrival_index: index,
                    backend_pid,
                    scheduled_us: micros(due.saturating_duration_since(start)),
                    ready_us: micros(ready.saturating_duration_since(start)),
                    dispatched_us: micros(began.saturating_duration_since(start)),
                    completed_us: micros(completed.saturating_duration_since(start)),
                    previous_completed_us,
                    sqlstate: result.as_ref().err().map(|error| {
                        error
                            .as_db_error()
                            .map(|e| e.code().code())
                            .unwrap_or("client_error")
                            .to_owned()
                    }),
                };
                previous_completed_us = Some(timing.completed_us);
                if let Some(diagnostics) = &mut metrics.dispatch {
                    diagnostics.record(timing)?;
                }
                match result {
                    Ok(2) => {
                        metrics.succeeded += 1;
                        metrics.completed_in_window += u64::from(completed <= end);
                        metrics
                            .service
                            .record_us(micros(completed.duration_since(began)))?;
                        metrics
                            .scheduled
                            .record_us(micros(completed.duration_since(due)))?;
                    }
                    // unslop-ignore NO_SQL_INTERPOLATION -- This is an error message; the prepared UPDATE binds both account IDs.
                    Ok(_) => return Err("a transfer did not update exactly two accounts".into()),
                    Err(error) => metrics.error(error),
                }
            }
            Ok(WorkerReport {
                oltp: true,
                metrics,
            })
        }
        Job::Olap {
            mut client,
            statement,
        } => {
            if let Some(delay) = start.checked_duration_since(Instant::now()) {
                thread::sleep(delay);
            }
            while Instant::now() < end {
                let began = Instant::now();
                metrics.attempted += 1;
                match client.query_one(&statement, &[]) {
                    Ok(row) => {
                        let count: i64 = row.try_get(0)?;
                        let sum: Option<i64> = row.try_get(1)?;
                        if count != i64::from(work.rows) || sum != Some(i64::from(work.rows) * 1000)
                        {
                            return Err(
                                "analytical read violated the transfer count/sum invariant".into(),
                            );
                        }
                        let completed = Instant::now();
                        metrics.succeeded += 1;
                        metrics.completed_in_window += u64::from(completed <= end);
                        metrics
                            .service
                            .record_us(micros(completed.duration_since(began)))?;
                    }
                    Err(error) => metrics.error(error),
                }
            }
            Ok(WorkerReport {
                oltp: false,
                metrics,
            })
        }
    }
}

fn analytics_sql(phase: Phase) -> Option<&'static str> {
    match phase.analytics {
        Analytics::None => None,
        Analytics::Heap => Some(FRESH_HEAP),
        Analytics::Fixed => Some(FIXED),
        Analytics::Capture => Some(FRESH_CAPTURE),
        Analytics::RustHeap => Some("SELECT rows, sum, min, max FROM syntra_heap_int4_summary('mixed_accounts','balance') LIMIT 1"),
    }
}

fn worker_client(
    cluster: &Cluster,
    database: &str,
    enabled: bool,
    analytical: bool,
) -> Result<Client> {
    let mut client = cluster.connect(database)?;
    if analytical {
        client.batch_execute("SET statement_timeout = '5s'; SET lock_timeout = '1s'")?;
    } else {
        client.batch_execute("SET statement_timeout = '2s'; SET lock_timeout = '1s'")?;
    }
    if enabled {
        client.query_one("SELECT syntra_version()", &[])?;
    }
    Ok(client)
}

fn run_phase(cluster: &Cluster, phase: Phase, options: &Options, round: usize) -> Result<Value> {
    let database = if phase.enabled {
        "syntra_enabled"
    } else {
        "syntra_plain"
    };
    let mut admin = cluster.connect(database)?;
    let setup = Instant::now();
    admin.batch_execute("DROP TABLE IF EXISTS mixed_projection; DROP TABLE IF EXISTS mixed_accounts; CREATE TABLE mixed_accounts (id int4 PRIMARY KEY, balance int4 NOT NULL)")?;
    admin.execute("INSERT INTO mixed_accounts (id, balance) SELECT n, 1000 FROM generate_series(1, $1::int4) AS n", &[&options.rows])?;
    admin.batch_execute("ANALYZE mixed_accounts")?;
    let tracked = options.track_source && phase.enabled;
    if tracked {
        admin.batch_execute("SELECT syntra_track_source('mixed_accounts', 'balance')")?;
    }
    if matches!(phase.analytics, Analytics::Fixed) {
        admin.batch_execute("CREATE TABLE mixed_projection AS SELECT 'fixed'::text AS name, segment FROM syntra_capture_int4('mixed_accounts'::regclass, 'balance'); ALTER TABLE mixed_projection ADD PRIMARY KEY (name); ANALYZE mixed_projection")?;
    }
    let setup_ms = setup.elapsed().as_secs_f64() * 1000.0;
    let mut jobs = Vec::new();
    let mut backend_pids = Vec::new();
    for worker in 0..options.oltp_clients {
        let mut client = worker_client(cluster, database, phase.enabled, false)?;
        let statement = client.prepare(TRANSFER)?;
        let backend_pid: i32 = client
            .query_one("SELECT pg_backend_pid()", &[])?
            .try_get(0)?;
        backend_pids.push(backend_pid);
        jobs.push(Job::Oltp {
            client,
            statement,
            worker,
            backend_pid,
        });
    }
    if let Some(sql) = analytics_sql(phase) {
        // unslop-ignore N_PLUS_ONE_IN_LOOP -- Prepare and warm each configured benchmark connection once, not once per source record.
        for _ in 0..options.olap_clients {
            let mut client = worker_client(cluster, database, phase.enabled, true)?;
            let statement = client.prepare(sql)?;
            let row = client.query_one(&statement, &[])?;
            if row.try_get::<_, i64>(0)? != i64::from(options.rows)
                || row.try_get::<_, Option<i64>>(1)? != Some(i64::from(options.rows) * 1000)
            {
                return Err("mixed analytical warmup failed correctness validation".into());
            }
            jobs.push(Job::Olap { client, statement });
        }
    }
    let wal_start: String = admin
        .query_one("SELECT pg_current_wal_insert_lsn()::text", &[])?
        .try_get(0)?;
    let gate = Arc::new(Gate::new());
    let mut oltp = Metrics::new(true)?;
    let mut olap = Metrics::new(false)?;
    let work = Workload {
        seconds: options.mixed_seconds,
        rate: options.offered_rate,
        clients: options.oltp_clients,
        rows: options.rows,
    };
    let mut handles = Vec::new();
    let sampler = if options.wait_sample_ms > 0 {
        Some(crate::wait_sampler::WaitSampler::start(
            cluster.connect(database)?,
            backend_pids,
            options.wait_sample_ms,
            gate.clone(),
        )?)
    } else {
        None
    };
    for job in jobs {
        let start_gate = gate.clone();
        match thread::Builder::new()
            .spawn(move || run_job(job, &start_gate, work).map_err(|error| error.to_string()))
        {
            Ok(handle) => handles.push(handle),
            Err(error) => {
                gate.cancel();
                for handle in handles {
                    let _ = handle.join();
                }
                return Err(error.into());
            }
        }
    }
    let start = Instant::now() + Duration::from_millis(50);
    if let Err(error) = gate.open(start) {
        gate.cancel();
        for handle in handles {
            let _ = handle.join();
        }
        return Err(error);
    }
    let mut failure: Option<String> = None;
    for handle in handles {
        match handle.join() {
            Ok(Ok(report)) => {
                let merged = if report.oltp {
                    oltp.merge(&report.metrics)
                } else {
                    olap.merge(&report.metrics)
                };
                if let Err(error) = merged {
                    failure = Some(error.to_string());
                }
            }
            Ok(Err(error)) => failure = Some(error),
            Err(_) => failure = Some("mixed workload worker panicked".to_owned()),
        }
    }
    let elapsed = start.elapsed().as_secs_f64();
    let waits = sampler
        .map(crate::wait_sampler::WaitSampler::finish)
        .transpose()?;
    if let Some(error) = failure {
        return Err(error.into());
    }
    if oltp.succeeded == 0 || (analytics_sql(phase).is_some() && olap.succeeded == 0) {
        return Err(
            "mixed workload produced no successful transactions or required analytics".into(),
        );
    }
    let final_row = admin.query_one("SELECT count(*), sum(balance), min(balance), max(balance), count(*) FILTER (WHERE balance <> 1000) FROM mixed_accounts", &[])?;
    if final_row.try_get::<_, i64>(0)? != i64::from(options.rows)
        || final_row.try_get::<_, Option<i64>>(1)? != Some(i64::from(options.rows) * 1000)
    {
        return Err("final heap violated the transfer invariant".into());
    }
    let changed: i64 = final_row.try_get(4)?;
    if changed == 0 {
        return Err("no account values changed during the mixed workload".into());
    }
    let wal_bytes: f64 = admin
        .query_one(
            "SELECT pg_wal_lsn_diff(pg_current_wal_insert_lsn(), $1::text::pg_lsn)::float8",
            &[&wal_start],
        )?
        .try_get(0)?;
    let offered = options.offered_rate * options.mixed_seconds;
    let frozen = matches!(phase.analytics, Analytics::Fixed);
    let diagnostics = oltp
        .dispatch
        .as_ref()
        .map(DispatchDiagnostics::report)
        .ok_or("missing OLTP diagnostics")?;
    let server_waits = waits
        .map(|samples| samples.report(&diagnostics, options.wait_sample_ms))
        .transpose()?;
    let tracking_generation = if tracked {
        let generation: i64 = admin.query_one("SELECT generation FROM syntra_source_tracking_status('mixed_accounts') WHERE guard_config_valid LIMIT 1", &[])?.try_get(0)?;
        if generation < 0 || generation as u64 != oltp.succeeded {
            return Err("source counter does not match committed benchmark mutations".into());
        }
        Some(generation)
    } else {
        None
    };
    Ok(
        json!({ "round": round, "phase": phase.name, "window_seconds": options.mixed_seconds,
        "elapsed_with_drain_seconds": elapsed, "setup_ms": setup_ms,
        "source_tracking": tracked, "source_tracking_generation": tracking_generation,
        "oltp_clients": options.oltp_clients, "olap_clients": if analytics_sql(phase).is_some() { options.olap_clients } else { 0 },
        "offered_transactions": offered, "offered_transactions_per_second": options.offered_rate,
        "attempted_transactions": oltp.attempted, "missed_arrivals": offered - oltp.attempted,
        "successful_transactions": oltp.succeeded, "completed_transactions_in_window": oltp.completed_in_window,
        "committed_tps_in_window": oltp.completed_in_window as f64 / options.mixed_seconds as f64,
        "errors_by_sqlstate": oltp.errors, "latency_complete": offered == oltp.succeeded,
        "scheduled_to_commit": oltp.scheduled.report(), "request_to_commit": oltp.service.report(),
        "dispatch_diagnostics": diagnostics, "server_wait_samples": server_waits,
        "analytical_queries": olap.succeeded, "analytical_qps_in_window": olap.completed_in_window as f64 / options.mixed_seconds as f64,
        "analytical_latency": olap.service.report(), "analytical_errors_by_sqlstate": olap.errors,
        "analytical_sql": analytics_sql(phase), "transfer_sql": TRANSFER,
        "freshness": match phase.analytics { Analytics::Fixed => "frozen before writes; not a fresh HTAP read",
            Analytics::Capture => "statement snapshot; heap scan plus rebuild and segment validation on each read",
            Analytics::RustHeap => "statement snapshot; Rust aggregate over native heap; no segment build",
            Analytics::Heap => "statement snapshot; native heap read", Analytics::None => "no analytical client" },
        "final_heap_min": final_row.try_get::<_, Option<i32>>(2)?, "final_heap_max": final_row.try_get::<_, Option<i32>>(3)?,
        "changed_accounts": changed, "fixed_projection_is_stale": frozen && changed > 0,
        "cluster_wal_bytes_including_background": wal_bytes, "transfer_invariant": "matched" }),
    )
}

pub(super) fn run(cluster: &Cluster, options: &Options) -> Result<Value> {
    let mut reports = Vec::new();
    for round in 0..options.mixed_rounds {
        for step in 0..PHASES.len() {
            let phase = PHASES[(round + step) % PHASES.len()];
            eprintln!(
                "Mixed round {}: {}, {} offered tx/s for {}s",
                round + 1,
                phase.name,
                options.offered_rate,
                options.mixed_seconds
            );
            reports.push(run_phase(cluster, phase, options, round + 1)?);
        }
    }
    let comparisons = syntra_bench::mixed_comparisons(&reports)?;
    Ok(
        json!({ "workload": "paced, prepared, atomic two-account transfers; one host for clients and server",
        "source_tracking_requested": options.track_source,
        "enabled_transaction_scope": if options.track_source { "source tracker attached; includes trigger and counter contention" } else { "no source tracker attached" },
        "scheduling": "absolute aggregate arrival schedule, partitioned by client; missed arrivals and errors are explicit",
        "latency_scope": "successful requests including drain; scheduled-to-commit includes queueing; percentile completeness flag requires every offered request to succeed",
        "warmup": "tables freshly loaded; one analytical query per analytical client before measurement; no timed OLTP warmup",
        "phase_order": "seven phases rotated deterministically per round", "reports": reports, "comparisons": comparisons }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_cancellation_releases_a_worker_without_dispatching_work() {
        let gate = Arc::new(Gate::new());
        let waiting = gate.clone();
        let (send, receive) = std::sync::mpsc::channel();
        let handle = thread::spawn(move || send.send(waiting.wait().unwrap().is_none()).unwrap());
        gate.cancel();
        assert!(receive.recv_timeout(Duration::from_secs(2)).unwrap());
        handle.join().unwrap();
    }

    #[test]
    fn every_waiter_receives_the_same_start_instant() {
        let gate = Arc::new(Gate::new());
        let (send, receive) = std::sync::mpsc::channel();
        let mut handles = Vec::new();
        for _ in 0..3 {
            let waiting = gate.clone();
            let send = send.clone();
            handles.push(thread::spawn(move || {
                send.send(waiting.wait().unwrap()).unwrap()
            }));
        }
        let start = Instant::now();
        gate.open(start).unwrap();
        for _ in 0..3 {
            assert_eq!(
                receive.recv_timeout(Duration::from_secs(2)).unwrap(),
                Some(start)
            );
        }
        for handle in handles {
            handle.join().unwrap();
        }
    }
}
