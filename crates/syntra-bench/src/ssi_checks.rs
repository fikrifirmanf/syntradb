//! Real SSI conflicts. Participant transactions never read a native heap oracle
//! alongside a custom read: that would supply the missing dependency itself.
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

#[derive(Clone, Copy)]
enum ReadPath {
    Heap,
    Range,
    Grouping,
    TextGrouping,
    UuidGrouping,
    IntegerDecimalGrouping,
    TextDecimalGrouping,
    UuidDecimalGrouping,
    Date,
    Timestamp,
    Timestamptz,
    Native,
    NativeRange,
}

impl ReadPath {
    fn sql(self) -> &'static str {
        match self {
            Self::Heap => "SELECT sum FROM syntra_heap_int4_summary('ssi_source','v') LIMIT 1",
            Self::Range => "SELECT sum FROM syntra_heap_int4_range_summary('ssi_source','v',1,1) LIMIT 1",
            Self::Grouping => "SELECT sum(sum)::bigint FROM syntra_heap_group_integer_summary('ssi_source','k','v')",
            Self::TextGrouping => "SELECT sum(sum)::bigint FROM syntra_heap_group_text_summary('ssi_source','label','v')",
            Self::UuidGrouping => "SELECT sum(sum)::bigint FROM syntra_heap_group_uuid_summary('ssi_source','identity','v')",
            Self::IntegerDecimalGrouping => "SELECT sum(sum)::bigint FROM syntra_heap_group_integer_numeric_summary('ssi_source','k','n')",
            Self::TextDecimalGrouping => "SELECT sum(sum)::bigint FROM syntra_heap_group_text_numeric_summary('ssi_source','label','n')",
            Self::UuidDecimalGrouping => "SELECT sum(sum)::bigint FROM syntra_heap_group_uuid_numeric_summary('ssi_source','identity','n')",
            Self::Date => "SELECT sum::bigint FROM syntra_heap_date_summary('ssi_source','v','d','2026-01-01','2026-01-02') LIMIT 1",
            Self::Timestamp => "SELECT sum::bigint FROM syntra_heap_timestamp_summary('ssi_source','v','t','2026-01-01','2026-01-02') LIMIT 1",
            Self::Timestamptz => "SELECT sum::bigint FROM syntra_heap_timestamptz_summary('ssi_source','v','z','2026-01-01+00','2026-01-02+00') LIMIT 1",
            Self::Native => "SELECT sum(v)::bigint FROM ssi_source",
            Self::NativeRange => "SELECT sum(v)::bigint FROM ssi_source WHERE v=1",
        }
    }
    fn read(self, client: &mut Client) -> Result<Option<i64>> {
        Ok(client.query_one(self.sql(), &[])?.try_get(0)?)
    }
}

fn predicate_lock(client: &mut Client) -> Result<()> {
    require(client.query_one("SELECT EXISTS(SELECT 1 FROM pg_locks WHERE pid=pg_backend_pid() AND relation='ssi_source'::regclass AND locktype='relation' AND mode='SIReadLock')", &[])?.try_get(0)?,
        "fresh heap read did not acquire its source relation predicate lock")
}

#[derive(Clone, Copy)]
enum Scenario {
    WriteSkew,
    EmptyPhantom,
    RangePhantom,
}

fn conflict(
    cluster: &Cluster,
    admin: &mut Client,
    a: ReadPath,
    b: ReadPath,
    scenario: Scenario,
) -> Result<()> {
    let (seed, expected, first_write, second_write, invariant) = match scenario {
        Scenario::WriteSkew => (
            "TRUNCATE ssi_source; INSERT INTO ssi_source VALUES(1,1),(2,1)",
            Some(2),
            "UPDATE ssi_source SET v=0,n=0 WHERE k=1; COMMIT",
            "UPDATE ssi_source SET v=0,n=0 WHERE k=2; COMMIT",
            "SELECT sum(v)=1 FROM ssi_source",
        ),
        Scenario::EmptyPhantom => (
            "TRUNCATE ssi_source",
            None,
            "INSERT INTO ssi_source VALUES(1,1); COMMIT",
            "INSERT INTO ssi_source VALUES(2,1); COMMIT",
            "SELECT count(*)=1 AND sum(v)=1 FROM ssi_source",
        ),
        Scenario::RangePhantom => (
            "TRUNCATE ssi_source; INSERT INTO ssi_source VALUES(10,10)",
            None,
            "INSERT INTO ssi_source VALUES(1,1); COMMIT",
            "INSERT INTO ssi_source VALUES(2,1); COMMIT",
            "SELECT count(*)=1 FROM ssi_source WHERE v=1",
        ),
    };
    if matches!(scenario, Scenario::RangePhantom)
        && (matches!(
            a,
            ReadPath::Date | ReadPath::Timestamp | ReadPath::Timestamptz
        ) || matches!(
            b,
            ReadPath::Date | ReadPath::Timestamp | ReadPath::Timestamptz
        ))
    {
        admin.batch_execute("TRUNCATE ssi_source; INSERT INTO ssi_source VALUES(10,10,'2026-02-01','2026-02-01','2026-02-01+00')")?;
    } else {
        admin.batch_execute(seed)?;
    }
    let mut first = cluster.connect("syntra_enabled")?;
    let mut second = cluster.connect("syntra_enabled")?;
    first.batch_execute(
        "SET statement_timeout='5s'; SET lock_timeout='1s'; BEGIN ISOLATION LEVEL SERIALIZABLE",
    )?;
    second.batch_execute(
        "SET statement_timeout='5s'; SET lock_timeout='1s'; BEGIN ISOLATION LEVEL SERIALIZABLE",
    )?;
    // These completed reads are the transaction barriers. Both readers must see
    // the original state before either writes a distinct key.
    require(
        a.read(&mut first)? == expected && b.read(&mut second)? == expected,
        "SSI conflict participants did not read the intended initial state",
    )?;
    predicate_lock(&mut first)?;
    predicate_lock(&mut second)?;
    first.batch_execute(first_write)?;
    let rejected = second
        .batch_execute(second_write)
        .err()
        .ok_or("both SSI conflict participants committed; read dependency was lost")?;
    require(
        rejected.code().is_some_and(|code| code.code() == "40001"),
        "SSI conflict did not surface SQLSTATE 40001",
    )?;
    second.batch_execute("ROLLBACK")?;
    // The native invariant is checked only after the participant transactions.
    require(
        admin.query_one(invariant, &[])?.try_get(0)?,
        "SSI conflict left the native source invariant broken",
    )
}

pub(super) fn run(cluster: &Cluster) -> Result<Vec<&'static str>> {
    let mut admin = cluster.connect("syntra_enabled")?;
    admin.batch_execute("CREATE TABLE ssi_source(k int4 PRIMARY KEY,v int4,d date DEFAULT '2026-01-01',t timestamp DEFAULT '2026-01-01',z timestamptz DEFAULT '2026-01-01+00',label text COLLATE \"C\" DEFAULT 'label',identity uuid DEFAULT '00000000-0000-0000-0000-000000000001',n numeric(18,2) DEFAULT 1.00); INSERT INTO ssi_source VALUES(1,1),(2,1)")?;
    let mut reader = cluster.connect("syntra_enabled")?;
    for mode in [
        "BEGIN ISOLATION LEVEL SERIALIZABLE",
        "BEGIN ISOLATION LEVEL SERIALIZABLE READ ONLY",
        "BEGIN ISOLATION LEVEL SERIALIZABLE READ ONLY DEFERRABLE",
    ] {
        for path in [
            ReadPath::Heap,
            ReadPath::Range,
            ReadPath::Grouping,
            ReadPath::TextGrouping,
            ReadPath::UuidGrouping,
            ReadPath::IntegerDecimalGrouping,
            ReadPath::TextDecimalGrouping,
            ReadPath::UuidDecimalGrouping,
            ReadPath::Date,
            ReadPath::Timestamp,
            ReadPath::Timestamptz,
        ] {
            reader.batch_execute(mode)?;
            require(
                path.read(&mut reader)? == Some(2),
                "serializable fresh read returned the wrong aggregate",
            )?;
            if mode == "BEGIN ISOLATION LEVEL SERIALIZABLE" {
                predicate_lock(&mut reader)?;
            }
            require(
                reader
                    .query_one("SELECT pg_current_xact_id_if_assigned() IS NULL", &[])?
                    .try_get(0)?,
                "serializable fresh read assigned an XID",
            )?;
            reader.batch_execute("COMMIT")?;
        }
    }
    for path in [
        ReadPath::Heap,
        ReadPath::Range,
        ReadPath::Grouping,
        ReadPath::TextGrouping,
        ReadPath::UuidGrouping,
        ReadPath::IntegerDecimalGrouping,
        ReadPath::TextDecimalGrouping,
        ReadPath::UuidDecimalGrouping,
        ReadPath::Date,
        ReadPath::Timestamp,
        ReadPath::Timestamptz,
    ] {
        for (a, b) in [
            (path, path),
            (path, ReadPath::Native),
            (ReadPath::Native, path),
        ] {
            conflict(cluster, &mut admin, a, b, Scenario::WriteSkew)?;
            conflict(cluster, &mut admin, a, b, Scenario::EmptyPhantom)?;
        }
    }
    for path in [
        ReadPath::Range,
        ReadPath::Date,
        ReadPath::Timestamp,
        ReadPath::Timestamptz,
    ] {
        for (a, b) in [
            (path, path),
            (path, ReadPath::NativeRange),
            (ReadPath::NativeRange, path),
        ] {
            conflict(cluster, &mut admin, a, b, Scenario::RangePhantom)?;
        }
    }

    // Force the pivot's outgoing conflict while a Rust heap scan is running,
    // rather than only exercising 40001 from a later native UPDATE.
    admin.batch_execute("CREATE TABLE ssi_pivot_input(k int4 PRIMARY KEY,v int4)")?;
    for path in [
        ReadPath::Heap,
        ReadPath::Range,
        ReadPath::Grouping,
        ReadPath::TextGrouping,
        ReadPath::UuidGrouping,
        ReadPath::IntegerDecimalGrouping,
        ReadPath::TextDecimalGrouping,
        ReadPath::UuidDecimalGrouping,
        ReadPath::Date,
        ReadPath::Timestamp,
        ReadPath::Timestamptz,
    ] {
        admin.batch_execute("TRUNCATE ssi_source,ssi_pivot_input; INSERT INTO ssi_source VALUES(1,1); INSERT INTO ssi_pivot_input VALUES(1,1)")?;
        let mut incoming = cluster.connect("syntra_enabled")?;
        let mut pivot = cluster.connect("syntra_enabled")?;
        let mut outgoing = cluster.connect("syntra_enabled")?;
        incoming.batch_execute("BEGIN ISOLATION LEVEL SERIALIZABLE")?;
        require(
            incoming
                .query_one("SELECT sum(v)::bigint FROM ssi_pivot_input", &[])?
                .try_get::<_, Option<i64>>(0)?
                == Some(1),
            "SSI incoming transaction read the wrong state",
        )?;
        pivot.batch_execute(
            "BEGIN ISOLATION LEVEL SERIALIZABLE; UPDATE ssi_pivot_input SET v=2 WHERE k=1",
        )?;
        outgoing.batch_execute(
            "BEGIN ISOLATION LEVEL SERIALIZABLE; UPDATE ssi_source SET v=2,n=2 WHERE k=1; COMMIT",
        )?;
        let aborted = pivot
            .query_one(path.sql(), &[])
            .err()
            .ok_or("fresh heap scan did not surface its read-time SSI conflict")?;
        require(
            aborted.code().is_some_and(|code| code.code() == "40001"),
            "fresh heap scan returned the wrong read-time SSI SQLSTATE",
        )?;
        pivot.batch_execute("ROLLBACK")?;
        incoming.batch_execute("COMMIT")?;
        let expected = if matches!(path, ReadPath::Range) {
            None
        } else {
            Some(2)
        };
        require(
            path.read(&mut pivot)? == expected,
            "fresh scan backend did not recover after a read-time SSI abort",
        )?;
        require(
            admin
                .query_one("SELECT sum(v)=1 FROM ssi_pivot_input", &[])?
                .try_get(0)?,
            "read-time SSI abort did not roll back the pivot's write",
        )?;
    }
    admin.batch_execute("DROP TABLE ssi_pivot_input")?;

    for path in [ReadPath::Date, ReadPath::Timestamp, ReadPath::Timestamptz] {
        admin.batch_execute("TRUNCATE ssi_source; INSERT INTO ssi_source VALUES(1,1),(2,1)")?;
        reader.batch_execute("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY")?;
        require(
            path.read(&mut reader)? == Some(2),
            "temporal RR initial read changed",
        )?;
        admin.batch_execute(
            "UPDATE ssi_source SET v=7,d='2026-02-01',t='2026-02-01',z='2026-02-01+00' WHERE k=1",
        )?;
        require(
            path.read(&mut reader)? == Some(2),
            "temporal RR lost its retained snapshot",
        )?;
        reader.batch_execute("COMMIT")?;
        require(
            path.read(&mut reader)? == Some(1),
            "temporal fresh snapshot missed the changed time predicate",
        )?;
    }

    admin.batch_execute("CREATE TABLE temporal_cancel(v int4,d date,t timestamp,z timestamptz); INSERT INTO temporal_cancel SELECT g,date '2026-01-01',timestamp '2026-01-01',timestamptz '2026-01-01+00' FROM generate_series(1,1000000) g")?;
    for sql in [
        "SELECT rows FROM syntra_heap_date_summary('temporal_cancel','v','d','2026-01-01','2026-01-02')",
        "SELECT rows FROM syntra_heap_date_summary('temporal_cancel','v','d','2100-01-01','2100-01-02')",
        "SELECT rows FROM syntra_heap_timestamp_summary('temporal_cancel','v','t','2026-01-01','2026-01-02')",
        "SELECT rows FROM syntra_heap_timestamp_summary('temporal_cancel','v','t','2100-01-01','2100-01-02')",
        "SELECT rows FROM syntra_heap_timestamptz_summary('temporal_cancel','v','z','2026-01-01+00','2026-01-02+00')",
        "SELECT rows FROM syntra_heap_timestamptz_summary('temporal_cancel','v','z','2100-01-01+00','2100-01-02+00')",
    ] {
        reader.batch_execute("SET statement_timeout='1ms'")?;
        let error = reader.query(sql, &[]).err().ok_or("temporal scan ignored statement_timeout")?;
        require(error.code().is_some_and(|code| code.code()=="57014"), "temporal cancellation returned the wrong SQLSTATE")?;
        reader.batch_execute("SET statement_timeout='5s'")?;
        require(reader.query_one("SELECT rows=1000000 AND sum=500000500000 FROM syntra_heap_date_summary('temporal_cancel','v','d','2026-01-01','2026-01-02')", &[])?.try_get(0)?, "temporal backend did not recover after cancellation")?;
    }
    admin.batch_execute("DROP TABLE temporal_cancel")?;

    for (sql, initial, changed) in [
        ("SELECT string_agg(coalesce(group_key,'NULL')||':'||sum::text,',' ORDER BY group_key) FROM syntra_heap_group_text_summary('ssi_source','label','v')", "label:2", "label:1,moved:7"),
        ("SELECT string_agg(coalesce(group_key::text,'NULL')||':'||sum::text,',' ORDER BY group_key) FROM syntra_heap_group_uuid_summary('ssi_source','identity','v')", "00000000-0000-0000-0000-000000000001:2", "00000000-0000-0000-0000-000000000001:1,00000000-0000-0000-0000-000000000002:7"),
    ] {
        admin.batch_execute("TRUNCATE ssi_source; INSERT INTO ssi_source VALUES(1,1),(2,1)")?;
        reader.batch_execute("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY")?;
        require(reader.query_one(sql,&[])?.try_get::<_,String>(0)?==initial,"owned grouping initial snapshot changed")?;
        admin.batch_execute("UPDATE ssi_source SET v=7,label='moved',identity='00000000-0000-0000-0000-000000000002' WHERE k=1")?;
        require(reader.query_one(sql,&[])?.try_get::<_,String>(0)?==initial,"owned grouping mixed old and new key/value versions")?;
        reader.batch_execute("COMMIT")?;
        require(reader.query_one(sql,&[])?.try_get::<_,String>(0)?==changed,"owned grouping fresh snapshot missed changed keys")?;
    }

    for (sql, initial, changed) in [
        ("SELECT string_agg(group_key::text||':'||sum::text,',' ORDER BY group_key) FROM syntra_heap_group_integer_numeric_summary('ssi_source','k','n')", "1:1.00,2:1.00", "2:1.00,3:7.25"),
        ("SELECT string_agg(group_key||':'||sum::text,',' ORDER BY group_key) FROM syntra_heap_group_text_numeric_summary('ssi_source','label','n')", "label:2.00", "label:1.00,moved:7.25"),
        ("SELECT string_agg(group_key::text||':'||sum::text,',' ORDER BY group_key) FROM syntra_heap_group_uuid_numeric_summary('ssi_source','identity','n')", "00000000-0000-0000-0000-000000000001:2.00", "00000000-0000-0000-0000-000000000001:1.00,00000000-0000-0000-0000-000000000002:7.25"),
    ] {
        admin.batch_execute("TRUNCATE ssi_source; INSERT INTO ssi_source VALUES(1,1),(2,1)")?;
        reader.batch_execute("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY")?;
        require(reader.query_one(sql,&[])?.try_get::<_,String>(0)?==initial,"decimal grouping initial snapshot changed")?;
        admin.batch_execute("UPDATE ssi_source SET k=3,n=7.25,label='moved',identity='00000000-0000-0000-0000-000000000002' WHERE k=1")?;
        require(reader.query_one(sql,&[])?.try_get::<_,String>(0)?==initial,"decimal grouping mixed old and new key/value versions")?;
        reader.batch_execute("COMMIT")?;
        require(reader.query_one(sql,&[])?.try_get::<_,String>(0)?==changed,"decimal grouping fresh snapshot missed changed keys or scale")?;
    }

    admin.batch_execute("CREATE TABLE owned_cancel(k text,u uuid,v int4,n numeric(18,2),i int4); INSERT INTO owned_cancel SELECT 'same','00000000-0000-0000-0000-000000000001',g,g::numeric/100,1 FROM generate_series(1,1000000) g")?;
    for (cancel_sql,reuse_sql) in [
        ("SELECT group_key FROM syntra_heap_group_text_summary('owned_cancel','k','v')", "SELECT rows=1000000 AND sum=500000500000 FROM syntra_heap_group_text_summary('owned_cancel','k','v')"),
        ("SELECT group_key FROM syntra_heap_group_uuid_summary('owned_cancel','u','v')", "SELECT rows=1000000 AND sum=500000500000 FROM syntra_heap_group_uuid_summary('owned_cancel','u','v')"),
        ("SELECT group_key FROM syntra_heap_group_integer_numeric_summary('owned_cancel','i','n')", "SELECT rows=1000000 AND sum=5000005000.00 FROM syntra_heap_group_integer_numeric_summary('owned_cancel','i','n')"),
        ("SELECT group_key FROM syntra_heap_group_text_numeric_summary('owned_cancel','k','n')", "SELECT rows=1000000 AND sum=5000005000.00 FROM syntra_heap_group_text_numeric_summary('owned_cancel','k','n')"),
        ("SELECT group_key FROM syntra_heap_group_uuid_numeric_summary('owned_cancel','u','n')", "SELECT rows=1000000 AND sum=5000005000.00 FROM syntra_heap_group_uuid_numeric_summary('owned_cancel','u','n')"),
    ] {
        reader.batch_execute("SET statement_timeout='1ms'")?;
        let error=reader.query(cancel_sql,&[]).err().ok_or("owned grouping ignored statement timeout")?;
        require(error.code().is_some_and(|c|c.code()=="57014"),"owned grouping cancellation returned wrong SQLSTATE")?;
        reader.batch_execute("SET statement_timeout='5s'")?;
        require(reader.query_one(reuse_sql,&[])?.try_get(0)?,"owned grouping backend did not recover after cancellation")?;
    }
    admin.batch_execute("DROP TABLE owned_cancel")?;

    admin.batch_execute("CREATE DATABASE owned_encoding TEMPLATE template0 ENCODING 'LATIN1' LC_COLLATE 'C' LC_CTYPE 'C'")?;
    let mut encoded = cluster.connect("owned_encoding")?;
    encoded.batch_execute("CREATE EXTENSION syntradb; CREATE TABLE encoding_source(k text,u uuid,v int4,n numeric(5,2) DEFAULT 1.25); INSERT INTO encoding_source VALUES ('a','00000000-0000-0000-0000-000000000001',1),('a','00000000-0000-0000-0000-000000000001',2)")?;
    let refused = encoded
        .query(
            "SELECT group_key FROM syntra_heap_group_text_summary('encoding_source','k','v')",
            &[],
        )
        .err()
        .ok_or("text grouping admitted a non-UTF8 database")?;
    require(
        refused.code().is_some_and(|c| c.code() == "0A000"),
        "encoding rejection returned wrong SQLSTATE",
    )?;
    require(encoded.query_one("SELECT rows=2 AND sum=3 FROM syntra_heap_group_uuid_summary('encoding_source','u','v')",&[])?.try_get(0)?,"UUID grouping incorrectly depended on database text encoding")?;
    let refused = encoded.query("SELECT group_key FROM syntra_heap_group_text_numeric_summary('encoding_source','k','n')", &[]).err().ok_or("decimal text grouping admitted non-UTF8")?;
    require(
        refused.code().is_some_and(|c| c.code() == "0A000"),
        "decimal encoding rejection returned wrong SQLSTATE",
    )?;
    require(encoded.query_one("SELECT rows=2 AND sum=2.50 FROM syntra_heap_group_uuid_numeric_summary('encoding_source','u','n')",&[])?.try_get(0)?,"decimal UUID grouping depended on database encoding")?;
    drop(encoded);
    admin.batch_execute("DROP DATABASE owned_encoding")?;

    // Source capture and reusable state keep the old restriction. They have no
    // proof yet that their metadata reads supply the source's SSI dependencies.
    admin.batch_execute("TRUNCATE ssi_source; INSERT INTO ssi_source VALUES(1,1),(2,1)")?;
    admin.query_one("SELECT syntra_seed_capture('ssi_source','k','v',64)", &[])?;
    for sql in [
        "SELECT source_rows FROM syntra_capture_int4('ssi_source','v')",
        "SELECT rows FROM syntra_live_int4_summary('ssi_source','v')",
        "SELECT rows FROM syntra_checkpoint_int4_summary('ssi_source','v',true)",
    ] {
        reader.batch_execute("BEGIN ISOLATION LEVEL SERIALIZABLE")?;
        let error = reader
            .query(sql, &[])
            .err()
            .ok_or("an unproven cached/capture SSI path was enabled")?;
        require(
            error.code().is_some_and(|code| code.code() == "0A000"),
            "cached/capture serializable rejection returned the wrong SQLSTATE",
        )?;
        reader.batch_execute("ROLLBACK")?;
    }
    admin.query_one("SELECT syntra_stop_capture('ssi_source')", &[])?;
    admin.batch_execute("DROP TABLE ssi_source")?;
    eprintln!("SSI checks: 33 fresh-read modes, 78 conflicting transaction pairs, eleven read-time aborts with backend reuse, three temporal repeatable-read snapshots and three cached/capture guards passed");
    Ok(vec![
        "serializable read-write, read-only and deferrable fresh reads without assigned XIDs",
        "source relation predicate locks, including empty and unmatched range reads",
        "write skew aborts one transaction with 40001 for custom and mixed native/custom reads",
        "empty-table and range-insertion phantoms abort one transaction with 40001",
        "read-time 40001 unwinds each native heap scan and permits reuse after rollback",
        "date, timestamp and timestamptz filters retain RR snapshots across concurrent predicate/value changes",
        "matching and fully filtered temporal scans honor cancellation and allow subsequent backend reuse",
        "integer, text and UUID decimal grouping preserve exact scale, RR key/value snapshots and cancellation recovery",
        "text and UUID grouping retain key/value snapshot pairs and permit reuse after cancellation",
        "text grouping refuses non-UTF8 databases while UUID grouping remains available",
        "native invariants checked after participant transactions, without masking custom dependencies",
        "capture, live reuse and checkpoint reuse remain explicitly unsupported under SSI",
    ])
}
