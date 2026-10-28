//! Real committed transactions are required: pgrx unit tests already own XIDs.
use crate::Cluster;
use postgres::Client;
use syntra_bench::Result;

fn require(value: bool, message: &'static str) -> Result<()> {
    if value {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn check(client: &mut Client, route: &str, reason: &str, sum: Option<i64>) -> Result<()> {
    let row = client.query_one(
        r#"SELECT s.route, s.reason, s.sum,
        ROW(s.rows,s.non_nulls,s.sum,s.min,s.max) IS NOT DISTINCT FROM
        ROW(n.rows,n.non_nulls,n.sum,n.min,n.max)
        FROM syntra_live_int4_summary('trusted_check','v') AS s,
        (SELECT count(*) AS rows,count(v) AS non_nulls,sum(v),min(v),max(v)
         FROM trusted_check) AS n LIMIT 1"#,
        &[],
    )?;
    let actual_route: &str = row.try_get(0)?;
    let actual_reason: &str = row.try_get(1)?;
    if actual_route != route || actual_reason != reason {
        return Err(format!(
            "trusted route {actual_route}/{actual_reason}; expected {route}/{reason}"
        )
        .into());
    }
    require(
        row.try_get::<_, Option<i64>>(2)? == sum && row.try_get::<_, bool>(3)?,
        "trusted aggregate disagreed with native SQL at the same snapshot",
    )
}

fn publish(client: &mut Client) -> Result<()> {
    let rows: i64 = client
        .query_one(
            "SELECT syntra_publish_trusted_int4('trusted_check','v')",
            &[],
        )?
        .try_get(0)?;
    require(rows == 3, "trusted publisher returned the wrong row count")
}

pub(super) fn run(cluster: &Cluster) -> Result<Vec<&'static str>> {
    let mut reader = cluster.connect("syntra_enabled")?;
    let mut writer = cluster.connect("syntra_enabled")?;
    reader.batch_execute("CREATE TABLE trusted_check(id int4 PRIMARY KEY,v int4); INSERT INTO trusted_check(id,v) VALUES(1,10),(2,20),(3,NULL)")?;
    check(&mut reader, "heap", "untracked", Some(30))?;
    reader.query_one("SELECT syntra_track_source('trusted_check','v')", &[])?;
    check(&mut reader, "heap", "no_projection", Some(30))?;
    publish(&mut reader)?;
    check(&mut reader, "projection", "eligible", Some(30))?;
    let before = reader.query_one("SELECT ctid::text,xmin::text FROM syntra.trusted_int4_projections WHERE source_relation='trusted_check'::regclass AND source_attribute=2 LIMIT 1", &[])?;
    reader.batch_execute("BEGIN")?;
    publish(&mut reader)?;
    let no_xid: bool = reader
        .query_one("SELECT pg_current_xact_id_if_assigned() IS NULL", &[])?
        .try_get(0)?;
    require(no_xid, "unchanged publication assigned a transaction ID")?;
    reader.batch_execute("COMMIT")?;
    let after = reader.query_one("SELECT ctid::text,xmin::text FROM syntra.trusted_int4_projections WHERE source_relation='trusted_check'::regclass AND source_attribute=2 LIMIT 1", &[])?;
    require(
        before.try_get::<_, String>(0)? == after.try_get::<_, String>(0)?
            && before.try_get::<_, String>(1)? == after.try_get::<_, String>(1)?,
        "unchanged publication rewrote its binding",
    )?;
    let range = reader.query_one("SELECT rows=1 AND non_nulls=1 AND sum=20 AND min=20 AND max=20 AND route='projection' FROM syntra_live_int4_range_summary('trusted_check','v',11,20) LIMIT 1", &[])?;
    require(range.try_get(0)?, "trusted range predicate failed")?;
    let inverted = reader.query_one("SELECT rows=0 AND non_nulls=0 AND sum IS NULL AND route='projection' FROM syntra_live_int4_range_summary('trusted_check','v',20,11) LIMIT 1", &[])?;
    require(inverted.try_get(0)?, "trusted inverted range failed")?;

    reader.batch_execute("BEGIN READ ONLY")?;
    check(&mut reader, "projection", "eligible", Some(30))?;
    require(
        reader
            .query_one("SELECT pg_current_xact_id_if_assigned() IS NULL", &[])?
            .try_get(0)?,
        "trusted read assigned a transaction ID",
    )?;
    reader.batch_execute("COMMIT")?;

    writer.batch_execute("UPDATE trusted_check SET v=11 WHERE id=1")?;
    check(&mut reader, "heap", "version_changed", Some(31))?;
    publish(&mut writer)?;
    check(&mut reader, "projection", "eligible", Some(31))?;
    reader.batch_execute("BEGIN; UPDATE trusted_check SET v=12 WHERE id=1")?;
    check(&mut reader, "heap", "transaction_has_xid", Some(32))?;
    reader.batch_execute("ROLLBACK")?;
    check(&mut reader, "projection", "eligible", Some(31))?;

    reader.batch_execute("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY")?;
    check(&mut reader, "projection", "eligible", Some(31))?;
    writer.batch_execute("UPDATE trusted_check SET v=13 WHERE id=1")?;
    publish(&mut writer)?;
    check(&mut reader, "projection", "eligible", Some(31))?;
    reader.batch_execute("COMMIT")?;
    check(&mut reader, "projection", "eligible", Some(33))?;

    reader.batch_execute("BEGIN")?;
    check(&mut reader, "projection", "eligible", Some(33))?;
    writer.batch_execute("UPDATE trusted_check SET v=14 WHERE id=1")?;
    check(&mut reader, "heap", "version_changed", Some(34))?;
    reader.batch_execute("COMMIT")?;
    publish(&mut writer)?;

    // Nested SPI sees the current row change before the AFTER STATEMENT counter.
    reader.batch_execute(r#"CREATE TABLE trusted_row_observation(ok boolean NOT NULL);
        CREATE FUNCTION trusted_row_probe() RETURNS trigger LANGUAGE plpgsql AS $$
        DECLARE s record; n bigint; p bigint;
        BEGIN
            SELECT sum,route,reason INTO s FROM syntra_live_int4_summary('trusted_check','v') LIMIT 1;
            SELECT sum(v) INTO n FROM trusted_check;
            PERFORM syntra_publish_trusted_int4('trusted_check','v');
            SELECT q.sum INTO p FROM syntra.trusted_int4_projections AS c,
                LATERAL syntra_segment_summary(c.segment) AS q
                WHERE c.source_relation='trusted_check'::regclass AND c.source_attribute=2 LIMIT 1;
            INSERT INTO trusted_row_observation(ok) VALUES
                (s.sum IS NOT DISTINCT FROM n AND p IS NOT DISTINCT FROM n AND s.route='heap' AND s.reason='transaction_has_xid');
            RETURN NEW;
        END $$;
        CREATE TRIGGER trusted_row_probe AFTER UPDATE ON trusted_check FOR EACH ROW EXECUTE FUNCTION trusted_row_probe()"#)?;
    // Trigger DDL changes the epoch; publish the complete new registration.
    publish(&mut writer)?;
    writer.batch_execute("UPDATE trusted_check SET v=v+1 WHERE id IN(1,2)")?;
    require(
        reader
            .query_one(
                "SELECT count(*)=2 AND bool_and(ok) FROM trusted_row_observation",
                &[],
            )?
            .try_get(0)?,
        "nested row-trigger aggregate reused an unfinished statement",
    )?;
    reader.batch_execute("DROP TRIGGER trusted_row_probe ON trusted_check; DROP FUNCTION trusted_row_probe(); DROP TABLE trusted_row_observation")?;
    publish(&mut writer)?;

    writer.batch_execute("UPDATE syntra.trusted_int4_projections SET segment=set_byte(segment,0,get_byte(segment,0) # 1) WHERE source_relation='trusted_check'::regclass")?;
    check(&mut reader, "heap", "invalid_segment", Some(36))?;
    publish(&mut writer)?;
    check(&mut reader, "projection", "eligible", Some(36))?;
    writer.query_one("SELECT syntra_track_source('trusted_check','v')", &[])?;
    check(&mut reader, "heap", "epoch_changed", Some(36))?;
    publish(&mut writer)?;
    writer.batch_execute("ALTER TABLE trusted_check DISABLE TRIGGER _syntra_source_changed")?;
    check(&mut reader, "heap", "guard_invalid", Some(36))?;
    writer.batch_execute("UPDATE trusted_check SET v=16 WHERE id=1; ALTER TABLE trusted_check ENABLE ALWAYS TRIGGER _syntra_source_changed")?;
    check(&mut reader, "heap", "epoch_changed", Some(37))?;
    publish(&mut writer)?;
    reader.batch_execute("SET event_triggers=false")?;
    check(&mut reader, "heap", "guard_invalid", Some(37))?;
    reader.batch_execute("SET event_triggers=true")?;

    // Pin metadata only. Reading the source here would correctly block TRUNCATE.
    reader.batch_execute("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY")?;
    reader.query_one("SELECT epoch FROM syntra.tracked_sources WHERE source_relation='trusted_check'::regclass LIMIT 1", &[])?;
    writer.batch_execute("TRUNCATE trusted_check")?;
    check(&mut reader, "heap", "storage_or_schema_changed", None)?;
    reader.batch_execute("COMMIT")?;

    reader.batch_execute("INSERT INTO trusted_check(id,v) VALUES(1,10),(2,20),(3,NULL)")?;
    publish(&mut writer)?;
    reader.batch_execute("CREATE ROLE trusted_reader NOLOGIN; GRANT SELECT ON trusted_check TO trusted_reader; SET ROLE trusted_reader")?;
    check(&mut reader, "projection", "eligible", Some(30))?;
    let private = reader
        .query_one("SELECT count(*) FROM syntra.trusted_int4_projections", &[])
        .err()
        .ok_or("ordinary caller read the trusted catalog")?;
    require(
        private.code().map(|c| c.code()) == Some("42501"),
        "private catalog denial had an unexpected SQLSTATE",
    )?;
    let publication = reader
        .query_one(
            "SELECT syntra_publish_trusted_int4('trusted_check','v')",
            &[],
        )
        .err()
        .ok_or("ordinary caller published trusted bytes")?;
    require(
        publication.code().map(|c| c.code()) == Some("42501"),
        "publication denial had an unexpected SQLSTATE",
    )?;
    reader.batch_execute(
        "RESET ROLE; REVOKE SELECT ON trusted_check FROM trusted_reader; SET ROLE trusted_reader",
    )?;
    let denied = reader
        .query_one(
            "SELECT sum FROM syntra_live_int4_summary('trusted_check','v') LIMIT 1",
            &[],
        )
        .err()
        .ok_or("trusted reader bypassed source ACLs")?;
    require(
        denied.code().map(|c| c.code()) == Some("42501"),
        "source ACL denial had an unexpected SQLSTATE",
    )?;
    reader.batch_execute("RESET ROLE")?;

    writer.batch_execute("BEGIN ISOLATION LEVEL REPEATABLE READ")?;
    let isolation = writer
        .query_one(
            "SELECT syntra_publish_trusted_int4('trusted_check','v')",
            &[],
        )
        .err()
        .ok_or("repeatable-read publication unexpectedly succeeded")?;
    require(
        isolation.code().map(|c| c.code()) == Some("0A000"),
        "publication isolation denial had an unexpected SQLSTATE",
    )?;
    writer.batch_execute("ROLLBACK; BEGIN")?;
    publish(&mut writer)?;
    let busy = reader
        .query_one(
            "SELECT syntra_publish_trusted_int4('trusted_check','v')",
            &[],
        )
        .err()
        .ok_or("concurrent publisher bypassed admission")?;
    require(
        busy.code().map(|c| c.code()) == Some("55P03"),
        "publication admission had an unexpected SQLSTATE",
    )?;
    writer.batch_execute("ROLLBACK")?;
    check(&mut reader, "projection", "eligible", Some(30))?;

    reader.batch_execute("SET session_replication_role=replica; DROP TABLE trusted_check; SET session_replication_role=origin")?;
    require(
        reader
            .query_one(
                "SELECT count(*)=0 FROM syntra.trusted_int4_projections",
                &[],
            )?
            .try_get(0)?,
        "replica-mode source drop left a trusted artifact",
    )?;
    reader.batch_execute("DROP ROLE trusted_reader")?;
    Ok(vec![
        "untracked fallback",
        "explicit trusted publication",
        "unchanged publication without rewrite or XID",
        "range and inverted bounds",
        "read-only without XID",
        "committed invalidation",
        "own-write fallback",
        "rollback eligibility",
        "repeatable-read historical cache",
        "read-committed next snapshot",
        "nested row-trigger fallback",
        "corrupt payload fallback",
        "registration epoch",
        "current source and global guards",
        "old-snapshot truncate storage identity",
        "ordinary reader and private catalog ACLs",
        "source ACL revocation",
        "publication isolation",
        "nonblocking publisher admission",
        "replica-mode source drop cleanup",
    ])
}
