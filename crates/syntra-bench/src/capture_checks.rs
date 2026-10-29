//! Committed capture checks, including a writer that commits during seed admission.
use crate::Cluster;
use postgres::Client;
use serde_json::{json, Value};
use std::{
    process::Command,
    thread,
    time::{Duration, Instant},
};
use syntra_bench::Result;
use syntra_storage::replay::{Batch, Binding, Limits};
// These checks capture int4 feeds; the aliases pin the kernel's int4 layout.
type Change = syntra_storage::replay::Change;
type ReplayState = syntra_storage::replay::ReplayState;
type Row = syntra_storage::replay::Row;

fn require(ok: bool, message: &'static str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(message.into())
    }
}
fn seed(client: &mut Client) -> Result<i64> {
    Ok(client
        .query_one(
            "SELECT syntra_seed_capture('capture_check','id','v',32)",
            &[],
        )?
        .try_get(0)?)
}

/// Caller pins a transaction snapshot. Reconstruct the visible prefix from a
/// fresh kernel state; this deliberately does not claim durable incremental apply.
fn verify(client: &mut Client) -> Result<usize> {
    let head=client.query_one("SELECT epoch,emitted,base_rows FROM (SELECT hh.source_relation,hh.epoch,hh.base_rows,hh.pruned,coalesce(m.ordinal,hh.emitted) AS emitted,coalesce(m.live_rows,hh.live_rows) AS live_rows FROM syntra.capture_heads AS hh LEFT JOIN LATERAL (SELECT ordinal,live_rows FROM syntra.capture_marks WHERE epoch=hh.epoch AND ordinal>hh.emitted ORDER BY ordinal DESC LIMIT 1) AS m ON true) AS h WHERE source_relation='capture_check'::regclass LIMIT 1",&[])?;
    let epoch: i64 = head.try_get(0)?;
    let emitted: i64 = head.try_get(1)?;
    let base=client.query("SELECT key::int4,value::int4 FROM syntra.capture_base_rows WHERE epoch=$1 ORDER BY key LIMIT 100001",&[&epoch])?
        .into_iter().map(|r|Ok(Row{key:r.try_get(0)?,value:r.try_get(1)?})).collect::<Result<Vec<_>>>()?;
    require(
        base.len() as i64 == head.try_get::<_, i64>(2)?,
        "capture base row count mismatch",
    )?;
    let events=client.query("SELECT ordinal,key::int4,before_present,before_value::int4,after_present,after_value::int4,origin_xid::text FROM syntra.capture_events WHERE epoch=$1 ORDER BY ordinal LIMIT 4097",&[&epoch])?;
    require(
        events.len() as i64 == emitted,
        "capture cursor and visible event prefix disagree",
    )?;
    let mut changes = Vec::new();
    for (index, row) in events.iter().enumerate() {
        require(
            row.try_get::<_, i64>(0)? == index as i64 + 1,
            "capture event prefix contains a gap",
        )?;
        let before: bool = row.try_get(2)?;
        let after: bool = row.try_get(4)?;
        let old: Option<i32> = row.try_get(3)?;
        let new: Option<i32> = row.try_get(5)?;
        require(
            (before || old.is_none()) && (after || new.is_none()),
            "capture absent state contains a value",
        )?;
        let xid: &str = row.try_get(6)?;
        require(xid != "0", "capture omitted origin transaction")?;
        changes.push(Change {
            key: row.try_get(1)?,
            before: before.then_some(old),
            after: after.then_some(new),
        });
    }
    let binding = Binding {
        stream: [19; 32],
        epoch: epoch.try_into()?,
    };
    let mut replay = ReplayState::seed(
        binding,
        0,
        &base,
        Limits {
            rows: 104096,
            changes: 4096,
            buffer_bytes: 8 * 1024 * 1024,
        },
        || true,
    )?;
    replay.apply(
        Batch {
            binding,
            previous: 0,
            next: 1,
            changes: &changes,
        },
        || true,
    )?;
    let expected = client
        .query(
            "SELECT id,v FROM capture_check ORDER BY id LIMIT 104097",
            &[],
        )?
        .into_iter()
        .map(|r| {
            Ok(Row {
                key: r.try_get(0)?,
                value: r.try_get(1)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    require(
        replay.rows() == expected,
        "captured base and visible changes disagree with native source rows",
    )?;
    Ok(changes.len())
}
fn snapshot_verify(client: &mut Client) -> Result<usize> {
    client.batch_execute("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY")?;
    let count = verify(client)?;
    client.batch_execute("COMMIT")?;
    Ok(count)
}

pub(super) fn run(cluster: &Cluster) -> Result<Vec<&'static str>> {
    let mut reader = cluster.connect("syntra_enabled")?;
    let mut writer = cluster.connect("syntra_enabled")?;
    reader.batch_execute("CREATE TABLE capture_check(id int4 PRIMARY KEY,v int4); INSERT INTO capture_check(id,v) VALUES(1,10),(2,NULL),(3,30)")?;
    writer.batch_execute("BEGIN; UPDATE capture_check SET v=11 WHERE id=1")?;
    let xid: String = writer
        .query_one("SELECT pg_current_xact_id()::text", &[])?
        .try_get(0)?;
    let source: u32 = reader
        .query_one("SELECT 'capture_check'::regclass::oid", &[])?
        .try_get(0)?;
    let mut seeder = cluster.connect("syntra_enabled")?;
    seeder.batch_execute("SET statement_timeout='10s'; SET lock_timeout='8s'")?;
    let pid: i32 = seeder
        .query_one("SELECT pg_backend_pid()", &[])?
        .try_get(0)?;
    let handle = thread::spawn(move || seed(&mut seeder).map_err(|e| e.to_string()));
    let wait = (|| -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(5);
        // unslop-ignore N_PLUS_ONE_IN_LOOP -- Poll a deterministic lock-admission condition; source data is never read here and no sleeps establish ordering.
        while Instant::now() < deadline {
            let waiting:bool=reader.query_one("SELECT EXISTS(SELECT pid FROM pg_locks WHERE pid=$1 AND relation=$2::oid AND mode='AccessExclusiveLock' AND NOT granted)",&[&pid,&source])?.try_get(0)?;
            if waiting {
                return Ok(());
            }
        }
        Err("capture seed did not wait for the source writer".into())
    })();
    let release = writer.batch_execute(if wait.is_ok() { "COMMIT" } else { "ROLLBACK" });
    let seeded = handle.join().map_err(|_| "capture seed worker panicked")?;
    wait?;
    release?;
    seeded.map_err(std::io::Error::other)?;
    require(
        snapshot_verify(&mut reader)? == 0,
        "capture seed logged pre-boundary changes",
    )?;
    let included:bool=reader.query_one("SELECT pg_visible_in_snapshot($1::text::xid8,base_snapshot::pg_snapshot) FROM syntra.capture_heads WHERE source_relation='capture_check'::regclass LIMIT 1",&[&xid])?.try_get(0)?;
    require(
        included,
        "capture base snapshot omitted the writer that committed during admission",
    )?;

    reader.batch_execute("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY")?;
    require(
        verify(&mut reader)? == 0,
        "initial repeatable capture prefix was not empty",
    )?;
    writer.batch_execute("UPDATE capture_check SET id=4,v=NULL WHERE id=1; INSERT INTO capture_check(id,v) VALUES(5,50); DELETE FROM capture_check WHERE id=2")?;
    require(
        verify(&mut reader)? == 0,
        "old capture snapshot exposed future events",
    )?;
    reader.batch_execute("COMMIT")?;
    require(
        snapshot_verify(&mut reader)? == 4,
        "capture did not preserve key move, insert and delete",
    )?;
    writer.batch_execute("BEGIN; UPDATE capture_check SET v=51 WHERE id=5")?;
    // The row trigger is deferred: the change has no event until it fires.
    let queued: i64 = writer
        .query_one(
            "SELECT count(*) FROM syntra.capture_events WHERE source_relation='capture_check'::regclass",
            &[],
        )?
        .try_get(0)?;
    require(
        queued == 4,
        "deferred capture wrote an event before it fired",
    )?;
    writer.batch_execute("SET CONSTRAINTS ALL IMMEDIATE")?;
    require(
        verify(&mut writer)? == 5,
        "capture missed own uncommitted change",
    )?;
    require(
        snapshot_verify(&mut reader)? == 4,
        "capture exposed an uncommitted change",
    )?;
    writer.batch_execute("ROLLBACK")?;
    require(
        snapshot_verify(&mut reader)? == 4,
        "capture rollback left reserved events",
    )?;
    writer.batch_execute("BEGIN; SAVEPOINT capture_save; UPDATE capture_check SET v=52 WHERE id=5; ROLLBACK TO capture_save; COMMIT")?;
    require(
        snapshot_verify(&mut reader)? == 4,
        "savepoint rollback retained capture events",
    )?;

    // Allocate the second writer's XID first, but commit it after the first.
    // Head order must follow the dependency, not numeric transaction order.
    let mut contender = cluster.connect("syntra_enabled")?;
    contender.batch_execute("SET lock_timeout='10s'; SET statement_timeout='15s'; BEGIN")?;
    let older: String = contender
        .query_one("SELECT pg_current_xact_id()::text", &[])?
        .try_get(0)?;
    let contender_pid: i32 = contender
        .query_one("SELECT pg_backend_pid()", &[])?
        .try_get(0)?;
    reader.batch_execute("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY")?;
    require(
        verify(&mut reader)? == 4,
        "concurrent capture initial prefix mismatch",
    )?;
    writer.batch_execute("BEGIN; UPDATE capture_check SET v=31 WHERE id=3")?;
    let pending = thread::spawn(move || -> std::result::Result<(), String> {
        contender.batch_execute("UPDATE capture_check SET v=51 WHERE id=5; UPDATE capture_check SET v=32 WHERE id=3; COMMIT").map_err(|e|e.to_string())
    });
    let waited = (|| -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(5);
        // unslop-ignore N_PLUS_ONE_IN_LOOP -- Observe real capture-head contention before releasing the first writer; this is test synchronization.
        while Instant::now() < deadline {
            let waiting: bool = reader
                .query_one(
                    "SELECT EXISTS(SELECT pid FROM pg_locks WHERE pid=$1 AND NOT granted)",
                    &[&contender_pid],
                )?
                .try_get(0)?;
            if waiting {
                return Ok(());
            }
        }
        Err("second capture writer did not wait on the head cursor".into())
    })();
    let release = writer.batch_execute(if waited.is_ok() { "COMMIT" } else { "ROLLBACK" });
    let finished = pending.join().map_err(|_| "capture writer panicked")?;
    waited?;
    release?;
    finished.map_err(std::io::Error::other)?;
    require(
        verify(&mut reader)? == 4,
        "old capture snapshot saw a newly committed prefix",
    )?;
    reader.batch_execute("COMMIT")?;
    require(
        snapshot_verify(&mut reader)? == 7,
        "concurrent capture writers lost ordered changes",
    )?;
    let order=reader.query_one("SELECT (SELECT origin_xid::text FROM syntra.capture_events WHERE source_relation='capture_check'::regclass AND ordinal=5 LIMIT 1),(SELECT origin_xid::text FROM syntra.capture_events WHERE source_relation='capture_check'::regclass AND ordinal=6 LIMIT 1)",&[])?;
    let first: String = order.try_get(0)?;
    let second: String = order.try_get(1)?;
    require(
        second == older && first.parse::<u64>()? > second.parse::<u64>()?,
        "capture ordering case did not reverse numeric XID order",
    )?;

    // Pin only catalog state, so source DDL can commit before this transaction's
    // first source write. Configuration validation must use current metadata.
    reader.batch_execute("BEGIN ISOLATION LEVEL REPEATABLE READ")?;
    reader.query_one("SELECT epoch FROM syntra.capture_heads WHERE source_relation='capture_check'::regclass LIMIT 1",&[])?;
    let reader_xid: String = reader
        .query_one("SELECT pg_current_xact_id()::text", &[])?
        .try_get(0)?;
    let reader_shard = reader_xid.parse::<u64>()? % 16;
    let mut admitted = false;
    // unslop-ignore N_PLUS_ONE_IN_LOOP -- Select distinct transaction-counter shards so an unrelated serialization error cannot hide a stale-configuration bug.
    for _ in 0..16 {
        writer.batch_execute("BEGIN")?;
        let xid: String = writer
            .query_one("SELECT pg_current_xact_id()::text", &[])?
            .try_get(0)?;
        if xid.parse::<u64>()? % 16 != reader_shard {
            admitted = true;
            break;
        }
        writer.batch_execute("ROLLBACK")?;
    }
    require(
        admitted,
        "cannot isolate the DDL configuration check from counter-shard conflicts",
    )?;
    writer.batch_execute("ALTER TABLE capture_check DISABLE TRIGGER _syntra_capture_row; UPDATE capture_check SET v=53 WHERE id=5; ALTER TABLE capture_check ENABLE ALWAYS TRIGGER _syntra_capture_row")?;
    writer.batch_execute("COMMIT")?;
    // The deferred row trigger raises the rejection at commit.
    let rejected = reader
        .batch_execute("UPDATE capture_check SET v=54 WHERE id=3; COMMIT")
        .err()
        .ok_or("old-snapshot capture accepted a DDL gap")?;
    require(
        rejected.code().is_some_and(|c| c.code() == "55000"),
        "DDL gap returned an unexpected error",
    )?;
    reader.batch_execute("ROLLBACK")?;
    seed(&mut writer)?;
    require(
        snapshot_verify(&mut reader)? == 0,
        "explicit reseed did not reset base and log",
    )?;
    let stopped: bool = reader
        .query_one("SELECT syntra_stop_capture('capture_check')", &[])?
        .try_get(0)?;
    require(
        stopped,
        "capture retirement did not remove its registration",
    )?;
    writer.batch_execute("UPDATE capture_check SET v=55 WHERE id=3")?;
    let retained:bool=reader.query_one("SELECT (SELECT v=55 FROM capture_check WHERE id=3) AND NOT EXISTS(SELECT epoch FROM syntra.capture_heads WHERE source_relation='capture_check'::regclass)",&[])?.try_get(0)?;
    require(
        retained,
        "capture retirement changed source behavior or retained registration",
    )?;
    reader.batch_execute("DROP TABLE capture_check")?;
    Ok(vec![
        "writer commits during seed lock admission",
        "fresh post-lock base snapshot",
        "native row reconstruction through replay kernel",
        "key moves and nullable values",
        "repeatable-read event prefix",
        "uncommitted isolation and own changes",
        "transaction and savepoint rollback",
        "concurrent commit prefixes independent of numeric XID order",
        "current DDL validation under an old snapshot",
        "explicit reseed",
        "explicit capture retirement preserves source writes",
    ])
}

pub(super) fn recovery(cluster: &mut Cluster) -> Result<Value> {
    let mut reader = cluster.connect("syntra_enabled")?;
    let mut writer = cluster.connect("syntra_enabled")?;
    reader.batch_execute("CREATE TABLE capture_check(id int4 PRIMARY KEY,v int4); INSERT INTO capture_check(id,v) VALUES(1,10),(2,NULL),(3,30)")?;
    seed(&mut reader)?;
    reader.batch_execute("UPDATE capture_check SET v=11 WHERE id=1; INSERT INTO capture_check(id,v) VALUES(4,40); CHECKPOINT")?;
    require(
        snapshot_verify(&mut reader)? == 2,
        "capture committed prefix setup failed",
    )?;
    writer.batch_execute(
        "BEGIN; SET CONSTRAINTS ALL IMMEDIATE; UPDATE capture_check SET v=12 WHERE id=1; DELETE FROM capture_check WHERE id=4",
    )?;
    require(
        verify(&mut writer)? == 4,
        "uncommitted capture crash case was not exercised",
    )?;
    require(
        snapshot_verify(&mut reader)? == 2,
        "reader saw uncommitted capture before crash",
    )?;
    cluster.crash_restart()?;
    drop(writer);
    drop(reader);
    let mut recovered = cluster.connect("syntra_enabled")?;
    require(
        snapshot_verify(&mut recovered)? == 2,
        "crash lost committed capture or retained aborted events",
    )?;
    require(
        recovered
            .query_one("SELECT syntra_apply_capture('capture_check')", &[])?
            .try_get::<_, i64>(0)?
            == 2,
        "checkpoint was not populated before logical backup",
    )?;
    let dump = cluster.root_path().join("capture-backup.dump");
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
    recovered.batch_execute("CREATE DATABASE capture_restored TEMPLATE template0")?;
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
                "capture_restored",
            ])
            .arg(&dump),
    )?;
    let mut restored = cluster.connect("capture_restored")?;
    let empty:bool=restored.query_one("SELECT (SELECT count(*) FROM syntra.capture_heads)=0 AND (SELECT count(*) FROM syntra.capture_base_rows)=0 AND (SELECT count(*) FROM syntra.capture_events)=0 AND (SELECT count(*) FROM syntra.capture_checkpoints)=0 AND (SELECT count(*) FROM syntra.capture_chunks)=0",&[])?.try_get(0)?;
    require(empty, "logical restore retained old capture history")?;
    let denied = restored
        .execute("UPDATE capture_check SET v=13 WHERE id=1", &[])
        .err()
        .ok_or("restored source wrote before capture reseed")?;
    require(
        denied.code().is_some_and(|c| c.code() == "55000"),
        "restore capture denial had an unexpected SQLSTATE",
    )?;
    seed(&mut restored)?;
    restored.batch_execute("UPDATE capture_check SET v=13 WHERE id=1")?;
    require(
        snapshot_verify(&mut restored)? == 1,
        "restored capture did not work after explicit reseed",
    )?;
    recovered.batch_execute("DROP TABLE capture_check")?;
    Ok(
        json!({"committed_base_and_events_survived":true,"aborted_source_and_events_discarded":true,
        "logical_restore_omitted_capture_history":true,"logical_restore_omitted_populated_checkpoint":true,"restore_requires_explicit_reseed":true,
        "scope":"capture durability, row reconstruction and checkpoint omission on logical restore; checkpoint crash recovery is checked separately"}),
    )
}
