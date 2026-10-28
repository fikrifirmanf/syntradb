//! Paced soak windows: per-window worker progress, storage, WAL, xmin horizon and
//! backend RSS, with explicit pass/fail limits. Correctness oracles stay in the
//! stress workers; this module only samples and judges resource behavior.
use postgres::Client;
use serde_json::{json, Map, Value};
use std::{
    process::Command,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};
use syntra_bench::Result;

const EVENT_BUDGET: i64 = 4096;
// Ceilings, not SLOs: earlier paced 30-second runs stayed far below each one.
const WAL_PER_COMMITTED_WRITE: u64 = 64 * 1024;
const SCHEDULED_P99_MS: f64 = 1000.0;
const MISSED_ARRIVAL_FRACTION: f64 = 0.05;
// After the retained snapshot ends, vacuum must stop catalog growth. The first
// full released window is the baseline; later growth is allowed 25% plus 16 MiB.
const CATALOG_GROWTH_FACTOR: f64 = 1.25;
const CATALOG_GROWTH_SLACK: i64 = 16 * 1024 * 1024;
// RSS includes shared buffers a backend has touched. Compare only released
// windows, after warm-up, so steady growth points at private memory.
const RSS_GROWTH_KIB: i64 = 64 * 1024;
const SAMPLE: &str = "SELECT json_build_object('database_bytes',pg_database_size(current_database()),'wal_dir_bytes',(SELECT coalesce(sum(size),0) FROM pg_ls_waldir()),'wal_lsn',pg_current_wal_insert_lsn()::text,'emitted',h.emitted,'pruned',h.pruned,'applied',c.applied,'generation',c.generation,'catalog_bytes',json_build_object('chunks',pg_total_relation_size('syntra.capture_chunks'),'manifests',pg_total_relation_size('syntra.capture_checkpoints'),'events',pg_total_relation_size('syntra.capture_events'),'heads',pg_total_relation_size('syntra.capture_heads'),'marks',pg_total_relation_size('syntra.capture_marks'),'source',pg_total_relation_size('checkpoint_stress_source')),'dead_tuples',(SELECT json_object_agg(relname,n_dead_tup) FROM pg_stat_all_tables WHERE relid IN('syntra.capture_chunks'::regclass,'syntra.capture_checkpoints'::regclass,'syntra.capture_events'::regclass,'syntra.capture_heads'::regclass,'syntra.capture_marks'::regclass,'checkpoint_stress_source'::regclass)),'horizon_age',syntra_cleanup_horizon_age(),'table_io',(SELECT json_object_agg(relname,json_build_object('seq_tup_read',seq_tup_read,'idx_scan',idx_scan,'idx_tup_fetch',idx_tup_fetch,'upd',n_tup_upd,'hot_upd',n_tup_hot_upd,'del',n_tup_del)) FROM pg_stat_all_tables WHERE schemaname IN('syntra','public') AND relname IN('capture_chunks','capture_checkpoints','capture_events','capture_heads','capture_marks','checkpoint_stress_source')),'waits',(SELECT coalesce(json_object_agg(w,n),'{}'::json) FROM (SELECT coalesce(wait_event_type||':'||wait_event,state) AS w,count(*) AS n FROM pg_stat_activity WHERE backend_type='client backend' AND pid<>pg_backend_pid() GROUP BY 1) AS q),'autovacuum_runs',(SELECT sum(autovacuum_count) FROM pg_stat_all_tables WHERE schemaname='syntra'),'oldest_xmin_age',(SELECT max(age(backend_xmin)) FROM pg_stat_activity WHERE backend_xmin IS NOT NULL AND pid<>pg_backend_pid())) FROM (SELECT hh.source_relation,hh.epoch,hh.base_rows,hh.pruned,coalesce(m.ordinal,hh.emitted) AS emitted,coalesce(m.live_rows,hh.live_rows) AS live_rows FROM syntra.capture_heads AS hh LEFT JOIN LATERAL (SELECT ordinal,live_rows FROM syntra.capture_marks WHERE epoch=hh.epoch AND ordinal>hh.emitted ORDER BY ordinal DESC LIMIT 1) AS m ON true) AS h JOIN syntra.capture_checkpoints AS c USING(source_relation,epoch) WHERE h.source_relation='checkpoint_stress_source'::regclass LIMIT 1";

/// Scheduled service of one writer or reader worker over the whole soak.
pub(super) struct Service {
    pub name: &'static str,
    pub scheduled_p99_ms: f64,
    pub planned: u64,
    pub missed: u64,
}

pub(super) struct Sampler<'a> {
    pub names: &'a [&'static str],
    pub pids: &'a [i32],
    pub progress: &'a [AtomicU64],
    pub start: Instant,
    pub window: Duration,
    pub windows: u64,
    pub disk_budget_bytes: u64,
    pub stop: &'a AtomicBool,
}

fn fail<T>(message: String) -> Result<T> {
    Err(message.into())
}

fn rss_kib(pids: &[i32]) -> Result<Vec<i64>> {
    let list = pids
        .iter()
        .map(i32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let output = Command::new("ps")
        .args(["-o", "pid=,rss=", "-p", &list])
        .output()?;
    let text = String::from_utf8(output.stdout)?;
    let mut found = vec![None; pids.len()];
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let (Some(pid), Some(rss)) = (fields.next(), fields.next()) else {
            continue;
        };
        let pid: i32 = pid.parse()?;
        if let Some(index) = pids.iter().position(|p| *p == pid) {
            found[index] = Some(rss.parse::<i64>()?);
        }
    }
    found
        .into_iter()
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| "soak worker backend exited before sampling".into())
}

fn catalog_bytes(sample: &Value) -> Option<i64> {
    let c = &sample["catalog_bytes"];
    Some(
        c["chunks"].as_i64()?
            + c["manifests"].as_i64()?
            + c["events"].as_i64()?
            + c["heads"].as_i64()?
            + c["marks"].as_i64()?,
    )
}

impl Sampler<'_> {
    /// Samples at each window boundary and calls `release` once, at the midpoint.
    /// Returns early, without error, when workers stop; the caller reports why.
    pub fn observe(
        &self,
        admin: &mut Client,
        windows: &mut Vec<Value>,
        mut release: impl FnMut() -> Result<()>,
    ) -> Result<()> {
        let mut previous = vec![0_u64; self.names.len()];
        let release_window = self.windows / 2;
        for index in 1..=self.windows {
            let target = self.start + self.window * u32::try_from(index)?;
            while Instant::now() < target {
                if self.stop.load(Ordering::Acquire) {
                    return Ok(());
                }
                thread::sleep(
                    target
                        .saturating_duration_since(Instant::now())
                        .min(Duration::from_millis(100)),
                );
            }
            let mut progress = Map::new();
            let mut stalled = Vec::new();
            for (n, name) in self.names.iter().enumerate() {
                let now = self.progress[n].load(Ordering::Acquire);
                if now == previous[n] {
                    stalled.push(*name);
                }
                progress.insert((*name).into(), json!(now - previous[n]));
                previous[n] = now;
            }
            let mut sample: Value = admin.query_one(SAMPLE, &[])?.try_get(0)?;
            let rss = rss_kib(self.pids)?;
            let emitted = sample["emitted"].as_i64().ok_or("missing emitted")?;
            let pruned = sample["pruned"].as_i64().ok_or("missing pruned")?;
            let applied = sample["applied"].as_i64().ok_or("missing applied")?;
            let disk = sample["database_bytes"].as_u64().ok_or("missing size")?
                + sample["wal_dir_bytes"].as_u64().ok_or("missing WAL size")?;
            sample["window"] = json!(index);
            sample["elapsed_seconds"] = json!(self.start.elapsed().as_secs_f64());
            sample["phase"] = json!(if index <= release_window {
                "retained"
            } else {
                "released"
            });
            sample["committed_progress"] = Value::Object(progress);
            sample["backend_rss_kib"] = json!(self
                .names
                .iter()
                .zip(&rss)
                .map(|(name, kib)| ((*name).to_owned(), json!(kib)))
                .collect::<Map<_, _>>());
            sample["pending_events"] = json!(emitted - applied);
            sample["retained_events"] = json!(emitted - pruned);
            eprintln!("Checkpoint soak window: {sample}");
            windows.push(sample);
            if !stalled.is_empty() {
                return fail(format!(
                    "window {index}: no committed progress for {stalled:?}"
                ));
            }
            if emitted - pruned > EVENT_BUDGET || emitted - applied > EVENT_BUDGET {
                return fail(format!("window {index}: capture feed exceeded its budget"));
            }
            if disk > self.disk_budget_bytes {
                return fail(format!(
                    "window {index}: database and WAL use {disk} bytes, over the {} byte budget",
                    self.disk_budget_bytes
                ));
            }
            if index == release_window {
                release()?;
            }
        }
        Ok(())
    }
}

/// Judges a complete soak. Every limit is reported with its measured value; any
/// violated limit is an error so the run cannot publish a passing report.
pub(super) fn evaluate(
    windows: &[Value],
    services: &[Service],
    wal_bytes: u64,
    committed_writes: u64,
) -> Result<Value> {
    let count = windows.len();
    if count < 4 || !count.is_multiple_of(2) {
        return fail(format!("soak recorded {count} windows"));
    }
    let release = count / 2;
    // windows[release] is the first window sampled entirely after the release.
    let (held, baseline, last) = (
        &windows[release - 1],
        &windows[release],
        &windows[count - 1],
    );
    let mut failures = Vec::new();

    let base_catalog = catalog_bytes(baseline).ok_or("missing catalog sample")?;
    let last_catalog = catalog_bytes(last).ok_or("missing catalog sample")?;
    let catalog_limit = (base_catalog as f64 * CATALOG_GROWTH_FACTOR) as i64 + CATALOG_GROWTH_SLACK;
    if last_catalog > catalog_limit {
        failures.push("catalog storage kept growing after the snapshot release");
    }

    let held_age = held["oldest_xmin_age"].as_i64().ok_or("missing xmin age")?;
    let last_age = last["oldest_xmin_age"].as_i64().unwrap_or(0);
    if last_age >= held_age {
        failures.push("xmin horizon did not advance after the snapshot release");
    }

    let mut rss_growth = Map::new();
    let base_rss = baseline["backend_rss_kib"]
        .as_object()
        .ok_or("missing RSS")?;
    for (name, kib) in last["backend_rss_kib"].as_object().ok_or("missing RSS")? {
        let growth = kib.as_i64().ok_or("bad RSS")?
            - base_rss
                .get(name)
                .and_then(Value::as_i64)
                .ok_or("missing RSS baseline")?;
        if growth > RSS_GROWTH_KIB {
            failures.push("backend RSS grew during the released phase");
        }
        rss_growth.insert(name.clone(), json!(growth));
    }

    let wal_per_write = wal_bytes / committed_writes.max(1);
    if committed_writes == 0 || wal_per_write > WAL_PER_COMMITTED_WRITE {
        failures.push("WAL per committed writer transaction exceeded its ceiling");
    }

    let mut service = Map::new();
    for worker in services {
        if worker.scheduled_p99_ms > SCHEDULED_P99_MS {
            failures.push("writer or reader scheduled p99 exceeded its ceiling");
        }
        if worker.missed as f64 > worker.planned as f64 * MISSED_ARRIVAL_FRACTION {
            failures.push("writer or reader missed too many offered arrivals");
        }
        service.insert(
            worker.name.into(),
            json!({"scheduled_p99_ms":worker.scheduled_p99_ms,"planned":worker.planned,"missed":worker.missed}),
        );
    }

    let peak_disk = windows
        .iter()
        .filter_map(|w| Some(w["database_bytes"].as_u64()? + w["wal_dir_bytes"].as_u64()?))
        .max();
    let verdict = json!({
        "windows": windows,
        "limits": {"event_budget": EVENT_BUDGET, "wal_per_committed_write_bytes": WAL_PER_COMMITTED_WRITE,
            "scheduled_p99_ms": SCHEDULED_P99_MS, "missed_arrival_fraction": MISSED_ARRIVAL_FRACTION,
            "catalog_growth_factor": CATALOG_GROWTH_FACTOR, "catalog_growth_slack_bytes": CATALOG_GROWTH_SLACK,
            "released_rss_growth_kib": RSS_GROWTH_KIB, "every_worker_commits_every_window": true},
        "measured": {"catalog_bytes_first_released_window": base_catalog, "catalog_bytes_final": last_catalog,
            "catalog_limit_bytes": catalog_limit, "oldest_xmin_age_before_release": held_age,
            "oldest_xmin_age_final": last_age, "released_rss_growth_kib": rss_growth,
            "wal_bytes": wal_bytes, "committed_writes": committed_writes, "wal_per_committed_write_bytes": wal_per_write,
            "service": service, "peak_database_and_wal_bytes": peak_disk},
        "failures": failures,
    });
    if !failures.is_empty() {
        eprintln!("Checkpoint soak verdict: {verdict}");
        return fail(format!("checkpoint soak limits failed: {failures:?}"));
    }
    Ok(verdict)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: i64 = 100 * 1024 * 1024;

    fn window(catalog: i64, xmin: i64, rss: i64) -> Value {
        json!({"catalog_bytes":{"chunks":catalog,"manifests":0,"events":0,"heads":0,"marks":0},
            "oldest_xmin_age":xmin,"backend_rss_kib":{"writer_0":rss},
            "database_bytes":1,"wal_dir_bytes":1})
    }
    // Two retained windows, then the released baseline and the final window.
    fn windows(last_catalog: i64, last_xmin: i64, last_rss: i64) -> Vec<Value> {
        vec![
            window(BASE / 2, 500, 1000),
            window(BASE, 900, 1000),
            window(BASE, 40, 2000),
            window(last_catalog, last_xmin, last_rss),
        ]
    }
    fn service(p99: f64, missed: u64) -> Vec<Service> {
        vec![Service {
            name: "writer_0",
            scheduled_p99_ms: p99,
            planned: 1000,
            missed,
        }]
    }
    fn failures(result: Result<Value>) -> String {
        result.err().map(|e| e.to_string()).unwrap_or_default()
    }
    const LIMIT: i64 = (BASE as f64 * CATALOG_GROWTH_FACTOR) as i64 + CATALOG_GROWTH_SLACK;

    #[test]
    fn soak_verdict_accepts_values_exactly_at_each_limit() {
        let verdict = evaluate(
            &windows(LIMIT, 899, 2000 + RSS_GROWTH_KIB),
            &service(SCHEDULED_P99_MS, 50),
            WAL_PER_COMMITTED_WRITE * 10,
            10,
        )
        .unwrap();
        assert_eq!(verdict["failures"], json!([]));
        assert_eq!(verdict["measured"]["catalog_limit_bytes"], json!(LIMIT));
        assert_eq!(
            verdict["measured"]["oldest_xmin_age_before_release"],
            json!(900)
        );
    }

    #[test]
    fn soak_verdict_rejects_each_limit_one_unit_over() {
        let ok = (
            LIMIT,
            899,
            2000,
            SCHEDULED_P99_MS,
            50,
            WAL_PER_COMMITTED_WRITE * 10,
        );
        let cases = [
            (
                (LIMIT + 1, ok.1, ok.2, ok.3, ok.4, ok.5),
                "catalog storage kept growing",
            ),
            (
                (ok.0, 900, ok.2, ok.3, ok.4, ok.5),
                "xmin horizon did not advance",
            ),
            (
                (ok.0, ok.1, 2001 + RSS_GROWTH_KIB, ok.3, ok.4, ok.5),
                "backend RSS grew",
            ),
            (
                (ok.0, ok.1, ok.2, SCHEDULED_P99_MS + 0.001, ok.4, ok.5),
                "scheduled p99",
            ),
            ((ok.0, ok.1, ok.2, ok.3, 51, ok.5), "missed too many"),
            (
                (ok.0, ok.1, ok.2, ok.3, ok.4, ok.5 + 10),
                "WAL per committed writer",
            ),
        ];
        for ((catalog, xmin, rss, p99, missed, wal), expected) in cases {
            let message = failures(evaluate(
                &windows(catalog, xmin, rss),
                &service(p99, missed),
                wal,
                10,
            ));
            assert!(message.contains(expected), "{expected}: {message}");
        }
    }

    #[test]
    fn soak_verdict_rejects_missing_writes_and_short_runs() {
        let message = failures(evaluate(&windows(LIMIT, 899, 2000), &service(1.0, 0), 0, 0));
        assert!(message.contains("WAL per committed writer"), "{message}");
        let short = &windows(LIMIT, 899, 2000)[..3];
        assert_eq!(
            failures(evaluate(short, &service(1.0, 0), 1, 1)),
            "soak recorded 3 windows"
        );
    }
}
