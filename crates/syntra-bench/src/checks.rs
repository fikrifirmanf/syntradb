//! Deterministic multi-session integration checks before any timings are reported.
use crate::Cluster;
use postgres::Client;
use serde_json::{json, Value};
use std::process::Command;
use syntra_bench::Result;

fn state(client: &mut Client) -> Result<(i64, Option<i64>, Option<i32>)> {
    let row = client.query_one(
        r#"SELECT s.rows, s.sum, s.min,
        ROW(r.rows,r.non_nulls,r.sum,r.min,r.max) IS NOT DISTINCT FROM ROW(s.rows,s.non_nulls,s.sum,s.min,s.max)
        AND ROW(r.rows,r.non_nulls,r.sum,r.min,r.max) IS NOT DISTINCT FROM ROW(n.rows,n.non_nulls,n.sum,n.min,n.max)
        AND ROW(f.rows,f.non_nulls,f.sum,f.min,f.max) IS NOT DISTINCT FROM ROW(b.rows,b.non_nulls,b.sum,b.min,b.max)
        FROM syntra_capture_int4('syntra_check_source'::regclass, 'balance') AS c,
        LATERAL syntra_segment_summary(c.segment) AS s,
        syntra_heap_int4_summary('syntra_check_source','balance') AS r,
        syntra_heap_int4_range_summary('syntra_check_source','balance',-2147483648,100) AS f,
        (SELECT count(*) AS rows,count(balance) AS non_nulls,sum(balance),min(balance),max(balance) FROM syntra_check_source) AS n,
        (SELECT count(*) AS rows,count(balance) AS non_nulls,sum(balance),min(balance),max(balance) FROM syntra_check_source WHERE balance <= 100) AS b LIMIT 1"#,
        &[],
    )?;
    require(
        row.try_get(3)?,
        "fresh Rust heap aggregate disagreed with native SQL at the same snapshot",
    )?;
    Ok((row.try_get(0)?, row.try_get(1)?, row.try_get(2)?))
}

fn require(condition: bool, message: &'static str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

pub(super) fn snapshot_checks(cluster: &Cluster, reader: &mut Client) -> Result<[&'static str; 7]> {
    let mut writer = cluster.connect("syntra_enabled")?;
    reader.batch_execute("CREATE TABLE syntra_check_source (id int4 PRIMARY KEY, balance int4); INSERT INTO syntra_check_source (id, balance) VALUES (1, 100), (2, 100)")?;
    reader.batch_execute("BEGIN ISOLATION LEVEL REPEATABLE READ")?;
    require(
        state(reader)? == (2, Some(200), Some(100)),
        "initial snapshot capture failed",
    )?;
    writer.batch_execute("UPDATE syntra_check_source SET balance = CASE id WHEN 1 THEN 90 ELSE 110 END WHERE id IN (1, 2)")?;
    require(
        state(reader)? == (2, Some(200), Some(100)),
        "capture exposed commits newer than a repeatable-read snapshot",
    )?;
    reader.batch_execute("ROLLBACK")?;

    reader.batch_execute(
        "BEGIN; UPDATE syntra_check_source SET balance = balance + 1 WHERE id = 1",
    )?;
    require(
        state(reader)? == (2, Some(201), Some(91)),
        "capture missed the caller's own writes",
    )?;
    let own_xid: Option<String> = reader.query_one("SELECT source_transaction FROM syntra_capture_int4('syntra_check_source'::regclass, 'balance') LIMIT 1", &[])?.try_get(0)?;
    require(
        own_xid.is_some(),
        "capture omitted its assigned transaction provenance",
    )?;
    reader.batch_execute("ROLLBACK")?;

    reader.batch_execute("BEGIN ISOLATION LEVEL READ COMMITTED")?;
    require(
        state(reader)? == (2, Some(200), Some(90)),
        "read-committed initial capture failed",
    )?;
    writer.batch_execute("UPDATE syntra_check_source SET balance = CASE id WHEN 1 THEN 80 ELSE 120 END WHERE id IN (1, 2)")?;
    require(
        state(reader)? == (2, Some(200), Some(80)),
        "read-committed capture missed the next statement snapshot",
    )?;
    reader.batch_execute("COMMIT")?;

    reader.batch_execute("CREATE TABLE syntra_check_projection AS SELECT 1::bigint AS generation, segment, source_snapshot FROM syntra_capture_int4('syntra_check_source'::regclass, 'balance')")?;
    writer.batch_execute("UPDATE syntra_check_source SET balance = 70 WHERE id = 1")?;
    reader.batch_execute("BEGIN; UPDATE syntra_check_projection AS p SET generation = p.generation + 1, segment = c.segment, source_snapshot = c.source_snapshot FROM syntra_capture_int4('syntra_check_source'::regclass, 'balance') AS c WHERE p.generation = 1")?;
    let updated: bool = reader.query_one("SELECT p.generation = 2 AND s.sum = 190 FROM syntra_check_projection AS p, LATERAL syntra_segment_summary(p.segment) AS s LIMIT 1", &[])?.try_get(0)?;
    require(
        updated,
        "projection refresh did not replace data and generation together",
    )?;
    reader.batch_execute("ROLLBACK")?;
    let restored: bool = reader.query_one("SELECT p.generation = 1 AND s.sum = 200 FROM syntra_check_projection AS p, LATERAL syntra_segment_summary(p.segment) AS s LIMIT 1", &[])?.try_get(0)?;
    require(restored, "projection refresh did not roll back atomically")?;
    reader.batch_execute("BEGIN READ ONLY")?;
    require(
        state(reader)? == (2, Some(190), Some(70)),
        "read-only fresh aggregate failed",
    )?;
    let no_xid: bool = reader
        .query_one("SELECT pg_current_xact_id_if_assigned() IS NULL", &[])?
        .try_get(0)?;
    require(no_xid, "read-only aggregate assigned a transaction ID")?;
    reader.batch_execute("COMMIT; BEGIN ISOLATION LEVEL SERIALIZABLE")?;
    let rejected = reader
        .query_one(
            "SELECT rows FROM syntra_heap_int4_summary('syntra_check_source','balance') LIMIT 1",
            &[],
        )
        .err()
        .ok_or("serializable fresh scan unexpectedly succeeded")?;
    require(
        rejected
            .as_db_error()
            .is_some_and(|error| error.code().code() == "0A000"),
        "serializable fresh scan returned an unexpected SQLSTATE",
    )?;
    reader.batch_execute("ROLLBACK")?;
    reader.batch_execute("DROP TABLE syntra_check_projection; DROP TABLE syntra_check_source")?;
    writer.batch_execute(
        "CREATE TABLE fresh_lock_probe (v int4); INSERT INTO fresh_lock_probe (v) VALUES (1)",
    )?;
    let source_oid: u32 = writer
        .query_one("SELECT 'fresh_lock_probe'::regclass::oid", &[])?
        .try_get(0)?;
    reader.batch_execute("BEGIN READ ONLY; SET LOCAL plan_cache_mode = force_generic_plan")?;
    let sum: i64 = reader
        .query_one(
            "SELECT sum FROM syntra_heap_int4_summary($1::oid::regclass,'v') LIMIT 1",
            &[&source_oid],
        )?
        .try_get(0)?;
    require(sum == 1, "lock probe returned an unexpected aggregate")?;
    let locked = writer
        .batch_execute("BEGIN; LOCK TABLE fresh_lock_probe IN ACCESS EXCLUSIVE MODE NOWAIT")
        .err()
        .ok_or("fresh aggregate released its source lock before transaction end")?;
    require(
        locked
            .as_db_error()
            .is_some_and(|e| e.code().code() == "55P03"),
        "source lock probe failed for an unexpected reason",
    )?;
    writer.batch_execute("ROLLBACK")?;
    reader.batch_execute("COMMIT")?;
    writer.batch_execute("BEGIN; LOCK TABLE fresh_lock_probe IN ACCESS EXCLUSIVE MODE NOWAIT; ROLLBACK; DROP TABLE fresh_lock_probe")?;
    Ok([
        "repeatable_read",
        "read_own_writes",
        "read_committed",
        "atomic_refresh_rollback",
        "fresh_heap_read_only_no_xid",
        "fresh_heap_serializable_rejected",
        "fresh_heap_transaction_lock_retention",
    ])
}

fn projection_state(client: &mut Client) -> Result<(i64, i64, Option<i64>, Option<i32>)> {
    let row = client.query_one("SELECT generation, rows, sum, min FROM syntra_fixed_projection_summary('accounts') LIMIT 1", &[])?;
    Ok((
        row.try_get(0)?,
        row.try_get(1)?,
        row.try_get(2)?,
        row.try_get(3)?,
    ))
}

fn tracked_generation(client: &mut Client) -> Result<i64> {
    Ok(client.query_one("SELECT generation FROM syntra_source_tracking_status('recovery_accounts') WHERE guard_config_valid LIMIT 1", &[])?.try_get(0)?)
}

fn trusted_state(client: &mut Client, route: &str, min: i32) -> Result<()> {
    let row = client.query_one("SELECT rows,non_nulls,sum,min,max,route FROM syntra_live_int4_summary('recovery_accounts','balance') LIMIT 1", &[])?;
    require(
        row.try_get::<_, i64>(0)? == 2
            && row.try_get::<_, i64>(1)? == 2
            && row.try_get::<_, Option<i64>>(2)? == Some(200)
            && row.try_get::<_, Option<i32>>(3)? == Some(min)
            && row.try_get::<_, Option<i32>>(4)? == Some(200 - min)
            && row.try_get::<_, &str>(5)? == route,
        "trusted recovery aggregate or route mismatch",
    )
}

pub(super) fn recovery_checks(cluster: &mut Cluster) -> Result<Value> {
    eprintln!("Recovery checks: oversized capture, committed publication, crash rollback and backup restore");
    let mut reader = cluster.connect("syntra_enabled")?;
    reader.batch_execute("CREATE TABLE recovery_limit_source (value int4); INSERT INTO recovery_limit_source (value) SELECT 1 FROM generate_series(1, 1000001)")?;
    let limit = reader.query_one("SELECT source_rows FROM syntra_capture_int4('recovery_limit_source'::regclass, 'value') LIMIT 1", &[])
        .err().ok_or("oversized source capture unexpectedly succeeded")?;
    require(
        limit
            .as_db_error()
            .is_some_and(|e| e.code().code() == "54000"),
        "oversized capture returned the wrong SQLSTATE",
    )?;
    let rows: i64 = reader
        .query_one("SELECT count(*) FROM recovery_limit_source", &[])?
        .try_get(0)?;
    require(rows == 1_000_001, "failed capture damaged its source")?;
    reader.batch_execute("DROP TABLE recovery_limit_source; CREATE TABLE recovery_accounts (id int4 PRIMARY KEY, balance int4); INSERT INTO recovery_accounts (id, balance) VALUES (1, 100), (2, 100)")?;
    reader.batch_execute("SELECT syntra_track_source('recovery_accounts', 'balance')")?;
    reader.batch_execute("SELECT syntra_publish_trusted_int4('recovery_accounts','balance')")?;
    reader.batch_execute("SELECT syntra_publish_fixed_projection('accounts', 'recovery_accounts', 'balance'); CHECKPOINT")?;
    reader.batch_execute("BEGIN ISOLATION LEVEL REPEATABLE READ")?;
    require(
        tracked_generation(&mut reader)? == 0,
        "initial source tracking generation failed",
    )?;
    require(
        projection_state(&mut reader)? == (1, 2, Some(200), Some(100)),
        "managed projection initial generation failed",
    )?;
    let mut writer = cluster.connect("syntra_enabled")?;
    writer.batch_execute("BEGIN; UPDATE recovery_accounts SET balance = 90 WHERE id = 1; UPDATE recovery_accounts SET balance = 110 WHERE id = 2; SELECT syntra_publish_fixed_projection('accounts', 'recovery_accounts', 'balance'); SELECT syntra_publish_trusted_int4('recovery_accounts','balance'); COMMIT")?;
    trusted_state(&mut reader, "projection", 100)?;
    require(
        projection_state(&mut reader)? == (1, 2, Some(200), Some(100)),
        "managed projection exposed a future generation in repeatable read",
    )?;
    require(
        tracked_generation(&mut reader)? == 0,
        "source tracking exposed a future commit in repeatable read",
    )?;
    reader.batch_execute("COMMIT")?;
    require(
        tracked_generation(&mut reader)? == 2,
        "committed source tracking generation failed",
    )?;
    require(
        projection_state(&mut reader)? == (2, 2, Some(200), Some(90)),
        "committed projection publication failed",
    )?;
    writer.batch_execute("BEGIN; UPDATE recovery_accounts SET balance = 80 WHERE id = 1; UPDATE recovery_accounts SET balance = 120 WHERE id = 2; SELECT syntra_publish_fixed_projection('accounts', 'recovery_accounts', 'balance'); SELECT syntra_publish_trusted_int4('recovery_accounts','balance')")?;
    trusted_state(&mut writer, "heap", 80)?;
    trusted_state(&mut reader, "projection", 90)?;
    require(
        projection_state(&mut writer)? == (3, 2, Some(200), Some(80)),
        "uncommitted refresh was not exercised",
    )?;
    require(
        tracked_generation(&mut writer)? == 4 && tracked_generation(&mut reader)? == 2,
        "source tracking exposed or missed uncommitted mutations",
    )?;
    require(
        projection_state(&mut reader)? == (2, 2, Some(200), Some(90)),
        "reader saw an uncommitted refresh",
    )?;
    // Leave writer's transaction open: dropping it first would only test rollback,
    // not recovery of uncommitted heap and projection changes after a crash.
    cluster.crash_restart()?;
    drop(writer);
    drop(reader);
    let mut recovered = cluster.connect("syntra_enabled")?;
    trusted_state(&mut recovered, "projection", 90)?;
    require(
        tracked_generation(&mut recovered)? == 2,
        "source tracking did not recover its committed generation",
    )?;
    require(
        projection_state(&mut recovered)? == (2, 2, Some(200), Some(90)),
        "crash exposed or lost a projection generation",
    )?;
    let balance: i32 = recovered
        .query_one(
            "SELECT balance FROM recovery_accounts WHERE id = 1 LIMIT 1",
            &[],
        )?
        .try_get(0)?;
    require(
        balance == 90,
        "crash did not preserve committed source state",
    )?;
    let original_database: u32 = recovered
        .query_one(
            "SELECT oid FROM pg_database WHERE datname = current_database() LIMIT 1",
            &[],
        )?
        .try_get(0)?;
    let dump = cluster.root_path().join("projection-backup.dump");
    crate::command(
        Command::new(cluster.pg_bin.join("pg_dump"))
            .arg("-h")
            .arg(cluster.root_path())
            .args([
                "-p",
                "28918",
                "-U",
                "syntra_bench",
                "-Fc",
                "-d",
                "syntra_enabled",
                "-f",
            ])
            .arg(&dump),
    )?;
    recovered.batch_execute("CREATE DATABASE syntra_restored TEMPLATE template0")?;
    crate::command(
        Command::new(cluster.pg_bin.join("pg_restore"))
            .arg("-h")
            .arg(cluster.root_path())
            .args([
                "-p",
                "28918",
                "-U",
                "syntra_bench",
                "--exit-on-error",
                "-d",
                "syntra_restored",
            ])
            .arg(&dump),
    )?;
    let mut restored = cluster.connect("syntra_restored")?;
    require(
        restored
            .query_one(
                "SELECT count(*)=0 FROM syntra.trusted_int4_projections",
                &[],
            )?
            .try_get(0)?,
        "logical restore retained trusted eligibility bindings",
    )?;
    trusted_state(&mut restored, "heap", 90)?;
    require(
        projection_state(&mut restored)? == (2, 2, Some(200), Some(90)),
        "backup restore changed projection contents",
    )?;
    let provenance: u32 = restored
        .query_one(
            "SELECT source_database FROM syntra.fixed_projections WHERE owner_oid = CURRENT_USER::regrole::oid AND name = 'accounts' LIMIT 1",
            &[],
        )?
        .try_get(0)?;
    let restored_database: u32 = restored
        .query_one(
            "SELECT oid FROM pg_database WHERE datname = current_database() LIMIT 1",
            &[],
        )?
        .try_get(0)?;
    require(
        provenance == original_database && provenance != restored_database,
        "restore silently relabeled historical provenance",
    )?;
    let rejected = restored
        .execute(
            "UPDATE recovery_accounts SET balance = 89 WHERE id = 1",
            &[],
        )
        .err()
        .ok_or("restored source accepted writes without tracking rebind")?;
    require(
        rejected.as_db_error().is_some_and(|e| {
            e.message()
                .contains("source tracking registration is missing")
        }),
        "restored source rejected writes for an unexpected reason",
    )?;
    restored.batch_execute("SELECT syntra_track_source('recovery_accounts', 'balance'); UPDATE recovery_accounts SET balance = 89 WHERE id = 1")?;
    require(
        tracked_generation(&mut restored)? == 1,
        "restored source tracking failed after explicit rebind",
    )?;
    require(
        projection_state(&mut restored)? == (2, 2, Some(200), Some(90)),
        "restored fixed copy changed after a source write",
    )?;
    // TRUNCATE replaces storage without preserving old heap versions. An old
    // snapshot can still see the old counter while its first heap read is empty.
    // Preserve this counterexample before any future cache eligibility work.
    recovered.batch_execute("CREATE TABLE tracking_truncate_source (value int4); INSERT INTO tracking_truncate_source (value) VALUES (1), (2); SELECT syntra_track_source('tracking_truncate_source', 'value'); SELECT syntra_publish_fixed_projection('truncate-proof', 'tracking_truncate_source', 'value')")?;
    let old_file: u32 = recovered.query_one("SELECT storage_file FROM syntra_source_identity('tracking_truncate_source', 'value') LIMIT 1", &[])?.try_get(0)?;
    let mut old_snapshot = cluster.connect("syntra_enabled")?;
    old_snapshot.batch_execute("BEGIN ISOLATION LEVEL REPEATABLE READ")?;
    let before: i64 = old_snapshot.query_one("SELECT generation FROM syntra_source_tracking_status('tracking_truncate_source') LIMIT 1", &[])?.try_get(0)?;
    require(
        before == 0,
        "truncate proof did not pin its initial snapshot",
    )?;
    recovered.batch_execute("TRUNCATE tracking_truncate_source")?;
    let new_file: u32 = old_snapshot.query_one("SELECT storage_file FROM syntra_source_identity('tracking_truncate_source', 'value') LIMIT 1", &[])?.try_get(0)?;
    require(
        new_file != old_file,
        "locked native storage identity missed a rewrite under an old snapshot",
    )?;
    let mismatch: bool = old_snapshot.query_one("SELECT (SELECT generation FROM syntra_source_tracking_status('tracking_truncate_source') LIMIT 1) = 0 AND (SELECT rows FROM syntra_fixed_projection_summary('truncate-proof') LIMIT 1) = 2 AND (SELECT count(*) FROM tracking_truncate_source) = 0", &[])?.try_get(0)?;
    require(mismatch, "truncate counterexample was not exercised")?;
    old_snapshot.batch_execute("COMMIT")?;
    recovered.batch_execute("SELECT syntra_delete_fixed_projection('truncate-proof'); DELETE FROM syntra.tracked_sources WHERE source_relation = 'tracking_truncate_source'::regclass::oid; DROP TABLE tracking_truncate_source")?;
    let mut maintenance = cluster.connect("syntra_enabled")?;
    maintenance.batch_execute("BEGIN READ ONLY; REINDEX TABLE recovery_accounts; COMMIT")?;
    require(
        tracked_generation(&mut recovered)? == 2,
        "read-only index maintenance changed the source counter",
    )?;
    recovered.batch_execute("CREATE DATABASE syntra_extension_drop_probe TEMPLATE template0")?;
    let mut removal = cluster.connect("syntra_extension_drop_probe")?;
    removal.batch_execute("CREATE EXTENSION syntradb; CREATE TABLE removal_source (id int4 PRIMARY KEY,v int4); SELECT syntra_seed_capture('removal_source','id','v',4); SELECT syntra_track_source('removal_source', 'v'); DROP EXTENSION syntradb CASCADE; INSERT INTO removal_source (id,v) VALUES (1,7); CREATE TABLE removal_followup (v int4)")?;
    let removed: bool = removal.query_one("SELECT NOT EXISTS (SELECT oid FROM pg_extension WHERE extname = 'syntradb') AND (SELECT v FROM removal_source LIMIT 1) = 7 AND NOT EXISTS (SELECT oid FROM pg_event_trigger WHERE evtname IN ('syntra_ddl_epoch_guard', 'syntra_ddl_drop_guard'))", &[])?.try_get(0)?;
    require(
        removed,
        "extension removal left active guards or blocked source writes",
    )?;
    Ok(
        json!({ "trusted_publication_crash_rollback": true, "trusted_restore_requires_republication": true,
        "read_only_index_maintenance": true, "extension_removal_released_guards": true,
        "locked_storage_identity_detects_truncate_under_old_snapshot": true,
    "truncate_counter_equality_is_insufficient": true,
        "source_tracking_crash_rollback": true, "source_tracking_restore_requires_rebind": true,
        "managed_projection_repeatable_read": true, "oversized_capture_rejected_with_54000": true, "source_unchanged_after_error": true,
        "committed_generation_survived_immediate_shutdown": true, "uncommitted_generation_discarded": true,
        "reader_never_saw_uncommitted_refresh": true, "backup_restore_preserved_fixed_copy": true,
        "historical_source_database_retained": true,
        "scope": "isolated process-crash and native dump/restore tests; no power-loss, disk-corruption or replica claim" }),
    )
}
