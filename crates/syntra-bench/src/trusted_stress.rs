//! Bounded, saturated correctness stress; no offered-load or speedup claim.
use crate::Cluster;
use postgres::{Client, Statement};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};
use syntra_bench::{correctness_values, AggregateValues, LatencyHistogram, Options, Result};

const READ_ALL: &str = r#"SELECT s.rows,s.non_nulls,s.sum,s.min,s.max,s.route,s.reason,
    ROW(s.rows,s.non_nulls,s.sum,s.min,s.max) IS NOT DISTINCT FROM
    ROW(n.rows,n.non_nulls,n.sum,n.min,n.max), pg_current_xact_id_if_assigned() IS NULL
    FROM syntra_live_int4_summary('trusted_stress_source','v') AS s,
    (SELECT count(*) AS rows,count(v) AS non_nulls,sum(v),min(v),max(v)
     FROM trusted_stress_source) AS n LIMIT 1"#;
const READ_RANGE: &str = r#"SELECT s.rows,s.non_nulls,s.sum,s.min,s.max,s.route,s.reason,
    ROW(s.rows,s.non_nulls,s.sum,s.min,s.max) IS NOT DISTINCT FROM
    ROW(n.rows,n.non_nulls,n.sum,n.min,n.max), pg_current_xact_id_if_assigned() IS NULL
    FROM syntra_live_int4_range_summary('trusted_stress_source','v',0,1000000) AS s,
    (SELECT count(*) AS rows,count(v) AS non_nulls,sum(v),min(v),max(v)
     FROM trusted_stress_source WHERE v BETWEEN 0 AND 1000000) AS n LIMIT 1"#;
const PUBLISH: &str = "SELECT syntra_publish_trusted_int4('trusted_stress_source','v')";
const TRANSFER: &str =
    "UPDATE trusted_stress_source SET v=v+CASE WHEN id=$1 THEN -1 ELSE 1 END WHERE id IN($1,$2)";

struct Stats {
    name: &'static str,
    cycles: u64,
    commits: u64,
    rollbacks: u64,
    lock_conflicts: u64,
    routes: BTreeMap<String, u64>,
    maintenance: BTreeMap<&'static str, u64>,
    latency: LatencyHistogram,
    start_skew_ms: f64,
}

impl Stats {
    fn new(name: &'static str, start: Instant) -> Result<Self> {
        Ok(Self {
            name,
            cycles: 0,
            commits: 0,
            rollbacks: 0,
            lock_conflicts: 0,
            routes: BTreeMap::new(),
            maintenance: BTreeMap::new(),
            latency: LatencyHistogram::new()?,
            start_skew_ms: start.elapsed().as_secs_f64() * 1000.0,
        })
    }
    fn finish(&mut self, start: Instant) -> Result<()> {
        self.cycles += 1;
        self.latency
            .record_us(u64::try_from(start.elapsed().as_micros())?)
    }
    fn report(&self) -> Value {
        json!({"name":self.name,"cycles":self.cycles,"commits":self.commits,
            "rollbacks":self.rollbacks,"expected_lock_conflicts":self.lock_conflicts,
            "read_routes_and_reasons":self.routes,"cycle_latency":self.latency.report(),
            "maintenance_successes":self.maintenance,
            "start_skew_ms":self.start_skew_ms})
    }
}

fn read(
    client: &mut Client,
    statement: &Statement,
    stats: &mut Stats,
    expected_xid: bool,
    expected_sum: Option<i64>,
) -> Result<AggregateValues> {
    let row = client.query_one(statement, &[])?;
    let values = correctness_values(&row)?;
    let route: &str = row.try_get(5)?;
    let reason: &str = row.try_get(6)?;
    if !row.try_get::<_, bool>(7)?
        || row.try_get::<_, bool>(8)? == expected_xid
        || expected_sum.is_some_and(|sum| values.2 != Some(sum))
    {
        return Err("trusted stress aggregate, snapshot or transaction invariant failed".into());
    }
    if !matches!(route, "heap" | "projection")
        || (expected_xid && (route != "heap" || reason != "transaction_has_xid"))
    {
        return Err("trusted stress returned an invalid execution route".into());
    }
    *stats.routes.entry(format!("{route}/{reason}")).or_default() += 1;
    Ok(values)
}

#[derive(Clone, Copy)]
enum Actor {
    Writer(i32),
    Reader(bool),
    Publisher,
    Maintenance,
}

struct Worker {
    client: Client,
    actor: Actor,
    all: Statement,
    range: Statement,
    transfer: Statement,
    publish: Statement,
    name: &'static str,
}

fn expected_conflict(error: &postgres::Error) -> bool {
    error.code().is_some_and(|code| code.code() == "55P03")
}

fn work(
    mut worker: Worker,
    start: Instant,
    deadline: Instant,
    stop: &AtomicBool,
    rows: i32,
    total: i64,
) -> Result<Stats> {
    let mut stats = Stats::new(worker.name, start)?;
    // unslop-ignore N_PLUS_ONE_IN_LOOP -- Each cycle is an independent concurrent database operation under stress, not a per-record application lookup.
    while Instant::now() < deadline && !stop.load(Ordering::Acquire) {
        let cycle = Instant::now();
        match worker.actor {
            Actor::Writer(index) => {
                worker.client.batch_execute("BEGIN")?;
                let left = 2 * index + 1;
                let right = left + 1;
                if worker.client.execute(&worker.transfer, &[&left, &right])? != 2 {
                    return Err("stress transfer changed an unexpected number of accounts".into());
                }
                let transient = rows + index + 1;
                if stats.cycles % 2 == 0 {
                    worker.client.execute("INSERT INTO trusted_stress_source(id,v) VALUES($1,NULL) ON CONFLICT(id) DO UPDATE SET v=EXCLUDED.v",&[&transient])?;
                } else {
                    worker.client.execute(
                        "DELETE FROM trusted_stress_source WHERE id=$1",
                        &[&transient],
                    )?;
                }
                if stats.cycles % 31 == 0 {
                    read(
                        &mut worker.client,
                        &worker.all,
                        &mut stats,
                        true,
                        Some(total),
                    )?;
                }
                if stats.cycles % 11 == 0 {
                    worker.client.batch_execute("ROLLBACK")?;
                    stats.rollbacks += 1;
                } else {
                    worker.client.batch_execute("COMMIT")?;
                    stats.commits += 1;
                }
            }
            Actor::Reader(repeatable) => {
                if repeatable {
                    worker
                        .client
                        .batch_execute("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY")?;
                }
                let first = read(
                    &mut worker.client,
                    &worker.all,
                    &mut stats,
                    false,
                    Some(total),
                )?;
                read(&mut worker.client, &worker.range, &mut stats, false, None)?;
                if repeatable {
                    let last = read(
                        &mut worker.client,
                        &worker.all,
                        &mut stats,
                        false,
                        Some(total),
                    )?;
                    if first != last {
                        return Err("repeatable-read stress snapshot changed".into());
                    }
                    worker.client.batch_execute("COMMIT")?;
                }
            }
            Actor::Publisher => match worker.client.query_one(&worker.publish, &[]) {
                Ok(row) => {
                    let captured: i64 = row.try_get(0)?;
                    if !(i64::from(rows)..=i64::from(rows) + 2).contains(&captured) {
                        return Err("stress publication returned an invalid row count".into());
                    }
                    stats.commits += 1;
                }
                Err(error) if expected_conflict(&error) => stats.lock_conflicts += 1,
                Err(error) => return Err(error.into()),
            },
            Actor::Maintenance => {
                let sql = match stats.cycles % 4 {
                    0 => "VACUUM (ANALYZE) trusted_stress_source",
                    1 => "REINDEX INDEX trusted_stress_source_pkey",
                    2 => "ALTER TABLE trusted_stress_source SET (autovacuum_enabled=true)",
                    _ => "VACUUM (ANALYZE) syntra.trusted_int4_projections",
                };
                match worker.client.batch_execute(sql) {
                    Ok(()) => {
                        stats.commits += 1;
                        *stats
                            .maintenance
                            .entry(match stats.cycles % 4 {
                                0 => "source_vacuum_analyze",
                                1 => "reindex",
                                2 => "ddl_epoch_change",
                                _ => "trusted_catalog_vacuum",
                            })
                            .or_default() += 1;
                    }
                    Err(error) if expected_conflict(&error) => stats.lock_conflicts += 1,
                    Err(error) => return Err(error.into()),
                }
            }
        }
        stats.finish(cycle)?;
    }
    if stats.cycles == 0 {
        return Err("trusted stress worker made no progress".into());
    }
    Ok(stats)
}

// Ensure peers stop on a returned error or panic, including during scoped join.
struct StopOnExit<'a>(&'a AtomicBool);
impl Drop for StopOnExit<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

fn footprint(client: &mut Client) -> Result<Value> {
    Ok(client
        .query_one(
            r#"SELECT json_build_object(
        'source_bytes',pg_total_relation_size('trusted_stress_source'),
        'trusted_catalog_bytes',pg_total_relation_size('syntra.trusted_int4_projections'),
        'counter_bytes',pg_total_relation_size('syntra.source_generation_shards'),
        'wal_lsn',pg_current_wal_insert_lsn()::text)"#,
            &[],
        )?
        .try_get(0)?)
}

pub(super) fn run(cluster: &Cluster, options: &Options) -> Result<Value> {
    eprintln!(
        "Trusted concurrency stress: {} seconds, 8 independent sessions",
        options.trusted_stress_seconds
    );
    let rows = options.rows.clamp(4, 100000);
    let mut admin = cluster.connect("syntra_enabled")?;
    admin.batch_execute("SET statement_timeout='15s'; SET lock_timeout='250ms'; CREATE TABLE trusted_stress_source(id int4 PRIMARY KEY,v int4); CREATE ROLE trusted_stress_reader NOLOGIN; GRANT SELECT ON trusted_stress_source TO trusted_stress_reader")?;
    admin.execute("INSERT INTO trusted_stress_source(id,v) SELECT n,CASE WHEN n%17=0 THEN NULL ELSE 1000000+n END FROM generate_series(1,$1::int4) AS n",&[&rows])?;
    admin.query_one(
        "SELECT syntra_track_source('trusted_stress_source','v')",
        &[],
    )?;
    admin.query_one(PUBLISH, &[])?;
    let total: i64 = admin
        .query_one("SELECT sum(v) FROM trusted_stress_source", &[])?
        .try_get(0)?;
    let before = footprint(&mut admin)?;
    let mut workers = Vec::new();
    for (actor, name) in [
        (Actor::Writer(0), "writer_0"),
        (Actor::Writer(1), "writer_1"),
        (Actor::Reader(false), "read_committed_0"),
        (Actor::Reader(false), "read_committed_1"),
        (Actor::Reader(true), "repeatable_read"),
        (Actor::Publisher, "publisher_0"),
        (Actor::Publisher, "publisher_1"),
        (Actor::Maintenance, "maintenance"),
    ] {
        let mut client = cluster.connect("syntra_enabled")?;
        client.batch_execute("SET statement_timeout='15s'; SET lock_timeout='250ms'; SET idle_in_transaction_session_timeout='20s'")?;
        if matches!(actor, Actor::Reader(_)) {
            client.batch_execute("SET ROLE trusted_stress_reader")?;
        }
        let all = client.prepare(READ_ALL)?;
        let range = client.prepare(READ_RANGE)?;
        let transfer = client.prepare(TRANSFER)?;
        let publish = client.prepare(PUBLISH)?;
        workers.push(Worker {
            client,
            actor,
            all,
            range,
            transfer,
            publish,
            name,
        });
    }
    let stop = AtomicBool::new(false);
    let start = Instant::now();
    let deadline = start + Duration::from_secs(options.trusted_stress_seconds);
    let reports = thread::scope(|scope| -> Result<Vec<Stats>> {
        let mut handles = Vec::new();
        for worker in workers {
            let stop = &stop;
            let name = worker.name;
            match thread::Builder::new()
                .name(worker.name.into())
                .spawn_scoped(scope, move || {
                    let _stop_on_exit = StopOnExit(stop);
                    work(worker, start, deadline, stop, rows, total)
                        .map_err(|error| format!("{name}: {error}"))
                }) {
                Ok(handle) => handles.push(handle),
                Err(error) => {
                    stop.store(true, Ordering::Release);
                    return Err(error.into());
                }
            }
        }
        let mut reports = Vec::new();
        let mut failure = None;
        for handle in handles {
            match handle.join() {
                Ok(Ok(report)) => reports.push(report),
                Ok(Err(error)) => {
                    failure.get_or_insert(error);
                    stop.store(true, Ordering::Release);
                }
                Err(_) => {
                    failure.get_or_insert_with(|| "trusted stress worker panicked".into());
                    stop.store(true, Ordering::Release);
                }
            }
        }
        if let Some(failure) = failure {
            return Err(failure.into());
        }
        Ok(reports)
    })?;
    let elapsed = start.elapsed().as_secs_f64();
    for stats in &reports {
        if stats.name == "maintenance" && stats.maintenance.len() != 4 {
            return Err("stress did not complete every maintenance operation".into());
        }
        if stats.name.starts_with("writer") && (stats.commits == 0 || stats.rollbacks == 0) {
            return Err("stress writer did not exercise commit and rollback".into());
        }
        if (stats.name.starts_with("publisher") || stats.name == "maintenance")
            && stats.commits == 0
        {
            return Err(
                "stress publisher or maintenance worker had no successful operation".into(),
            );
        }
    }
    let after = footprint(&mut admin)?;
    let wal_bytes: String = admin
        .query_one(
            "SELECT pg_wal_lsn_diff($1::text::pg_lsn,$2::text::pg_lsn)::text",
            &[
                &after["wal_lsn"].as_str().ok_or("missing final LSN")?,
                &before["wal_lsn"].as_str().ok_or("missing initial LSN")?,
            ],
        )?
        .try_get(0)?;
    admin.query_one(PUBLISH, &[])?;
    let statement = admin.prepare(READ_ALL)?;
    let mut final_stats = Stats::new("quiescent_check", Instant::now())?;
    read(&mut admin, &statement, &mut final_stats, false, Some(total))?;
    if final_stats.routes.get("projection/eligible") != Some(&1) {
        return Err("trusted reuse did not recover after stress stopped".into());
    }
    // VACUUM must have its own protocol command, outside a transaction block.
    admin.batch_execute("VACUUM (ANALYZE) trusted_stress_source")?;
    admin.batch_execute("VACUUM (ANALYZE) syntra.trusted_int4_projections")?;
    admin.batch_execute("VACUUM (ANALYZE) syntra.source_generation_shards")?;
    let vacuumed = footprint(&mut admin)?;
    admin.batch_execute("DROP TABLE trusted_stress_source; DROP ROLE trusted_stress_reader")?;
    Ok(
        json!({"duration_seconds":options.trusted_stress_seconds,"elapsed_including_drain_seconds":elapsed,
        "source_rows":rows,"workers":reports.iter().map(Stats::report).collect::<Vec<_>>(),
        "unexpected_errors":0,"invariant_failures":0,"quiescent_projection_reuse":true,
        "before":before,"after":after,"after_vacuum":vacuumed,"wal_bytes":wal_bytes,
        "scope":"saturated correctness stress; per-cycle service latency; no arrival pacing, hard memory quota or comparative OLTP latency claim"}),
    )
}
