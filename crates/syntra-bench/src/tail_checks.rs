//! Committed read-only tail replay against native SQL and persisted tuples.
use crate::Cluster;
use postgres::Client;
use serde_json::Value;
use syntra_bench::{correctness_values, AggregateValues, Result};
fn require(ok: bool, message: &'static str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(message.into())
    }
}
fn read(client: &mut Client, route: &str, reason: &str, expected: AggregateValues) -> Result<()> {
    let row=client.query_one("SELECT s.rows,s.non_nulls,s.sum,s.min,s.max,s.route,s.reason,ROW(s.rows,s.non_nulls,s.sum,s.min,s.max) IS NOT DISTINCT FROM ROW(n.rows,n.non_nulls,n.sum,n.min,n.max),pg_current_xact_id_if_assigned() IS NULL FROM syntra_checkpoint_int4_summary('tail_check','v',true) AS s,(SELECT count(*) AS rows,count(v) AS non_nulls,sum(v),min(v),max(v) FROM tail_check) AS n LIMIT 1",&[])?;
    let actual: &str = row.try_get(5)?;
    let why: &str = row.try_get(6)?;
    if actual != route || why != reason {
        return Err(format!("tail route {actual}/{why}; expected {route}/{reason}").into());
    }
    require(
        correctness_values(&row)? == expected && row.try_get::<_, bool>(7)?,
        "tail summary disagreed with native SQL or expected extrema",
    )?;
    require(row.try_get::<_, bool>(8)?, "tail read assigned an XID")
}
fn fingerprint(client: &mut Client) -> Result<Value> {
    Ok(client.query_one("SELECT json_build_object('head',ROW(h.emitted,h.pruned,h.live_rows)::text,'checkpoint',ROW(c.applied,c.generation,c.ctid::text,c.xmin::text)::text,'chunks',(SELECT json_agg(ROW(k.partition,k.revision,k.ctid::text,k.xmin::text)::text ORDER BY partition) FROM syntra.capture_chunks AS k WHERE k.source_relation=h.source_relation),'events',(SELECT json_agg(ROW(e.ordinal,e.ctid::text,e.xmin::text)::text ORDER BY ordinal) FROM syntra.capture_events AS e WHERE e.source_relation=h.source_relation)) FROM (SELECT hh.source_relation,hh.epoch,hh.base_rows,hh.pruned,coalesce(m.ordinal,hh.emitted) AS emitted,coalesce(m.live_rows,hh.live_rows) AS live_rows FROM syntra.capture_heads AS hh LEFT JOIN LATERAL (SELECT ordinal,live_rows FROM syntra.capture_marks WHERE epoch=hh.epoch AND ordinal>hh.emitted ORDER BY ordinal DESC LIMIT 1) AS m ON true) AS h JOIN syntra.capture_checkpoints AS c USING(source_relation,epoch) WHERE h.source_relation='tail_check'::regclass LIMIT 1",&[])?.try_get(0)?)
}
fn reset(client: &mut Client) -> Result<()> {
    client.query_one(
        "SELECT syntra_seed_capture('tail_check','id','v',4096)",
        &[],
    )?;
    client.query_one("SELECT syntra_apply_capture('tail_check')", &[])?;
    Ok(())
}
pub(super) fn run(cluster: &Cluster) -> Result<Vec<&'static str>> {
    let mut writer = cluster.connect("syntra_enabled")?;
    let mut reader = cluster.connect("syntra_enabled")?;
    writer.batch_execute("CREATE TABLE tail_check(id int4 PRIMARY KEY,v int4); INSERT INTO tail_check VALUES(1,-50),(2,NULL),(3,100),(4,7)")?;
    reset(&mut writer)?;
    writer.batch_execute("DELETE FROM tail_check WHERE id IN(1,3); UPDATE tail_check SET id=5,v=NULL WHERE id=4; UPDATE tail_check SET v=-2 WHERE id=2; INSERT INTO tail_check VALUES(6,8)")?;
    let control:bool=reader.query_one("SELECT s.route='heap' AND s.reason='checkpoint_lagging' AND s.sum=6 AND f.route='heap' AND f.reason='checkpoint_lagging' AND f.sum=6 FROM syntra_checkpoint_int4_summary('tail_check','v') AS s,syntra_checkpoint_int4_summary('tail_check','v',false) AS f LIMIT 1",&[])?.try_get(0)?;
    require(
        control,
        "default checkpoint summary enabled tail replay without opt-in",
    )?;
    let before = fingerprint(&mut writer)?;
    reader.batch_execute("BEGIN READ ONLY")?;
    read(
        &mut reader,
        "replay",
        "bounded_tail",
        (3, 2, Some(6), Some(-2), Some(8)),
    )?;
    read(
        &mut reader,
        "replay",
        "bounded_tail",
        (3, 2, Some(6), Some(-2), Some(8)),
    )?;
    reader.batch_execute("COMMIT")?;
    require(
        before == fingerprint(&mut writer)?,
        "tail read changed checkpoint tuples, cursor, pruning or events",
    )?;
    reader.batch_execute("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY")?;
    read(
        &mut reader,
        "replay",
        "bounded_tail",
        (3, 2, Some(6), Some(-2), Some(8)),
    )?;
    writer.batch_execute(
        "SELECT syntra_apply_capture('tail_check'); SELECT syntra_prune_capture('tail_check')",
    )?;
    read(
        &mut reader,
        "replay",
        "bounded_tail",
        (3, 2, Some(6), Some(-2), Some(8)),
    )?;
    reader.batch_execute("COMMIT")?;
    read(
        &mut reader,
        "checkpoint",
        "eligible",
        (3, 2, Some(6), Some(-2), Some(8)),
    )?;
    writer.batch_execute("DELETE FROM tail_check WHERE id IN(2,6)")?;
    read(
        &mut reader,
        "replay",
        "bounded_tail",
        (1, 0, None, None, None),
    )?;
    writer.batch_execute("DELETE FROM tail_check WHERE id=5")?;
    read(
        &mut reader,
        "replay",
        "bounded_tail",
        (0, 0, None, None, None),
    )?;
    writer.batch_execute("INSERT INTO tail_check VALUES(7,-9); UPDATE tail_check SET v=10 WHERE id=7; DELETE FROM tail_check WHERE id=7; INSERT INTO tail_check VALUES(7,NULL); UPDATE tail_check SET v=-9 WHERE id=7")?;
    read(
        &mut reader,
        "replay",
        "bounded_tail",
        (1, 1, Some(-9), Some(-9), Some(-9)),
    )?;
    writer.batch_execute("DELETE FROM syntra.capture_events WHERE source_relation='tail_check'::regclass AND ordinal=(SELECT emitted FROM (SELECT hh.source_relation,hh.epoch,hh.base_rows,hh.pruned,coalesce(m.ordinal,hh.emitted) AS emitted,coalesce(m.live_rows,hh.live_rows) AS live_rows FROM syntra.capture_heads AS hh LEFT JOIN LATERAL (SELECT ordinal,live_rows FROM syntra.capture_marks WHERE epoch=hh.epoch AND ordinal>hh.emitted ORDER BY ordinal DESC LIMIT 1) AS m ON true) AS h WHERE source_relation='tail_check'::regclass LIMIT 1)")?;
    read(
        &mut reader,
        "heap",
        "checkpoint_history_gap",
        (1, 1, Some(-9), Some(-9), Some(-9)),
    )?;
    reset(&mut writer)?;
    writer.batch_execute("UPDATE tail_check SET v=11 WHERE id=7; UPDATE syntra.capture_events SET before_value=999 WHERE source_relation='tail_check'::regclass AND ordinal=1")?;
    read(
        &mut reader,
        "heap",
        "checkpoint_tail_rejected",
        (1, 1, Some(11), Some(11), Some(11)),
    )?;
    writer.batch_execute("UPDATE syntra.capture_events SET before_value=-9 WHERE source_relation='tail_check'::regclass AND ordinal=1")?;
    let partition = syntra_storage::chunks::partition(7) as i16;
    writer.execute("UPDATE syntra.capture_chunks SET state=set_byte(state,0,get_byte(state,0) # 1) WHERE source_relation='tail_check'::regclass AND partition=$1",&[&partition])?;
    read(
        &mut reader,
        "heap",
        "invalid_checkpoint",
        (1, 1, Some(11), Some(11), Some(11)),
    )?;
    writer.execute("UPDATE syntra.capture_chunks SET state=set_byte(state,0,get_byte(state,0) # 1) WHERE source_relation='tail_check'::regclass AND partition=$1",&[&partition])?;
    let clean = (partition + 1) % 64;
    writer.execute("UPDATE syntra.capture_chunks SET segment=set_byte(segment,0,get_byte(segment,0) # 1) WHERE source_relation='tail_check'::regclass AND partition=$1",&[&clean])?;
    read(
        &mut reader,
        "heap",
        "invalid_checkpoint",
        (1, 1, Some(11), Some(11), Some(11)),
    )?;
    writer.execute("UPDATE syntra.capture_chunks SET segment=set_byte(segment,0,get_byte(segment,0) # 1) WHERE source_relation='tail_check'::regclass AND partition=$1",&[&clean])?;
    writer.batch_execute("SELECT syntra_apply_capture('tail_check'); SELECT syntra_prune_capture('tail_check'); DO $$ BEGIN FOR n IN 1..256 LOOP UPDATE tail_check SET v=v+1 WHERE id=7; END LOOP; END $$")?;
    read(
        &mut reader,
        "replay",
        "bounded_tail",
        (1, 1, Some(267), Some(267), Some(267)),
    )?;
    writer.batch_execute("UPDATE tail_check SET v=v+1 WHERE id=7")?;
    read(
        &mut reader,
        "heap",
        "checkpoint_tail_limit",
        (1, 1, Some(268), Some(268), Some(268)),
    )?;
    writer.batch_execute("SELECT syntra_apply_capture('tail_check'); SELECT syntra_prune_capture('tail_check'); CREATE ROLE tail_reader NOLOGIN; GRANT SELECT(v) ON tail_check TO tail_reader; UPDATE tail_check SET v=269 WHERE id=7")?;
    reader.batch_execute("SET ROLE tail_reader")?;
    let column_only = reader
        .query_one(
            "SELECT rows FROM syntra_checkpoint_int4_summary('tail_check','v',true) LIMIT 1",
            &[],
        )
        .err()
        .ok_or("tail replay accepted column-only access despite the table SELECT contract")?;
    require(
        column_only.code().is_some_and(|c| c.code() == "42501"),
        "tail column-only denial had unexpected SQLSTATE",
    )?;
    writer.batch_execute("GRANT SELECT ON tail_check TO tail_reader")?;
    read(
        &mut reader,
        "replay",
        "bounded_tail",
        (1, 1, Some(269), Some(269), Some(269)),
    )?;
    writer.batch_execute("BEGIN; UPDATE tail_check SET v=270 WHERE id=7")?;
    let own:bool=writer.query_one("SELECT s.route='heap' AND s.reason='transaction_has_xid' AND s.sum=270 AND pg_current_xact_id_if_assigned() IS NOT NULL FROM syntra_checkpoint_int4_summary('tail_check','v',true) AS s LIMIT 1",&[])?.try_get(0)?;
    require(own, "explicit replay bypassed own-write heap protection")?;
    writer.batch_execute("ROLLBACK")?;
    let private = reader
        .query_one("SELECT count(*) FROM syntra.capture_chunks", &[])
        .err()
        .ok_or("tail reader accessed private chunks")?;
    require(
        private.code().is_some_and(|c| c.code() == "42501"),
        "tail chunk ACL denial had unexpected SQLSTATE",
    )?;
    writer.batch_execute("REVOKE SELECT ON tail_check FROM tail_reader")?;
    let denied = reader
        .query_one(
            "SELECT rows FROM syntra_checkpoint_int4_summary('tail_check','v',true) LIMIT 1",
            &[],
        )
        .err()
        .ok_or("tail replay bypassed source ACL")?;
    require(
        denied.code().is_some_and(|c| c.code() == "42501"),
        "tail source ACL denial had unexpected SQLSTATE",
    )?;
    reader.batch_execute("RESET ROLE")?;
    writer.batch_execute("DROP TABLE tail_check; DROP ROLE tail_reader")?;
    Ok(vec![
        "default summary retains lag fallback",
        "read-only replay without XID or tuple changes",
        "old tail snapshot survives apply and prune",
        "deleted extrema and cross-partition key move",
        "all-null and empty results",
        "ordered repeated-key lifecycle",
        "missing event fallback",
        "bad before-image fallback",
        "corrupt touched state fallback",
        "corrupt untouched segment fallback",
        "256-event replay and 257-event fallback",
        "explicit opt-in retains own-write heap protection",
        "column-only source access denied",
        "table SELECT, source revocation and private catalog denial",
    ])
}
