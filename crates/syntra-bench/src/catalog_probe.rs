//! Controlled retained-version lookup work; no concurrent writer during samples.
use crate::Cluster;
use postgres::Client;
use serde_json::{json, Value};
use std::time::Instant;
use syntra_bench::{correctness_values, AggregateValues, Options, Result};
const READ:&str="SELECT rows,non_nulls,sum,min,max,route,reason,pg_current_xact_id_if_assigned() IS NULL FROM syntra_checkpoint_int4_summary('catalog_probe_source','v',true) LIMIT 1";
const PLAN:&str="EXPLAIN (ANALYZE,BUFFERS,WAL,FORMAT JSON) SELECT rows,non_nulls,sum,min,max FROM syntra_checkpoint_int4_summary('catalog_probe_source','v',true) LIMIT 1";
const NATIVE: &str = "SELECT count(*),count(v),sum(v),min(v),max(v) FROM catalog_probe_source";
const TRANSFER: &str =
    "UPDATE catalog_probe_source SET v=v+CASE WHEN id=1 THEN -1 ELSE 1 END WHERE id IN(1,2)";
const CYCLE:&str="UPDATE catalog_probe_source SET v=v+CASE WHEN id=1 THEN -1 ELSE 1 END WHERE id IN(1,2); SELECT syntra_apply_capture('catalog_probe_source'); SELECT syntra_prune_capture('catalog_probe_source')";
fn require(ok: bool, message: &'static str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(message.into())
    }
}
fn counters(client: &mut Client) -> Result<Value> {
    Ok(client.query_one("SELECT json_build_object('chunk_index_entries',pg_stat_get_xact_tuples_returned('syntra.capture_chunks_pkey'::regclass)+COALESCE(pg_stat_get_xact_tuples_returned(to_regclass('syntra.capture_chunks_version_index')),0),'chunk_heap_fetches',pg_stat_get_xact_tuples_fetched('syntra.capture_chunks'::regclass),'chunk_heap_blocks',pg_stat_get_xact_blocks_fetched('syntra.capture_chunks'::regclass),'manifest_index_entries',pg_stat_get_xact_tuples_returned('syntra.capture_checkpoints_pkey'::regclass)+COALESCE(pg_stat_get_xact_tuples_returned(to_regclass('syntra.capture_checkpoint_latest_index')),0),'manifest_heap_blocks',pg_stat_get_xact_blocks_fetched('syntra.capture_checkpoints'::regclass),'head_index_entries',pg_stat_get_xact_tuples_returned('syntra.capture_heads_pkey'::regclass)+COALESCE(pg_stat_get_xact_tuples_returned(to_regclass('syntra.capture_head_latest_index')),0),'head_heap_blocks',pg_stat_get_xact_blocks_fetched('syntra.capture_heads'::regclass),'event_index_entries',pg_stat_get_xact_tuples_returned('syntra.capture_events_pkey'::regclass))",&[])?.try_get(0)?)
}
fn values(client: &mut Client, route: &str, expected: AggregateValues) -> Result<()> {
    let row = client.query_one(READ, &[])?;
    require(
        correctness_values(&row)? == expected
            && row.try_get::<_, &str>(5)? == route
            && row.try_get::<_, bool>(7)?,
        "catalog probe result, route or XID invariant failed",
    )
}
fn sample(client: &mut Client, repetitions: usize, route: &str) -> Result<Value> {
    client.batch_execute("BEGIN READ ONLY")?;
    let result = sample_active(client, repetitions, route)?;
    client.batch_execute("COMMIT")?;
    Ok(result)
}
fn sample_active(client: &mut Client, repetitions: usize, route: &str) -> Result<Value> {
    let expected = correctness_values(&client.query_one(NATIVE, &[])?)?;
    let request = client.prepare(READ)?;
    let before = counters(client)?;
    let mut samples = Vec::new();
    // unslop-ignore N_PLUS_ONE_IN_LOOP -- Each iteration measures a distinct prepared request; batching changes the latency being measured.
    for round in 0..=repetitions {
        let start = Instant::now();
        let row = client.query_one(&request, &[])?;
        let elapsed = start.elapsed().as_secs_f64() * 1000.0;
        require(
            correctness_values(&row)? == expected
                && row.try_get::<_, &str>(5)? == route
                && row.try_get::<_, bool>(7)?,
            "catalog probe request disagreed with native values, route or XID contract",
        )?;
        if round > 0 {
            samples.push(elapsed);
        }
    }
    let after = counters(client)?;
    let mut delta = serde_json::Map::new();
    for (key, value) in after.as_object().ok_or("invalid catalog counters")? {
        let difference = value
            .as_i64()
            .and_then(|v| v.checked_sub(before[key].as_i64()?))
            .filter(|v| *v >= 0)
            .ok_or("catalog counter decreased during probe transaction")?;
        delta.insert(key.clone(), json!(difference));
    }
    require(
        delta["chunk_index_entries"].as_i64().is_some_and(|n| n > 0),
        "native catalog lookup counters did not record work",
    )?;
    let plan: Value = client.query_one(PLAN, &[])?.try_get(0)?;
    Ok(
        json!({"route":route,"prepared_request_samples_ms":samples,"counter_requests_including_warmup":repetitions+1,"backend_local_counter_deltas":delta,"separate_explain":plan}),
    )
}
fn stage(
    admin: &mut Client,
    reader: &mut Client,
    options: &Options,
    cycles: usize,
    held: bool,
) -> Result<Value> {
    let generation:i64=admin.query_one("SELECT generation FROM syntra.capture_checkpoints WHERE source_relation='catalog_probe_source'::regclass LIMIT 1",&[])?.try_get(0)?;
    let checkpoint = sample(reader, options.repetitions, "checkpoint")?;
    admin.batch_execute(TRANSFER)?;
    let replay = sample(reader, options.repetitions, "replay")?;
    admin.batch_execute("BEGIN")?;
    let started = Instant::now();
    let applied: i64 = admin
        .query_one("SELECT syntra_apply_capture('catalog_probe_source')", &[])?
        .try_get(0)?;
    let apply_ms = started.elapsed().as_secs_f64() * 1000.0;
    let started = Instant::now();
    let pruned: i64 = admin
        .query_one("SELECT syntra_prune_capture('catalog_probe_source')", &[])?
        .try_get(0)?;
    let prune_ms = started.elapsed().as_secs_f64() * 1000.0;
    let started = Instant::now();
    admin.batch_execute("COMMIT")?;
    let commit_ms = started.elapsed().as_secs_f64() * 1000.0;
    require(
        applied > 0 && pruned == 2,
        "catalog probe maintenance failed to settle the two-event tail",
    )?;
    Ok(
        json!({"churn_cycles":cycles,"held_snapshot":held,"manifest_generation_before_samples":generation,"checkpoint":checkpoint,"two_event_replay":replay,
            "settle_tail":{"apply_ms":apply_ms,"prune_ms":prune_ms,"commit_ms":commit_ms,"pruned_events":pruned}}),
    )
}
pub(super) fn run(cluster: &Cluster, options: &Options) -> Result<Value> {
    eprintln!(
        "Retained catalog lookup probe: fixed churn stages and {} read samples",
        options.repetitions
    );
    let rows = options.rows.clamp(4, 100000);
    let mut admin = cluster.connect("syntra_enabled")?;
    let mut reader = cluster.connect("syntra_enabled")?;
    let mut anchor = cluster.connect("syntra_enabled")?;
    admin.batch_execute("SET statement_timeout='15s'; SET lock_timeout='5s'; CREATE TABLE catalog_probe_source(id int4 PRIMARY KEY,v int4)")?;
    admin.execute(
        "INSERT INTO catalog_probe_source SELECT n,1000000+n FROM generate_series(1,$1::int4) AS n",
        &[&rows],
    )?;
    admin.query_one(
        "SELECT syntra_seed_capture('catalog_probe_source','id','v',4096)",
        &[],
    )?;
    admin.query_one("SELECT syntra_apply_capture('catalog_probe_source')", &[])?;
    reader.batch_execute("SET statement_timeout='15s'; SET lock_timeout='5s'")?;
    anchor.batch_execute("SET statement_timeout='15s'; SET idle_in_transaction_session_timeout='330s'; BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY")?;
    let initial = correctness_values(&anchor.query_one(NATIVE, &[])?)?;
    values(&mut anchor, "checkpoint", initial)?;
    let mut stages = Vec::new();
    let mut completed = 0;
    // unslop-ignore N_PLUS_ONE_IN_LOOP -- Churn is a sequence of committed publications whose retained versions are the subject of this probe.
    for target in [0, 64, 256, 1024] {
        let mut churn_samples = Vec::new();
        while completed < target {
            let started = Instant::now();
            admin.batch_execute(CYCLE)?;
            churn_samples.push(started.elapsed().as_secs_f64() * 1000.0);
            completed += 1;
        }
        let mut result = stage(&mut admin, &mut reader, options, completed, true)?;
        result["churn_cycle_samples_ms"] = json!(churn_samples);
        result["historical_checkpoint"] =
            sample_active(&mut anchor, options.repetitions, "checkpoint")?;
        require(
            correctness_values(&anchor.query_one(NATIVE, &[])?)? == initial,
            "historical probe lost its source snapshot",
        )?;
        stages.push(result);
    }
    values(&mut anchor, "checkpoint", initial)?;
    anchor.batch_execute("COMMIT")?;
    for sql in [
        "VACUUM (ANALYZE) syntra.capture_chunks",
        "VACUUM (ANALYZE) syntra.capture_checkpoints",
        "VACUUM (ANALYZE) syntra.capture_events",
        "VACUUM (ANALYZE) syntra.capture_marks",
        "VACUUM (ANALYZE) syntra.capture_heads",
        "VACUUM (ANALYZE) catalog_probe_source",
    ] {
        admin.batch_execute(sql)?;
    }
    let released = stage(&mut admin, &mut reader, options, completed, false)?;
    admin.batch_execute("DROP TABLE catalog_probe_source")?;
    Ok(
        json!({"source_rows":rows,"stages":stages,"after_anchor_release_and_vacuum":released,"old_snapshot_valid":true,"scope":"sequential controlled churn; current and historical checkpoint samples; transfer/apply/prune cycle timing; writers paused during samples; backend-local index/heap counters include one warmup and exclude EXPLAIN; normal prepared roundtrip timing; native expected values checked; no concurrent throughput or cross-database claim"}),
    )
}
