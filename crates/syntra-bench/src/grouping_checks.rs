//! Independent-session checks for the explicit integer grouping SQL path.
use crate::Cluster;
use postgres::Client;
use syntra_bench::Result;

fn require(ok: bool, message: &'static str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn verify(client: &mut Client) -> Result<String> {
    let row = client.query_one(
        r#"WITH actual AS (
            SELECT group_key,rows,non_nulls,sum,min,max
            FROM syntra_heap_group_integer_summary('group_commands','k','v',16)
        ), expected AS (
            SELECT k AS group_key,count(*) AS rows,count(v) AS non_nulls,
                   sum(v) AS sum,min(v) AS min,max(v) AS max
            FROM group_commands GROUP BY k
        ) SELECT NOT EXISTS (
            SELECT group_key,rows,non_nulls,sum,min,max FROM actual
            EXCEPT ALL SELECT group_key,rows,non_nulls,sum,min,max FROM expected
        ) AND NOT EXISTS (
            SELECT group_key,rows,non_nulls,sum,min,max FROM expected
            EXCEPT ALL SELECT group_key,rows,non_nulls,sum,min,max FROM actual
        ), coalesce((SELECT sum(sum)::text FROM actual),'NULL')"#,
        &[],
    )?;
    require(
        row.try_get(0)?,
        "SQL grouping disagreed with native SQL at the caller's snapshot",
    )?;
    Ok(row.try_get(1)?)
}

pub(super) fn run(cluster: &Cluster) -> Result<Vec<&'static str>> {
    let mut reader = cluster.connect("syntra_enabled")?;
    let mut writer = cluster.connect("syntra_enabled")?;
    reader.batch_execute("CREATE TABLE group_commands(k int8,v int8); INSERT INTO group_commands VALUES(1,9223372036854775807),(1,9223372036854775807),(2,NULL),(NULL,-9)")?;
    reader.batch_execute("BEGIN ISOLATION LEVEL REPEATABLE READ")?;
    let before = verify(&mut reader)?;
    writer.batch_execute("UPDATE group_commands SET k=3,v=5 WHERE k=2; UPDATE group_commands SET v=-10 WHERE k IS NULL")?;
    require(
        verify(&mut reader)? == before,
        "SQL grouping changed beneath a repeatable-read snapshot",
    )?;
    reader.batch_execute("COMMIT")?;
    require(
        verify(&mut reader)? != before,
        "SQL grouping missed a committed key/value change",
    )?;

    writer.batch_execute("BEGIN; UPDATE group_commands SET v=100 WHERE k=1")?;
    let own = verify(&mut writer)?;
    require(
        verify(&mut reader)? != own,
        "SQL grouping exposed an uncommitted source write",
    )?;
    writer.batch_execute("ROLLBACK")?;

    reader.batch_execute("BEGIN READ ONLY")?;
    verify(&mut reader)?;
    require(
        reader
            .query_one("SELECT pg_current_xact_id_if_assigned() IS NULL", &[])?
            .try_get(0)?,
        "SQL grouping assigned an XID in a read-only transaction",
    )?;
    let blocked = writer
        .batch_execute("BEGIN; LOCK TABLE group_commands IN ACCESS EXCLUSIVE MODE NOWAIT")
        .err()
        .ok_or("SQL grouping released its source DDL lock before transaction end")?;
    require(
        blocked.code().is_some_and(|code| code.code() == "55P03"),
        "SQL grouping DDL protection returned the wrong SQLSTATE",
    )?;
    writer.batch_execute("ROLLBACK")?;
    reader.batch_execute("COMMIT")?;
    writer.batch_execute(
        "BEGIN; LOCK TABLE group_commands IN ACCESS EXCLUSIVE MODE NOWAIT; ROLLBACK",
    )?;

    reader.batch_execute("BEGIN ISOLATION LEVEL SERIALIZABLE")?;
    let unsupported = reader
        .query(
            "SELECT group_key FROM syntra_heap_group_integer_summary('group_commands','k','v')",
            &[],
        )
        .err()
        .ok_or("SQL grouping accepted unsupported serializable isolation")?;
    require(
        unsupported
            .code()
            .is_some_and(|code| code.code() == "0A000"),
        "serializable SQL grouping returned the wrong SQLSTATE",
    )?;
    reader.batch_execute("ROLLBACK")?;

    reader.batch_execute("CREATE TABLE group_cancel(k int4,v int4); INSERT INTO group_cancel SELECT 1,g FROM generate_series(1,1000000) AS g; SET statement_timeout='1ms'")?;
    let cancelled = reader
        .query(
            "SELECT group_key FROM syntra_heap_group_integer_summary('group_cancel','k','v',1)",
            &[],
        )
        .err()
        .ok_or("SQL grouping did not honor statement_timeout")?;
    require(
        cancelled.code().is_some_and(|code| code.code() == "57014"),
        "SQL grouping cancellation returned the wrong SQLSTATE",
    )?;
    reader.batch_execute("SET statement_timeout=0")?;
    require(reader.query_one("SELECT rows=1000000 AND sum=500000500000 FROM syntra_heap_group_integer_summary('group_cancel','k','v',1)", &[])?.try_get(0)?, "SQL grouping did not recover cleanly after cancellation")?;
    reader.batch_execute("DROP TABLE group_cancel; DROP TABLE group_commands")?;
    Ok(vec![
        "nullable keys and values at the caller's snapshot",
        "repeatable-read visibility across committed key/value changes",
        "own-write visibility and uncommitted isolation",
        "read-only grouping without an assigned XID",
        "source DDL lock retained until transaction end",
        "serializable isolation rejected explicitly",
        "statement timeout and clean reuse after cancellation",
    ])
}
