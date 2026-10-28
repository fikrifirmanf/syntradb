//! Backend-attributed statement WAL, plus separately labelled utility intervals.
use crate::Cluster;
use postgres::Client;
use serde_json::{json, Value};
use std::time::Instant;
use syntra_bench::{Options, Result, WalUsage};

const PUBLISH: &str = "SELECT syntra_publish_trusted_int4('refresh_tracked','v')";
const CHANGE: &str =
    "UPDATE refresh_tracked SET v=v+CASE WHEN id=1 THEN -1 ELSE 1 END WHERE id IN(1,2)";

struct Case {
    name: &'static str,
    sql: &'static str,
    explain: &'static str,
    publication: bool,
    changed: bool,
}
const CASES:[Case;6]=[
    Case{name:"native_transfer",sql:"SELECT refresh_transfer_plain()",explain:"EXPLAIN (ANALYZE,BUFFERS,WAL,FORMAT JSON) SELECT refresh_transfer_plain()",publication:false,changed:false},
    Case{name:"tracked_transfer",sql:"SELECT refresh_transfer_tracked()",explain:"EXPLAIN (ANALYZE,BUFFERS,WAL,FORMAT JSON) SELECT refresh_transfer_tracked()",publication:false,changed:false},
    Case{name:"captured_transfer",sql:"SELECT refresh_transfer_captured()",explain:"EXPLAIN (ANALYZE,BUFFERS,WAL,FORMAT JSON) SELECT refresh_transfer_captured()",publication:false,changed:false},
    Case{name:"source_capture",sql:"SELECT source_rows FROM syntra_capture_int4('refresh_tracked','v') LIMIT 1",explain:"EXPLAIN (ANALYZE,BUFFERS,WAL,FORMAT JSON) SELECT source_rows FROM syntra_capture_int4('refresh_tracked','v') LIMIT 1",publication:false,changed:false},
    Case{name:"publication_changed",sql:PUBLISH,explain:"EXPLAIN (ANALYZE,BUFFERS,WAL,FORMAT JSON) SELECT syntra_publish_trusted_int4('refresh_tracked','v')",publication:true,changed:true},
    Case{name:"publication_unchanged",sql:PUBLISH,explain:"EXPLAIN (ANALYZE,BUFFERS,WAL,FORMAT JSON) SELECT syntra_publish_trusted_int4('refresh_tracked','v')",publication:true,changed:false},
];

fn prepare(client: &mut Client, case: &Case) -> Result<()> {
    if case.publication {
        if case.changed {
            client.execute(CHANGE, &[])?;
        } else {
            client.query_one(PUBLISH, &[])?;
        }
    }
    Ok(())
}

fn verify(client: &mut Client, rows: i32, total: i64) -> Result<()> {
    let matched:bool=client.query_one(r#"SELECT s.route='projection' AND
        ROW(s.rows,s.non_nulls,s.sum,s.min,s.max) IS NOT DISTINCT FROM
        ROW(n.rows,n.non_nulls,n.sum,n.min,n.max) AND n.rows=$1::int8 AND n.sum=$2
        FROM syntra_live_int4_summary('refresh_tracked','v') AS s,
        (SELECT count(*) AS rows,count(v) AS non_nulls,sum(v),min(v),max(v) FROM refresh_tracked) AS n LIMIT 1"#,&[&i64::from(rows),&total])?.try_get(0)?;
    if !matched {
        return Err("refresh publication disagreed with native SQL".into());
    }
    Ok(())
}

pub(super) fn run(cluster: &Cluster, options: &Options) -> Result<Value> {
    eprintln!(
        "Isolated refresh costs: {} samples per case",
        options.repetitions
    );
    let rows = options.rows.max(2);
    let mut client = cluster.connect("syntra_enabled")?;
    client.batch_execute("SET statement_timeout='15s'; SET lock_timeout='1s'; CREATE TABLE refresh_plain(id int4 PRIMARY KEY,v int4); CREATE TABLE refresh_tracked(id int4 PRIMARY KEY,v int4); CREATE TABLE refresh_captured(id int4 PRIMARY KEY,v int4)")?;
    client.execute(
        "INSERT INTO refresh_plain(id,v) SELECT n,1000000+n FROM generate_series(1,$1::int4) AS n",
        &[&rows],
    )?;
    client.execute("INSERT INTO refresh_tracked(id,v) SELECT n,1000000+n FROM generate_series(1,$1::int4) AS n",&[&rows])?;
    let captured_rows = rows.min(100000);
    client.execute("INSERT INTO refresh_captured(id,v) SELECT n,1000000+n FROM generate_series(1,$1::int4) AS n",&[&captured_rows])?;
    client.query_one(
        "SELECT syntra_seed_capture('refresh_captured','id','v',4096)",
        &[],
    )?;
    client.batch_execute(
        r#"CREATE FUNCTION refresh_transfer_plain() RETURNS bigint LANGUAGE plpgsql AS $$
        DECLARE changed bigint; BEGIN
        UPDATE refresh_plain SET v=v+CASE WHEN id=1 THEN -1 ELSE 1 END WHERE id IN(1,2);
        GET DIAGNOSTICS changed=ROW_COUNT; RETURN changed; END $$;
        CREATE FUNCTION refresh_transfer_tracked() RETURNS bigint LANGUAGE plpgsql AS $$
        DECLARE changed bigint; BEGIN
        UPDATE refresh_tracked SET v=v+CASE WHEN id=1 THEN -1 ELSE 1 END WHERE id IN(1,2);
        GET DIAGNOSTICS changed=ROW_COUNT; RETURN changed; END $$;
        CREATE FUNCTION refresh_transfer_captured() RETURNS bigint LANGUAGE plpgsql AS $$
        DECLARE changed bigint; BEGIN
        UPDATE refresh_captured SET v=v+CASE WHEN id=1 THEN -1 ELSE 1 END WHERE id IN(1,2);
        GET DIAGNOSTICS changed=ROW_COUNT; RETURN changed; END $$"#,
    )?;
    client.query_one("SELECT syntra_track_source('refresh_tracked','v')", &[])?;
    client.query_one(PUBLISH, &[])?;
    let total: i64 = client
        .query_one("SELECT sum(v) FROM refresh_tracked", &[])?
        .try_get(0)?;
    let mut reports = Vec::new();
    // unslop-ignore N_PLUS_ONE_IN_LOOP -- Each case prepares and measures a distinct benchmark route; batching would change the measured operation.
    for case in &CASES {
        let request = client.prepare(case.sql)?;
        let plan = client.prepare(case.explain)?;
        let mut times = Vec::new();
        let mut wal = Vec::new();
        // unslop-ignore N_PLUS_ONE_IN_LOOP -- Each iteration measures an independent prepared statement, with setup and correctness checks outside its timing.
        for round in 0..=options.repetitions {
            prepare(&mut client, case)?;
            let start = Instant::now();
            let output = client.query(&request, &[])?;
            let elapsed = start.elapsed().as_secs_f64() * 1000.0;
            if case.name.ends_with("transfer") {
                if output.len() != 1 || output[0].try_get::<_, i64>(0)? != 2 {
                    return Err("isolated transfer changed the wrong row count".into());
                }
            } else if output.len() != 1 || output[0].try_get::<_, i64>(0)? != i64::from(rows) {
                return Err("isolated capture/publication row count mismatch".into());
            }
            if case.publication {
                verify(&mut client, rows, total)?;
            }
            prepare(&mut client, case)?;
            let explain: Value = client.query_one(&plan, &[])?.try_get(0)?;
            let usage = WalUsage::from_explain(&explain)?;
            if case.publication {
                verify(&mut client, rows, total)?;
            }
            if round != 0 {
                times.push(elapsed);
                wal.push(usage.report());
            }
        }
        if case.name == "captured_transfer" {
            let expected = 4_i64 * i64::try_from(options.repetitions + 1)?;
            let complete:bool=client.query_one("SELECT h.emitted=$1 AND (SELECT count(*) FROM syntra.capture_events AS e WHERE e.epoch=h.epoch)=$1 AND (SELECT v FROM refresh_captured WHERE id=1)=1000001-$1/2 AND (SELECT v FROM refresh_captured WHERE id=2)=1000002+$1/2 FROM (SELECT hh.source_relation,hh.epoch,hh.base_rows,hh.pruned,coalesce(m.ordinal,hh.emitted) AS emitted,coalesce(m.live_rows,hh.live_rows) AS live_rows FROM syntra.capture_heads AS hh LEFT JOIN LATERAL (SELECT ordinal,live_rows FROM syntra.capture_marks WHERE epoch=hh.epoch AND ordinal>hh.emitted ORDER BY ordinal DESC LIMIT 1) AS m ON true) AS h WHERE h.source_relation='refresh_captured'::regclass LIMIT 1",&[&expected])?.try_get(0)?;
            if !complete {
                return Err("captured transfer probe omitted source changes or event rows".into());
            }
        }
        reports.push(json!({"name":case.name,"prepared_request_samples_ms":times,"statement_wal_samples":wal}));
    }
    let checkpoint_costs = checkpoint_costs(&mut client, options, captured_rows)?;
    let mut utilities = Vec::new();
    for (name, sql) in [
        ("source_vacuum", "VACUUM (ANALYZE) refresh_tracked"),
        (
            "catalog_vacuum",
            "VACUUM (ANALYZE) syntra.trusted_int4_projections",
        ),
        ("reindex", "REINDEX INDEX refresh_tracked_pkey"),
        (
            "source_ddl",
            "ALTER TABLE refresh_tracked SET (autovacuum_enabled=true)",
        ),
    ] {
        let mut samples = Vec::new();
        // unslop-ignore N_PLUS_ONE_IN_LOOP -- Utility samples measure isolated operations, not per-row application requests.
        for _ in 0..options.repetitions {
            let before: String = client
                .query_one("SELECT pg_current_wal_insert_lsn()::text", &[])?
                .try_get(0)?;
            let start = Instant::now();
            client.batch_execute(sql)?;
            let elapsed = start.elapsed().as_secs_f64() * 1000.0;
            let bytes: String = client
                .query_one(
                    "SELECT pg_wal_lsn_diff(pg_current_wal_insert_lsn(),$1::text::pg_lsn)::text",
                    &[&before],
                )?
                .try_get(0)?;
            samples.push(json!({"request_ms":elapsed,"node_wal_bytes":bytes}));
        }
        utilities.push(json!({"name":name,"samples":samples}));
    }
    client.query_one(PUBLISH, &[])?;
    verify(&mut client, rows, total)?;
    client.batch_execute("DROP FUNCTION refresh_transfer_plain(),refresh_transfer_tracked(),refresh_transfer_captured(); DROP TABLE refresh_plain,refresh_tracked,refresh_captured")?;
    Ok(
        json!({"rows":rows,"captured_rows":captured_rows,"cases":reports,"utilities":utilities,"checkpoint_costs":checkpoint_costs,
        "statement_wal_scope":"root SELECT EXPLAIN WAL counters for this backend; transfer wrappers include AFTER STATEMENT trigger work; excludes commit record and setup; do not sum child nodes",
        "utility_wal_scope":"isolated node insert-LSN intervals, including commit/background work; no exact backend attribution",
        "timing_scope":"normal prepared request roundtrip; setup, correctness checks and separate EXPLAIN probes excluded; cases sequential"}),
    )
}

const CHECKPOINT_APPLY: &str = "SELECT syntra_apply_capture('refresh_captured')";
const CHECKPOINT_PRUNE: &str = "SELECT syntra_prune_capture('refresh_captured')";
fn checkpoint_prepare(client: &mut Client, name: &str) -> Result<()> {
    client.query_one(CHECKPOINT_APPLY, &[])?;
    client.query_one(CHECKPOINT_PRUNE, &[])?;
    if matches!(
        name,
        "apply_two_events"
            | "prune_two_events"
            | "replay_two_events"
            | "native_two_events"
            | "apply_prune_two_events"
            | "separate_apply_prune_two_events"
    ) {
        let changed: i64 = client
            .query_one("SELECT refresh_transfer_captured()", &[])?
            .try_get(0)?;
        if changed != 2 {
            return Err("checkpoint cost setup changed the wrong row count".into());
        }
        if name == "prune_two_events" {
            client.query_one(CHECKPOINT_APPLY, &[])?;
        }
    }
    if matches!(name, "replay_256_events" | "native_256_events") {
        client.batch_execute("DO $$ BEGIN FOR n IN 1..256 LOOP UPDATE refresh_captured SET v=v+1 WHERE id=1; END LOOP; END $$")?;
    }
    if matches!(name, "heap_257_events" | "native_257_events") {
        client.batch_execute("DO $$ BEGIN FOR n IN 1..257 LOOP UPDATE refresh_captured SET v=v+1 WHERE id=1; END LOOP; END $$")?;
    }
    Ok(())
}
fn checkpoint_verify(client: &mut Client, name: &str) -> Result<()> {
    let route = match name {
        "native_two_events" | "replay_two_events" | "replay_256_events" | "native_256_events" => {
            "replay"
        }
        "heap_257_events" | "native_257_events" => "heap",
        _ => "checkpoint",
    };
    let matched:bool=client.query_one("SELECT s.route=$1 AND ROW(s.rows,s.non_nulls,s.sum,s.min,s.max) IS NOT DISTINCT FROM ROW(n.rows,n.non_nulls,n.sum,n.min,n.max) FROM syntra_checkpoint_int4_summary('refresh_captured','v',true) AS s,(SELECT count(*) AS rows,count(v) AS non_nulls,sum(v),min(v),max(v) FROM refresh_captured) AS n LIMIT 1",&[&route])?.try_get(0)?;
    if !matched {
        return Err("checkpoint cost result disagreed with native SQL".into());
    }
    if name.contains("apply_prune") {
        let settled:bool=client.query_one("SELECT h.pruned=h.emitted AND c.applied=h.emitted AND (SELECT count(*) FROM syntra.capture_events WHERE source_relation=h.source_relation)=0 FROM (SELECT hh.source_relation,hh.epoch,hh.base_rows,hh.pruned,coalesce(m.ordinal,hh.emitted) AS emitted,coalesce(m.live_rows,hh.live_rows) AS live_rows FROM syntra.capture_heads AS hh LEFT JOIN LATERAL (SELECT ordinal,live_rows FROM syntra.capture_marks WHERE epoch=hh.epoch AND ordinal>hh.emitted ORDER BY ordinal DESC LIMIT 1) AS m ON true) AS h JOIN syntra.capture_checkpoints AS c USING(source_relation,epoch) WHERE h.source_relation='refresh_captured'::regclass LIMIT 1", &[])?.try_get(0)?;
        if !settled {
            return Err("combined maintenance left an inconsistent retained feed".into());
        }
    }
    Ok(())
}
fn checkpoint_costs(client: &mut Client, options: &Options, rows: i32) -> Result<Value> {
    let cases = [
        ("native_summary", "SELECT count(*),count(v),sum(v),min(v),max(v) FROM refresh_captured", "EXPLAIN (ANALYZE,BUFFERS,WAL,FORMAT JSON) SELECT count(*),count(v),sum(v),min(v),max(v) FROM refresh_captured"),
        ("checkpoint_summary", "SELECT rows,non_nulls,sum,min,max FROM syntra_checkpoint_int4_summary('refresh_captured','v') LIMIT 1", "EXPLAIN (ANALYZE,BUFFERS,WAL,FORMAT JSON) SELECT rows,non_nulls,sum,min,max FROM syntra_checkpoint_int4_summary('refresh_captured','v') LIMIT 1"),
        ("native_two_events", "SELECT count(*),count(v),sum(v),min(v),max(v) FROM refresh_captured", "EXPLAIN (ANALYZE,BUFFERS,WAL,FORMAT JSON) SELECT count(*),count(v),sum(v),min(v),max(v) FROM refresh_captured"),
        ("replay_two_events", "SELECT rows,non_nulls,sum,min,max FROM syntra_checkpoint_int4_summary('refresh_captured','v',true) LIMIT 1", "EXPLAIN (ANALYZE,BUFFERS,WAL,FORMAT JSON) SELECT rows,non_nulls,sum,min,max FROM syntra_checkpoint_int4_summary('refresh_captured','v',true) LIMIT 1"),
        ("native_256_events", "SELECT count(*),count(v),sum(v),min(v),max(v) FROM refresh_captured", "EXPLAIN (ANALYZE,BUFFERS,WAL,FORMAT JSON) SELECT count(*),count(v),sum(v),min(v),max(v) FROM refresh_captured"),
        ("native_257_events", "SELECT count(*),count(v),sum(v),min(v),max(v) FROM refresh_captured", "EXPLAIN (ANALYZE,BUFFERS,WAL,FORMAT JSON) SELECT count(*),count(v),sum(v),min(v),max(v) FROM refresh_captured"),
        ("replay_256_events", "SELECT rows,non_nulls,sum,min,max FROM syntra_checkpoint_int4_summary('refresh_captured','v',true) LIMIT 1", "EXPLAIN (ANALYZE,BUFFERS,WAL,FORMAT JSON) SELECT rows,non_nulls,sum,min,max FROM syntra_checkpoint_int4_summary('refresh_captured','v',true) LIMIT 1"),
        ("heap_257_events", "SELECT rows,non_nulls,sum,min,max FROM syntra_checkpoint_int4_summary('refresh_captured','v',true) LIMIT 1", "EXPLAIN (ANALYZE,BUFFERS,WAL,FORMAT JSON) SELECT rows,non_nulls,sum,min,max FROM syntra_checkpoint_int4_summary('refresh_captured','v',true) LIMIT 1"),
        ("apply_two_events", CHECKPOINT_APPLY, "EXPLAIN (ANALYZE,BUFFERS,WAL,FORMAT JSON) SELECT syntra_apply_capture('refresh_captured')"),
        ("apply_prune_two_events", "SELECT applied,pruned FROM syntra_apply_prune_capture('refresh_captured')", "EXPLAIN (ANALYZE,BUFFERS,WAL,FORMAT JSON) SELECT applied,pruned FROM syntra_apply_prune_capture('refresh_captured')"),
        ("apply_prune_unchanged", "SELECT applied,pruned FROM syntra_apply_prune_capture('refresh_captured')", "EXPLAIN (ANALYZE,BUFFERS,WAL,FORMAT JSON) SELECT applied,pruned FROM syntra_apply_prune_capture('refresh_captured')"),
        ("separate_apply_prune_two_events", "WITH a AS MATERIALIZED (SELECT syntra_apply_capture('refresh_captured') AS applied) SELECT applied,syntra_prune_capture('refresh_captured') AS pruned FROM a", "EXPLAIN (ANALYZE,BUFFERS,WAL,FORMAT JSON) WITH a AS MATERIALIZED (SELECT syntra_apply_capture('refresh_captured') AS applied) SELECT applied,syntra_prune_capture('refresh_captured') AS pruned FROM a"),
        ("separate_apply_prune_unchanged", "WITH a AS MATERIALIZED (SELECT syntra_apply_capture('refresh_captured') AS applied) SELECT applied,syntra_prune_capture('refresh_captured') AS pruned FROM a", "EXPLAIN (ANALYZE,BUFFERS,WAL,FORMAT JSON) WITH a AS MATERIALIZED (SELECT syntra_apply_capture('refresh_captured') AS applied) SELECT applied,syntra_prune_capture('refresh_captured') AS pruned FROM a"),
        ("apply_unchanged", CHECKPOINT_APPLY, "EXPLAIN (ANALYZE,BUFFERS,WAL,FORMAT JSON) SELECT syntra_apply_capture('refresh_captured')"),
        ("prune_two_events", CHECKPOINT_PRUNE, "EXPLAIN (ANALYZE,BUFFERS,WAL,FORMAT JSON) SELECT syntra_prune_capture('refresh_captured')"),
    ];
    let mut reports = Vec::new();
    // unslop-ignore N_PLUS_ONE_IN_LOOP -- Each case measures one distinct prepared operation, rather than fetching application rows.
    for (name, sql, explain) in cases {
        let request = client.prepare(sql)?;
        let plan = client.prepare(explain)?;
        let mut times = Vec::new();
        let mut wal = Vec::new();
        // unslop-ignore N_PLUS_ONE_IN_LOOP -- Independent timed requests and separate instrumented probes require per-sample setup outside their measured interval.
        for round in 0..=options.repetitions {
            checkpoint_prepare(client, name)?;
            let expected = match name {
                "apply_two_events" | "apply_unchanged" | "apply_prune_two_events" | "apply_prune_unchanged" | "separate_apply_prune_two_events" | "separate_apply_prune_unchanged" => client.query_one("SELECT emitted FROM (SELECT hh.source_relation,hh.epoch,hh.base_rows,hh.pruned,coalesce(m.ordinal,hh.emitted) AS emitted,coalesce(m.live_rows,hh.live_rows) AS live_rows FROM syntra.capture_heads AS hh LEFT JOIN LATERAL (SELECT ordinal,live_rows FROM syntra.capture_marks WHERE epoch=hh.epoch AND ordinal>hh.emitted ORDER BY ordinal DESC LIMIT 1) AS m ON true) AS h WHERE source_relation='refresh_captured'::regclass LIMIT 1",&[])?.try_get(0)? ,
                "prune_two_events" => 2,
                _ => i64::from(rows),
            };
            let start = Instant::now();
            let row = client.query_one(&request, &[])?;
            let result: i64 = row.try_get(0)?;
            let maintenance_pruned = if name.contains("apply_prune") {
                Some(row.try_get::<_, i64>(1)?)
            } else {
                None
            };
            let elapsed = start.elapsed().as_secs_f64() * 1000.0;
            if result != expected {
                return Err("checkpoint cost cursor or row count mismatch".into());
            }
            if name.contains("apply_prune") {
                let expected_pruned = if name.ends_with("two_events") {
                    2_i64
                } else {
                    0
                };
                if maintenance_pruned != Some(expected_pruned) {
                    return Err("maintenance deleted-event count mismatch".into());
                }
            }
            checkpoint_verify(client, name)?;
            checkpoint_prepare(client, name)?;
            let value: Value = client.query_one(&plan, &[])?.try_get(0)?;
            let usage = WalUsage::from_explain(&value)?;
            checkpoint_verify(client, name)?;
            if round != 0 {
                times.push(elapsed);
                wal.push(usage.report());
            }
        }
        reports.push(
            json!({"name":name,"prepared_request_samples_ms":times,"statement_wal_samples":wal}),
        );
    }
    Ok(
        json!({"rows":rows,"cases":reports,"scope":"sequential warm single-source operations; 2/256/257-event read setup excluded; two-event setup and initial checkpoint excluded; apply rewrites affected keyed and columnar partitions plus manifest; full five-field summaries checked against native SQL"}),
    )
}
