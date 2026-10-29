//! Committed checkpoint visibility, maintenance admission and crash recovery.
use crate::Cluster;
use postgres::Client;
use serde_json::{json, Value};
use syntra_bench::Result;
fn require(ok: bool, message: &'static str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(message.into())
    }
}
fn setup(client: &mut Client) -> Result<()> {
    client.batch_execute("CREATE TABLE checkpoint_check(id int4 PRIMARY KEY,v int4,w int4 DEFAULT 7); INSERT INTO checkpoint_check(id,v) VALUES(1,10),(2,NULL),(3,30)")?;
    reseed(client)
}
fn reseed(client: &mut Client) -> Result<()> {
    client.query_one(
        "SELECT syntra_seed_capture('checkpoint_check','id','v',8)",
        &[],
    )?;
    Ok(())
}
fn apply(client: &mut Client) -> Result<i64> {
    Ok(client
        .query_one("SELECT syntra_apply_capture('checkpoint_check')", &[])?
        .try_get(0)?)
}
fn prune(client: &mut Client) -> Result<i64> {
    Ok(client
        .query_one("SELECT syntra_prune_capture('checkpoint_check')", &[])?
        .try_get(0)?)
}
const COMBINED: &str = "SELECT applied,pruned FROM syntra_apply_prune_capture('checkpoint_check')";
fn combined(client: &mut Client) -> Result<(i64, i64)> {
    let row = client.query_one(COMBINED, &[])?;
    Ok((row.try_get(0)?, row.try_get(1)?))
}
fn read(client: &mut Client, route: &str, sum: i64) -> Result<()> {
    let row=client.query_one(r#"SELECT s.route,s.reason,s.sum,
        ROW(s.rows,s.non_nulls,s.sum,s.min,s.max) IS NOT DISTINCT FROM ROW(n.rows,n.non_nulls,n.sum,n.min,n.max)
        FROM syntra_checkpoint_int4_summary('checkpoint_check','v') AS s,
        (SELECT count(*) AS rows,count(v) AS non_nulls,sum(v),min(v),max(v) FROM checkpoint_check) AS n LIMIT 1"#,&[])?;
    let actual: &str = row.try_get(0)?;
    if actual != route {
        let reason: &str = row.try_get(1)?;
        return Err(
            format!("checkpoint route {actual}/{reason}; expected {route}, sum {sum}").into(),
        );
    }
    require(
        actual != "replay" || row.try_get::<_, &str>(1)? == "bounded_tail",
        "replay returned an invalid reason",
    )?;
    require(
        row.try_get::<_, Option<i64>>(2)? == Some(sum) && row.try_get::<_, bool>(3)?,
        "checkpoint aggregate disagreed with native snapshot",
    )
}
fn progress(client: &mut Client) -> Result<(i64, i64, i64)> {
    let row=client.query_one("SELECT c.applied,c.generation,h.pruned FROM syntra.capture_checkpoints AS c JOIN syntra.capture_heads AS h USING(source_relation,epoch) WHERE c.source_relation='checkpoint_check'::regclass LIMIT 1",&[])?;
    Ok((row.try_get(0)?, row.try_get(1)?, row.try_get(2)?))
}
pub(super) fn run(cluster: &Cluster) -> Result<Vec<&'static str>> {
    let mut reader = cluster.connect("syntra_enabled")?;
    let mut older = cluster.connect("syntra_enabled")?;
    let mut writer = cluster.connect("syntra_enabled")?;
    setup(&mut writer)?;
    read(&mut reader, "heap", 40)?;
    require(
        combined(&mut writer)? == (0, 0),
        "initial checkpoint progress mismatch",
    )?;
    read(&mut reader, "checkpoint", 40)?;
    reader.batch_execute("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY")?;
    read(&mut reader, "checkpoint", 40)?;
    writer.batch_execute("UPDATE checkpoint_check SET v=11 WHERE id=1")?;
    older.batch_execute("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY")?;
    read(&mut older, "heap", 41)?;
    require(
        combined(&mut writer)? == (1, 1),
        "checkpoint apply/prune did not advance",
    )?;
    read(&mut reader, "checkpoint", 40)?;
    read(&mut older, "heap", 41)?;
    require(older.query_one("SELECT count(*)=1 FROM syntra.capture_events WHERE source_relation='checkpoint_check'::regclass",&[])?.try_get(0)?,"pruning removed an old reader's visible event version")?;
    reader.batch_execute("COMMIT")?;
    older.batch_execute("COMMIT")?;
    read(&mut reader, "checkpoint", 41)?;
    writer.batch_execute("INSERT INTO checkpoint_check(id,v) VALUES(4,40); DELETE FROM checkpoint_check WHERE id=2; UPDATE checkpoint_check SET id=5,v=31 WHERE id=3")?;
    read(&mut reader, "heap", 82)?;
    writer.batch_execute("BEGIN")?;
    require(
        combined(&mut writer)? == (5, 4),
        "uncommitted checkpoint case not exercised",
    )?;
    require(
        progress(&mut writer)? == (5, 3, 5),
        "writer did not see its checkpoint progress",
    )?;
    require(
        progress(&mut reader)? == (1, 2, 1),
        "reader saw uncommitted checkpoint rows or cursor",
    )?;
    read(&mut writer, "heap", 82)?;
    read(&mut reader, "heap", 82)?;
    writer.batch_execute("ROLLBACK")?;
    require(
        progress(&mut reader)? == (1, 2, 1),
        "rollback retained checkpoint progress or pruning",
    )?;
    require(
        combined(&mut writer)? == (5, 4),
        "checkpoint retry after rollback failed",
    )?;
    read(&mut reader, "checkpoint", 82)?;
    let before=writer.query_one("SELECT ctid::text,xmin::text FROM syntra.capture_checkpoints WHERE source_relation='checkpoint_check'::regclass LIMIT 1",&[])?;
    writer.batch_execute("BEGIN")?;
    require(
        combined(&mut writer)? == (5, 0),
        "idempotent maintenance changed progress",
    )?;
    require(
        writer
            .query_one("SELECT pg_current_xact_id_if_assigned() IS NULL", &[])?
            .try_get(0)?,
        "idempotent checkpoint maintenance assigned an XID",
    )?;
    writer.batch_execute("COMMIT")?;
    let after=writer.query_one("SELECT ctid::text,xmin::text FROM syntra.capture_checkpoints WHERE source_relation='checkpoint_check'::regclass LIMIT 1",&[])?;
    require(
        before.try_get::<_, String>(0)? == after.try_get::<_, String>(0)?
            && before.try_get::<_, String>(1)? == after.try_get::<_, String>(1)?,
        "checkpoint retry rewrote its state",
    )?;

    writer.batch_execute("UPDATE syntra.capture_chunks SET segment=set_byte(segment,0,get_byte(segment,0) # 1) WHERE source_relation='checkpoint_check'::regclass")?;
    read(&mut reader, "heap", 82)?;
    let damaged = writer
        .query_one("SELECT syntra_prune_capture('checkpoint_check')", &[])
        .err()
        .ok_or("prune trusted a corrupt checkpoint")?;
    require(
        damaged.code().is_some_and(|c| c.code() == "55000"),
        "corrupt checkpoint prune had an unexpected SQLSTATE",
    )?;
    reseed(&mut writer)?;
    read(&mut reader, "heap", 82)?;

    // Initial publication holds a parent FK key-share lock through commit.
    // Ordinary captured writes must still acquire their non-key head update.
    writer.batch_execute("BEGIN")?;
    require(apply(&mut writer)? == 0, "new-epoch checkpoint seed failed")?;
    older.batch_execute("SET lock_timeout='250ms'; UPDATE checkpoint_check SET v=12 WHERE id=1")?;
    read(&mut reader, "heap", 83)?;
    writer.batch_execute("COMMIT")?;
    read(&mut reader, "heap", 83)?;
    require(
        apply(&mut writer)? == 1 && prune(&mut writer)? == 1,
        "checkpoint did not catch concurrent producer",
    )?;
    read(&mut reader, "checkpoint", 83)?;
    writer.batch_execute("BEGIN")?;
    apply(&mut writer)?;
    for sql in [
        "SELECT syntra_apply_capture('checkpoint_check')",
        "SELECT syntra_prune_capture('checkpoint_check')",
        COMBINED,
    ] {
        let busy = older
            .query_one(sql, &[])
            .err()
            .ok_or("checkpoint maintenance bypassed admission")?;
        require(
            busy.code().is_some_and(|c| c.code() == "55P03"),
            "checkpoint admission had an unexpected SQLSTATE",
        )?;
    }
    read(&mut reader, "checkpoint", 83)?;
    writer.batch_execute("ROLLBACK")?;
    reader.batch_execute("CREATE ROLE checkpoint_reader NOLOGIN; GRANT SELECT ON checkpoint_check TO checkpoint_reader; SET ROLE checkpoint_reader")?;
    read(&mut reader, "checkpoint", 83)?;
    let private = reader
        .query_one("SELECT count(*) FROM syntra.capture_checkpoints", &[])
        .err()
        .ok_or("ordinary reader accessed checkpoint catalog")?;
    require(
        private.code().is_some_and(|c| c.code() == "42501"),
        "checkpoint catalog denial had an unexpected SQLSTATE",
    )?;
    reader.batch_execute("RESET ROLE; REVOKE SELECT ON checkpoint_check FROM checkpoint_reader; SET ROLE checkpoint_reader")?;
    let denied = reader
        .query_one(
            "SELECT sum FROM syntra_checkpoint_int4_summary('checkpoint_check','v') LIMIT 1",
            &[],
        )
        .err()
        .ok_or("checkpoint read bypassed source ACL")?;
    require(
        denied.code().is_some_and(|c| c.code() == "42501"),
        "checkpoint source ACL denial had an unexpected SQLSTATE",
    )?;
    reader.batch_execute("RESET ROLE")?;
    let other:bool=reader.query_one("SELECT sum=21 AND route='heap' FROM syntra_checkpoint_int4_summary('checkpoint_check','w') LIMIT 1",&[])?.try_get(0)?;
    require(other, "checkpoint reused values for a different column")?;
    for begin in ["BEGIN READ ONLY", "BEGIN ISOLATION LEVEL REPEATABLE READ"] {
        reader.batch_execute(begin)?;
        let error = reader
            .query_one(COMBINED, &[])
            .err()
            .ok_or("combined maintenance accepted unsupported transaction")?;
        reader.batch_execute("ROLLBACK")?;
        require(
            error.code().is_some_and(|c| c.code() == "0A000"),
            "combined maintenance isolation denial had wrong SQLSTATE",
        )?;
    }
    writer.batch_execute("ALTER TABLE checkpoint_check DISABLE TRIGGER _syntra_capture_row; UPDATE checkpoint_check SET v=13 WHERE id=1; ALTER TABLE checkpoint_check ENABLE ALWAYS TRIGGER _syntra_capture_row")?;
    read(&mut reader, "heap", 84)?;
    let stale = writer
        .query_one("SELECT syntra_apply_capture('checkpoint_check')", &[])
        .err()
        .ok_or("checkpoint apply accepted a DDL gap")?;
    require(
        stale.code().is_some_and(|c| c.code() == "55000"),
        "checkpoint DDL denial had an unexpected SQLSTATE",
    )?;
    writer.batch_execute("SELECT syntra_stop_capture('checkpoint_check')")?;
    read(&mut reader, "heap", 84)?;
    reader.batch_execute("DROP TABLE checkpoint_check; DROP ROLE checkpoint_reader")?;
    combined_concurrent_cut(cluster)?;
    Ok(vec![
        "combined initial checkpoint and default lag fallback",
        "old checkpoint snapshots",
        "old event visibility after pruning",
        "atomic uncommitted rows and progress",
        "rollback and retry",
        "no-op maintenance without XID or tuple rewrite",
        "corrupt projection fallback and prune rejection",
        "reseed invalidation",
        "producer progress during checkpoint publication",
        "nonblocking maintenance admission",
        "ordinary reader and private catalog ACLs",
        "source column binding",
        "DDL gap and retirement fallback",
        "combined read-only and repeatable-read rejection",
        "combined captured cut with a concurrent producer",
    ])
}

pub(super) fn recovery(cluster: &mut Cluster) -> Result<Value> {
    let mut writer = cluster.connect("syntra_enabled")?;
    let mut reader = cluster.connect("syntra_enabled")?;
    setup(&mut writer)?;
    apply(&mut writer)?;
    writer.batch_execute("UPDATE checkpoint_check SET v=11 WHERE id=1")?;
    combined(&mut writer)?;
    require(
        progress(&mut reader)? == (1, 2, 1),
        "committed checkpoint recovery setup failed",
    )?;
    writer.batch_execute("UPDATE checkpoint_check SET v=12 WHERE id=1")?;
    writer.batch_execute("BEGIN")?;
    combined(&mut writer)?;
    require(
        progress(&mut writer)? == (2, 3, 2),
        "uncommitted checkpoint crash case was not exercised",
    )?;
    require(
        progress(&mut reader)? == (1, 2, 1),
        "checkpoint leaked before crash",
    )?;
    cluster.crash_restart()?;
    drop(writer);
    drop(reader);
    let mut recovered = cluster.connect("syntra_enabled")?;
    require(
        progress(&mut recovered)? == (1, 2, 1),
        "crash retained uncommitted checkpoint or lost committed progress",
    )?;
    read(&mut recovered, "heap", 42)?;
    require(
        combined(&mut recovered)? == (2, 1),
        "checkpoint replay retry failed after crash",
    )?;
    require(
        progress(&mut recovered)? == (2, 3, 2),
        "checkpoint replay advanced more than once",
    )?;
    read(&mut recovered, "checkpoint", 42)?;
    require(
        combined(&mut recovered)? == (2, 0) && progress(&mut recovered)? == (2, 3, 2),
        "checkpoint retry after recovery was not idempotent",
    )?;
    recovered.batch_execute("DROP TABLE checkpoint_check")?;
    Ok(
        json!({"combined_maintenance":true,"committed_checkpoint_survived":true,"uncommitted_apply_and_prune_rolled_back":true,
        "captured_source_change_survived":true,"retry_applied_exactly_once":true,
        "scope":"single-node process crash; no power-loss, physical restore or replica failover claim"}),
    )
}

fn combined_concurrent_cut(cluster: &Cluster) -> Result<()> {
    use std::time::{Duration, Instant};
    let mut admin = cluster.connect("syntra_enabled")?;
    let mut producer = cluster.connect("syntra_enabled")?;
    let mut older = cluster.connect("syntra_enabled")?;
    let mut maintenance = cluster.connect("syntra_enabled")?;
    admin.batch_execute("SET statement_timeout='15s'; CREATE TABLE combined_cut(id int4 PRIMARY KEY,v int4); INSERT INTO combined_cut VALUES(1,10); SELECT syntra_seed_capture('combined_cut','id','v',16); SELECT syntra_apply_capture('combined_cut')")?;
    older.batch_execute("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY")?;
    let old: i64 = older
        .query_one(
            "SELECT sum FROM syntra_checkpoint_int4_summary('combined_cut','v') LIMIT 1",
            &[],
        )?
        .try_get(0)?;
    producer.batch_execute("SET statement_timeout='15s'; SET lock_timeout='5s'; UPDATE combined_cut SET v=11 WHERE id=1")?;
    // Publication appends the partition delta; hold it there, mid-transaction.
    admin.batch_execute("CREATE FUNCTION combined_cut_gate() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.source_relation='combined_cut'::regclass THEN PERFORM pg_advisory_xact_lock(735190019::bigint); END IF; RETURN NEW; END $$; CREATE TRIGGER combined_cut_gate BEFORE INSERT ON syntra.capture_chunk_deltas FOR EACH ROW EXECUTE FUNCTION combined_cut_gate(); SELECT pg_advisory_lock(735190019::bigint)")?;
    maintenance.batch_execute("SET statement_timeout='15s'; SET lock_timeout='5s'")?;
    let pid: i32 = maintenance
        .query_one("SELECT pg_backend_pid()", &[])?
        .try_get(0)?;
    let result = std::thread::scope(|scope| -> Result<(i64, i64)> {
        let handle = scope.spawn(move || -> std::result::Result<(i64, i64), String> {
            let row = maintenance
                .query_one(
                    "SELECT applied,pruned FROM syntra_apply_prune_capture('combined_cut')",
                    &[],
                )
                .map_err(|e| e.to_string())?;
            Ok((
                row.try_get(0).map_err(|e| e.to_string())?,
                row.try_get(1).map_err(|e| e.to_string())?,
            ))
        });
        let observation = (|| -> Result<()> {
            let deadline = Instant::now() + Duration::from_secs(4);
            // unslop-ignore N_PLUS_ONE_IN_LOOP -- Observe the database lock condition, bounded by a deadline, rather than assuming a fixed delay.
            loop {
                let waiting: bool = admin.query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1 AND wait_event='advisory')", &[&pid])?.try_get(0)?;
                if waiting {
                    break;
                }
                require(
                    Instant::now() < deadline,
                    "combined publication never reached its deterministic gate",
                )?;
                std::thread::yield_now();
            }
            producer.batch_execute("UPDATE combined_cut SET v=12 WHERE id=1")?;
            Ok(())
        })();
        let unlocked = admin.query_one("SELECT pg_advisory_unlock(735190019::bigint)", &[]);
        let result = handle
            .join()
            .map_err(|_| "combined publication worker panicked")?;
        observation?;
        require(unlocked?.try_get(0)?, "combined gate was not held")?;
        result.map_err(Into::into)
    })?;
    require(
        result == (1, 1),
        "combined maintenance pruned beyond its captured cut",
    )?;
    let preserved: bool = admin.query_one("SELECT h.emitted=2 AND h.pruned=1 AND c.applied=1 AND (SELECT count(*) FROM syntra.capture_events WHERE source_relation=h.source_relation AND ordinal=2 AND before_value=11 AND after_value=12)=1 FROM (SELECT hh.source_relation,hh.epoch,hh.base_rows,hh.pruned,coalesce(m.ordinal,hh.emitted) AS emitted,coalesce(m.live_rows,hh.live_rows) AS live_rows FROM syntra.capture_heads AS hh LEFT JOIN LATERAL (SELECT ordinal,live_rows FROM syntra.capture_marks WHERE epoch=hh.epoch AND ordinal>hh.emitted ORDER BY ordinal DESC LIMIT 1) AS m ON true) AS h JOIN syntra.capture_checkpoints AS c USING(source_relation,epoch) WHERE h.source_relation='combined_cut'::regclass LIMIT 1", &[])?.try_get(0)?;
    require(
        preserved,
        "combined maintenance lost the later producer event",
    )?;
    require(old==10 && older.query_one("SELECT sum=10 AND route='checkpoint' FROM syntra_checkpoint_int4_summary('combined_cut','v') LIMIT 1", &[])?.try_get(0)?,"combined maintenance changed an old source snapshot")?;
    older.batch_execute("COMMIT")?;
    admin.batch_execute("DROP TRIGGER combined_cut_gate ON syntra.capture_chunk_deltas; DROP FUNCTION combined_cut_gate()")?;
    let final_row = admin.query_one(
        "SELECT applied,pruned FROM syntra_apply_prune_capture('combined_cut')",
        &[],
    )?;
    require(
        final_row.try_get::<_, i64>(0)? == 2 && final_row.try_get::<_, i64>(1)? == 1,
        "combined retry failed to consume the later event exactly once",
    )?;
    require(admin.query_one("SELECT sum=12 AND route='checkpoint' FROM syntra_checkpoint_int4_summary('combined_cut','v') LIMIT 1", &[])?.try_get(0)?,"combined final summary did not match the source")?;
    admin.batch_execute("DROP TABLE combined_cut")?;
    Ok(())
}
