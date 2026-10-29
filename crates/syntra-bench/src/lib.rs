//! Input validation and result extraction for the isolated SQL benchmark runner.
#![forbid(unsafe_code)]

use serde_json::{json, Value};
use std::path::PathBuf;
mod diagnostics;
mod server_waits;
mod wal_usage;
pub use diagnostics::{DispatchDiagnostics, RequestTiming};
pub use server_waits::{ActivityObservation, ActivitySample, WaitSamples};
pub use wal_usage::WalUsage;

/// Checkpoint stress and soak feed layouts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckpointColumn {
    /// int4 key and value.
    Int4,
    /// int8 key and value.
    Int8,
    /// int4 key and numeric(18,2) value, stored as hundredths in 8 bytes.
    Numeric,
}

pub type Error = Box<dyn std::error::Error>;
pub type Result<T> = std::result::Result<T, Error>;

pub fn verify_source_fingerprints(extension: &str, runner: &str) -> Result<()> {
    let valid = |value: &str| {
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    };
    if !valid(extension) || !valid(runner) || extension != runner {
        return Err("extension and benchmark runner require matching source fingerprints; rebuild both from unchanged sources".into());
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
pub struct Options {
    pub pg_bin: PathBuf,
    pub output: PathBuf,
    pub rows: i32,
    pub repetitions: usize,
    pub require_production: bool,
    pub mixed_seconds: u64,
    pub mixed_rounds: usize,
    pub oltp_clients: usize,
    pub olap_clients: usize,
    pub offered_rate: u64,
    pub wait_sample_ms: u64,
    pub wal_init_zero: bool,
    /// Zero keeps PostgreSQL's default `shared_buffers`.
    pub shared_buffers_mb: u64,
    pub recovery_checks: bool,
    pub track_source: bool,
    pub trusted_stress_seconds: u64,
    pub checkpoint_stress_seconds: u64,
    pub checkpoint_worker_rate: u64,
    pub checkpoint_reader_replay: bool,
    pub checkpoint_combined_maintenance: bool,
    /// The feed layout the checkpoint stress and soak use.
    pub checkpoint_column: CheckpointColumn,
    pub checkpoint_soak_seconds: u64,
    pub checkpoint_soak_window_seconds: u64,
    pub checkpoint_soak_disk_budget_mb: u64,
    pub checkpoint_soak_skip: Vec<String>,
    pub refresh_cost_checks: bool,
    pub catalog_probe_checks: bool,
}

impl Options {
    pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Self> {
        let mut args = args.into_iter();
        let mut options = Self {
            pg_bin: PathBuf::new(),
            output: "benchmarks/results/latest.json".into(),
            rows: 100_000,
            repetitions: 7,
            require_production: false,
            mixed_seconds: 0,
            mixed_rounds: 3,
            oltp_clients: 4,
            olap_clients: 1,
            offered_rate: 1000,
            wait_sample_ms: 0,
            wal_init_zero: true,
            shared_buffers_mb: 0,
            recovery_checks: false,
            track_source: false,
            trusted_stress_seconds: 0,
            checkpoint_stress_seconds: 0,
            checkpoint_worker_rate: 0,
            checkpoint_reader_replay: true,
            checkpoint_combined_maintenance: false,
            checkpoint_column: CheckpointColumn::Int4,
            checkpoint_soak_seconds: 0,
            checkpoint_soak_window_seconds: 60,
            checkpoint_soak_disk_budget_mb: 2048,
            checkpoint_soak_skip: Vec::new(),
            refresh_cost_checks: false,
            catalog_probe_checks: false,
        };
        while let Some(flag) = args.next() {
            let value = args.next().ok_or("each option requires a value")?;
            match flag.as_str() {
                "--pg-bin" => options.pg_bin = value.into(),
                "--output" => options.output = value.into(),
                "--rows" => options.rows = value.parse()?,
                "--repetitions" => options.repetitions = value.parse()?,
                "--require-production" => options.require_production = value.parse()?,
                "--mixed-seconds" => options.mixed_seconds = value.parse()?,
                "--mixed-rounds" => options.mixed_rounds = value.parse()?,
                "--oltp-clients" => options.oltp_clients = value.parse()?,
                "--olap-clients" => options.olap_clients = value.parse()?,
                "--offered-rate" => options.offered_rate = value.parse()?,
                "--wait-sample-ms" => options.wait_sample_ms = value.parse()?,
                "--wal-init-zero" => options.wal_init_zero = value.parse()?,
                "--shared-buffers-mb" => options.shared_buffers_mb = value.parse()?,
                "--recovery-checks" => options.recovery_checks = value.parse()?,
                "--track-source" => options.track_source = value.parse()?,
                "--trusted-stress-seconds" => options.trusted_stress_seconds = value.parse()?,
                "--checkpoint-stress-seconds" => {
                    options.checkpoint_stress_seconds = value.parse()?
                }
                "--checkpoint-worker-rate" => options.checkpoint_worker_rate = value.parse()?,
                "--checkpoint-reader-replay" => options.checkpoint_reader_replay = value.parse()?,
                "--checkpoint-combined-maintenance" => {
                    options.checkpoint_combined_maintenance = value.parse()?
                }
                "--checkpoint-column-type" => {
                    options.checkpoint_column = match value.as_str() {
                        "int4" => CheckpointColumn::Int4,
                        "int8" => CheckpointColumn::Int8,
                        "numeric" => CheckpointColumn::Numeric,
                        _ => {
                            return Err(
                                "--checkpoint-column-type must be int4, int8 or numeric".into()
                            )
                        }
                    }
                }
                "--checkpoint-soak-seconds" => options.checkpoint_soak_seconds = value.parse()?,
                "--checkpoint-soak-window-seconds" => {
                    options.checkpoint_soak_window_seconds = value.parse()?
                }
                "--checkpoint-soak-disk-budget-mb" => {
                    options.checkpoint_soak_disk_budget_mb = value.parse()?
                }
                "--checkpoint-soak-skip" => {
                    options.checkpoint_soak_skip = value
                        .split(',')
                        .filter(|name| !name.is_empty())
                        .map(str::to_owned)
                        .collect()
                }
                "--refresh-cost-checks" => options.refresh_cost_checks = value.parse()?,
                "--catalog-probe-checks" => options.catalog_probe_checks = value.parse()?,
                _ => return Err("unknown benchmark option".into()),
            }
        }
        if options.pg_bin.as_os_str().is_empty() {
            return Err("--pg-bin is required".into());
        }
        if !(1..=1_000_000).contains(&options.rows) {
            return Err("rows must be between 1 and 1000000".into());
        }
        if !(3..=101).contains(&options.repetitions) {
            return Err("repetitions must be between 3 and 101".into());
        }
        if options.output.as_os_str().is_empty() {
            return Err("output path must not be empty".into());
        }
        if options.mixed_seconds != 0 && !(2..=30).contains(&options.mixed_seconds) {
            return Err("mixed seconds must be zero or between 2 and 30".into());
        }
        if !(1..=5).contains(&options.mixed_rounds)
            || !(1..=16).contains(&options.oltp_clients)
            || !(1..=4).contains(&options.olap_clients)
            || !(10..=100_000).contains(&options.offered_rate)
        {
            return Err("invalid mixed workload rounds, clients or offered rate".into());
        }
        if options.mixed_seconds != 0 && options.rows < 2 {
            return Err("mixed transactions require at least two accounts".into());
        }
        if options.shared_buffers_mb != 0 && !(16..=16384).contains(&options.shared_buffers_mb) {
            return Err("shared buffers must be zero or between 16 and 16384 MB".into());
        }
        if options.wait_sample_ms != 0 && !(5..=1000).contains(&options.wait_sample_ms) {
            return Err("wait sampling must be zero or between 5 and 1000 milliseconds".into());
        }
        if options.trusted_stress_seconds != 0
            && !(5..=300).contains(&options.trusted_stress_seconds)
        {
            return Err("trusted stress seconds must be zero or between 5 and 300".into());
        }
        if options.checkpoint_stress_seconds != 0
            && !(5..=300).contains(&options.checkpoint_stress_seconds)
        {
            return Err("checkpoint stress seconds must be zero or between 5 and 300".into());
        }
        if options.checkpoint_worker_rate != 0
            && (!(10..=1000).contains(&options.checkpoint_worker_rate)
                || (options.checkpoint_stress_seconds == 0 && options.checkpoint_soak_seconds == 0))
        {
            return Err(
                "checkpoint worker rate requires enabled stress and must be zero or 10..1000"
                    .into(),
            );
        }
        if !(10..=600).contains(&options.checkpoint_soak_window_seconds)
            || !(256..=65536).contains(&options.checkpoint_soak_disk_budget_mb)
        {
            return Err(
                "checkpoint soak windows must be 10..600 seconds and disk budget 256..65536 MB"
                    .into(),
            );
        }
        // Ablated soaks find which actor costs writers throughput. The feed must keep
        // draining, so writers, one applier and the pruner always run.
        if !options.checkpoint_soak_skip.is_empty()
            && (options.checkpoint_soak_seconds == 0
                || options.checkpoint_soak_skip.iter().any(|name| {
                    ![
                        "read_committed",
                        "repeatable_read",
                        "keyed_monitor",
                        "vacuum",
                        "applier_1",
                    ]
                    .contains(&name.as_str())
                }))
        {
            return Err("checkpoint soak skip needs a soak and only names read_committed, repeatable_read, keyed_monitor, vacuum or applier_1".into());
        }
        if options.checkpoint_soak_seconds != 0 {
            let windows = options.checkpoint_soak_seconds / options.checkpoint_soak_window_seconds;
            // Saturated writers can fill the disk; soaks must be paced. The retained
            // snapshot is released at the midpoint, which must be a window boundary.
            if !(240..=14400).contains(&options.checkpoint_soak_seconds)
                || options.checkpoint_stress_seconds != 0
                || options.checkpoint_worker_rate == 0
                || !options
                    .checkpoint_soak_seconds
                    .is_multiple_of(options.checkpoint_soak_window_seconds)
                || windows < 4
                || !windows.is_multiple_of(2)
            {
                return Err("checkpoint soak needs 240..14400 seconds, a worker rate, no stress run and an even number (at least four) of whole windows".into());
            }
        }
        Ok(options)
    }
}

pub fn production_guard(
    settings: &Value,
    extension_profile: &str,
    runner_release: bool,
) -> Result<()> {
    if settings["debug_assertions"] != "off"
        || extension_profile != "release"
        || !runner_release
        || settings["fsync"] != "on"
        || settings["synchronous_commit"] != "on"
        || settings["full_page_writes"] != "on"
    {
        return Err(
            "production baseline requires assertions off, release Rust and durability settings on"
                .into(),
        );
    }
    Ok(())
}

/// Constant-memory, three-significant-digit histogram. Times are microseconds;
/// zero-duration observations use the minimum one-microsecond bucket.
pub struct LatencyHistogram(hdrhistogram::Histogram<u64>);

impl LatencyHistogram {
    pub fn new() -> Result<Self> {
        Ok(Self(hdrhistogram::Histogram::new_with_bounds(
            1,
            120_000_000,
            3,
        )?))
    }
    pub fn record_us(&mut self, microseconds: u64) -> Result<()> {
        if microseconds > 120_000_000 {
            return Err("latency exceeded histogram bounds".into());
        }
        self.0.record(microseconds.max(1))?;
        Ok(())
    }
    pub fn merge(&mut self, other: &Self) -> Result<()> {
        self.0.add(&other.0)?;
        Ok(())
    }
    pub fn report(&self) -> Value {
        json!({ "count": self.0.len(), "precision_significant_digits": 3,
            "p50_ms": self.0.value_at_quantile(0.5) as f64 / 1000.0,
            "p95_ms": self.0.value_at_quantile(0.95) as f64 / 1000.0,
            "p99_ms": self.0.value_at_quantile(0.99) as f64 / 1000.0,
            "max_ms": self.0.max() as f64 / 1000.0,
            "buckets_us": self.0.iter_recorded().map(|v| [v.value_iterated_to(), v.count_at_value()]).collect::<Vec<_>>() })
    }
}

/// Offset for one aggregate arrival index. Each index belongs to exactly one
/// client via index % clients; pacing uses absolute times to avoid drift.
pub fn arrival_offset(index: u64, rate: u64) -> Result<std::time::Duration> {
    if rate == 0 || index > 3_000_000 {
        return Err("invalid arrival schedule".into());
    }
    Ok(std::time::Duration::from_nanos(
        index * 1_000_000_000 / rate,
    ))
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    pub execution_ms: f64,
    pub planning_ms: f64,
    pub shared_hit_blocks: u64,
    pub shared_read_blocks: u64,
}

impl Sample {
    pub fn from_explain(explain: &Value) -> Result<Self> {
        let root = explain
            .as_array()
            .and_then(|v| v.first())
            .ok_or("EXPLAIN must contain a root object")?;
        let execution_ms = root["Execution Time"]
            .as_f64()
            .filter(|n| n.is_finite() && *n >= 0.0)
            .ok_or("missing execution time")?;
        let planning_ms = root["Planning Time"]
            .as_f64()
            .filter(|n| n.is_finite() && *n >= 0.0)
            .ok_or("missing planning time")?;
        let plan = root
            .get("Plan")
            .filter(|p| p.is_object())
            .ok_or("missing plan")?;
        Ok(Self {
            execution_ms,
            planning_ms,
            shared_hit_blocks: plan["Shared Hit Blocks"]
                .as_u64()
                .ok_or("missing buffer hit count")?,
            shared_read_blocks: plan["Shared Read Blocks"]
                .as_u64()
                .ok_or("missing buffer read count")?,
        })
    }
}

/// Nearest-rank percentile: p95 of seven samples is the maximum, not an estimate
/// of a population tail. Keep raw samples alongside this descriptive statistic.
pub fn summarize_samples(samples: &[Sample]) -> Result<Value> {
    if samples.is_empty() {
        return Err("cannot summarize zero samples".into());
    }
    let mut times: Vec<_> = samples.iter().map(|s| s.execution_ms).collect();
    if times.iter().any(|n| !n.is_finite() || *n < 0.0) {
        return Err("invalid execution sample".into());
    }
    times.sort_by(f64::total_cmp);
    let median = if times.len().is_multiple_of(2) {
        (times[times.len() / 2 - 1] + times[times.len() / 2]) / 2.0
    } else {
        times[times.len() / 2]
    };
    let p95 = times[(times.len() * 95).div_ceil(100) - 1];
    Ok(
        json!({ "median_ms": median, "p95_ms": p95, "samples": samples.iter().map(|s| json!({
        "execution_ms": s.execution_ms, "planning_ms": s.planning_ms,
        "shared_hit_blocks": s.shared_hit_blocks, "shared_read_blocks": s.shared_read_blocks,
    })).collect::<Vec<_>>() }),
    )
}

pub type AggregateValues = (i64, i64, Option<i64>, Option<i32>, Option<i32>);

pub fn arrival_indices(
    worker: usize,
    clients: usize,
    offered: u64,
) -> Result<std::iter::StepBy<std::ops::Range<u64>>> {
    if clients == 0 || clients > 16 || worker >= clients || offered > 3_000_000 {
        return Err("invalid arrival partition".into());
    }
    Ok((worker as u64..offered).step_by(clients))
}

/// Compare only complete observations at identical offered load. Keep rounds
/// separate rather than averaging percentiles or hiding incomplete phases.
pub fn mixed_comparisons(reports: &[Value]) -> Result<Value> {
    let mut pairs = vec![
        ("postgresql_oltp_only", "enabled_oltp_only"),
        ("postgresql_oltp_only", "postgresql_fresh_heap"),
        ("enabled_oltp_only", "enabled_fresh_heap"),
        ("enabled_oltp_only", "enabled_frozen_projection"),
        ("enabled_oltp_only", "enabled_fresh_capture"),
    ];
    // Older six-phase reports remain readable; new reports require the extra
    // fresh Rust phase in every round once that phase appears anywhere.
    if reports
        .iter()
        .any(|r| r["phase"] == "enabled_fresh_rust_heap")
    {
        pairs.push(("postgresql_fresh_heap", "enabled_fresh_rust_heap"));
    }
    let rounds = reports
        .iter()
        .map(|r| r["round"].as_u64().ok_or("missing mixed round"))
        .collect::<std::result::Result<std::collections::BTreeSet<_>, _>>()?;
    let mut comparisons = Vec::new();
    for round in rounds {
        for &(base, loaded) in &pairs {
            let find = |name| {
                let mut matches = reports
                    .iter()
                    .filter(|r| r["round"] == round && r["phase"] == name);
                let report = matches.next().ok_or("incomplete mixed round")?;
                if matches.next().is_some() {
                    return Err("duplicate mixed phase");
                }
                Ok(report)
            };
            let baseline = find(base)?;
            let concurrent = find(loaded)?;
            for report in [baseline, concurrent] {
                for key in ["offered_transactions", "window_seconds", "oltp_clients"] {
                    if report[key].as_u64().is_none_or(|n| n == 0) {
                        return Err("invalid offered-load metadata".into());
                    }
                }
            }
            let comparable = baseline["latency_complete"] == true
                && concurrent["latency_complete"] == true
                && baseline["offered_transactions"] == concurrent["offered_transactions"]
                && baseline["window_seconds"] == concurrent["window_seconds"]
                && baseline["oltp_clients"] == concurrent["oltp_clients"];
            let ratio = if comparable {
                let first = baseline["scheduled_to_commit"]["p99_ms"]
                    .as_f64()
                    .filter(|n| *n > 0.0)
                    .ok_or("invalid baseline p99")?;
                let second = concurrent["scheduled_to_commit"]["p99_ms"]
                    .as_f64()
                    .filter(|n| *n > 0.0)
                    .ok_or("invalid concurrent p99")?;
                Some(second / first)
            } else {
                None
            };
            comparisons.push(
                json!({ "round": round, "baseline": base, "concurrent": loaded,
                "p99_ratio": ratio, "complete_equal_offered_load": comparable,
                "frozen_projection_not_fresh": loaded == "enabled_frozen_projection" }),
            );
        }
    }
    Ok(Value::Array(comparisons))
}

pub fn correctness_values(row: &postgres::Row) -> Result<AggregateValues> {
    Ok((
        row.try_get(0)?,
        row.try_get(1)?,
        row.try_get(2)?,
        row.try_get(3)?,
        row.try_get(4)?,
    ))
}

/// Select the latest due slot, dropping older arrivals instead of issuing a burst.
/// `total` is the finite offered count; returning it means the schedule has ended.
pub fn checkpoint_due_slot(
    next: u64,
    elapsed: std::time::Duration,
    rate: u64,
    total: u64,
) -> Result<u64> {
    if !(10..=1000).contains(&rate) || total > 300_000 || next > total {
        return Err("invalid checkpoint arrival schedule".into());
    }
    let due = elapsed.as_nanos().saturating_mul(u128::from(rate)) / 1_000_000_000;
    Ok(next
        .max(u64::try_from(due.min(u128::from(total)))?)
        .min(total))
}
