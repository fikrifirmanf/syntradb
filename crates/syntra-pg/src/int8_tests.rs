// int8 feeds. Values beyond int4 and sums beyond bigint exercise both new widths;
// the oracle is PostgreSQL's own aggregate over the same table.
fn sql_oracle(table: &str) -> crate::source::WideSummary {
    let (rows, non_nulls, sum) = Spi::get_three::<i64, i64, String>(&format!(
        "SELECT count(*),count(v),coalesce(sum(v)::text,'') FROM {table}"
    ))
    .unwrap();
    let (min, max) =
        Spi::get_two::<i64, i64>(&format!("SELECT min(v)::int8,max(v)::int8 FROM {table}"))
            .unwrap();
    let sum = sum.unwrap();
    (
        rows.unwrap(),
        non_nulls.unwrap(),
        (!sum.is_empty()).then(|| sum.parse().unwrap()),
        min,
        max,
    )
}

// pgrx test transactions have an XID, so the SQL readers take the heap route.
// Call the snapshot engine directly, as tail_snapshot does for int4.
fn snapshot8(table: &str, width: i16) -> Result<(crate::source::WideSummary, bool), &'static str> {
    let oid = Spi::get_one_with_args::<pg_sys::Oid>("SELECT $1::regclass::oid", &[table.into()])
        .unwrap()
        .unwrap();
    // SAFETY: this test resolved an existing relation and keeps its read lock.
    let source = unsafe { pgrx::PgRelation::with_lock(oid, pg_sys::AccessShareLock as i32) };
    let _snapshot = crate::capture::Snapshot::latest();
    crate::checkpoint::snapshot_summary(&source, crate::source::source_snapshot(), 2, width, true)
}

#[pg_test]
fn int8_feed_checkpoint_and_replay_match_postgresql_beyond_bigint_sums() {
    capture_fixture("CREATE TABLE wide_values(id int8 PRIMARY KEY,v int8); INSERT INTO wide_values VALUES(1,9000000000000000000),(3000000000,9000000000000000000),(-5,NULL),(-9000000000000000000,-7); SELECT syntra_seed_capture('wide_values','id','v',64); SELECT syntra_apply_capture('wide_values')");
    let checkpoint = snapshot8("wide_values", 8).unwrap();
    assert_eq!(checkpoint, (sql_oracle("wide_values"), false));
    // 2 * 9e18 - 7 does not fit bigint; PostgreSQL returns it as numeric.
    assert_eq!(checkpoint.0 .2, Some(17_999_999_999_999_999_993));
    Spi::run("UPDATE wide_values SET v=v-1 WHERE id=3000000000; DELETE FROM wide_values WHERE id=1; INSERT INTO wide_values VALUES(9223372036854775807,-9223372036854775808); UPDATE wide_values SET id=4000000000 WHERE id=-5").unwrap();
    let replayed = snapshot8("wide_values", 8).unwrap();
    assert_eq!(replayed, (sql_oracle("wide_values"), true));
    assert_eq!(replayed.0 .3, Some(i64::MIN));
    Spi::get_one::<i64>("SELECT applied FROM syntra_apply_prune_capture('wide_values')").unwrap();
    assert_eq!(
        snapshot8("wide_values", 8),
        Ok((sql_oracle("wide_values"), false))
    );
    assert_eq!(Spi::get_one::<bool>("SELECT key_width=8 AND value_width=8 FROM syntra.capture_bindings WHERE source_relation='wide_values'::regclass").unwrap(), Some(true));
}

#[pg_test]
fn mixed_width_feeds_replay_and_checkpoint_exactly() {
    capture_fixture("CREATE TABLE narrow_key(id int4 PRIMARY KEY,v int8); INSERT INTO narrow_key SELECT g,g*5000000000 FROM generate_series(1,300) AS g; SELECT syntra_seed_capture('narrow_key','id','v',512); SELECT syntra_apply_capture('narrow_key'); CREATE TABLE wide_key(id int8 PRIMARY KEY,v int4); INSERT INTO wide_key SELECT g*5000000000,g FROM generate_series(1,300) AS g; SELECT syntra_seed_capture('wide_key','id','v',512); SELECT syntra_apply_capture('wide_key')");
    for (table, width) in [("narrow_key", 8), ("wide_key", 4)] {
        assert_eq!(
            snapshot8(table, width),
            Ok((sql_oracle(table), false)),
            "{table}"
        );
    }
    Spi::run("UPDATE narrow_key SET v=-v WHERE id%7=0; DELETE FROM narrow_key WHERE id>290; UPDATE wide_key SET v=NULL WHERE id<50000000000; UPDATE wide_key SET id=id+1 WHERE v=200").unwrap();
    for (table, width) in [("narrow_key", 8), ("wide_key", 4)] {
        assert_eq!(
            snapshot8(table, width),
            Ok((sql_oracle(table), true)),
            "{table}"
        );
        Spi::get_one_with_args::<i64>(
            "SELECT applied FROM syntra_apply_prune_capture($1::regclass)",
            &[table.into()],
        )
        .unwrap();
        assert_eq!(
            snapshot8(table, width),
            Ok((sql_oracle(table), false)),
            "{table}"
        );
    }
    assert_eq!(Spi::get_one::<String>("SELECT string_agg(key_width||'/'||value_width,' ' ORDER BY source_relation::regclass::text) FROM syntra.capture_bindings WHERE source_relation IN('narrow_key'::regclass,'wide_key'::regclass)").unwrap(), Some("4/8 8/4".to_string()));
}

#[pg_test]
fn int8_summary_function_matches_sql_aggregates() {
    capture_fixture("CREATE TABLE wide_sql(id int8 PRIMARY KEY,v int8); INSERT INTO wide_sql VALUES(1,9000000000000000000),(2,9000000000000000000),(3,NULL); SELECT syntra_seed_capture('wide_sql','id','v',16); SELECT syntra_apply_capture('wide_sql')");
    assert_eq!(Spi::get_one::<bool>("SELECT ROW(s.rows,s.non_nulls,s.sum,s.min,s.max) IS NOT DISTINCT FROM ROW(n.rows,n.non_nulls,n.sum,n.min,n.max) FROM syntra_checkpoint_int8_summary('wide_sql','v',true) AS s,(SELECT count(*) AS rows,count(v) AS non_nulls,sum(v),min(v),max(v) FROM wide_sql) AS n").unwrap(), Some(true));
    assert_eq!(Spi::get_one::<String>("SELECT sum::text||' '||route||' '||reason FROM syntra_checkpoint_int8_summary('wide_sql','v')").unwrap(), Some("18000000000000000000 heap transaction_has_xid".to_string()));
}

#[pg_test(error = "source capture requires a stored, non-generated int2 or int4 column")]
fn int4_reader_rejects_an_int8_column() {
    capture_fixture("CREATE TABLE wide_reader(id int8 PRIMARY KEY,v int8); SELECT syntra_seed_capture('wide_reader','id','v',16)");
    Spi::run("SELECT rows FROM syntra_checkpoint_int4_summary('wide_reader','v')").unwrap();
}

#[pg_test(error = "source capture requires a stored, non-generated int8 column")]
fn int8_reader_rejects_an_int4_column() {
    capture_fixture("CREATE TABLE narrow_reader(id int4 PRIMARY KEY,v int4); SELECT syntra_seed_capture('narrow_reader','id','v',16)");
    Spi::run("SELECT rows FROM syntra_checkpoint_int8_summary('narrow_reader','v')").unwrap();
}

#[pg_test(
    error = "capture requires a stored, non-generated int2, int4, int8 or numeric(p,s) value column"
)]
fn capture_rejects_other_numeric_types() {
    capture_fixture("CREATE TABLE float_values(id int4 PRIMARY KEY,v float8); SELECT syntra_seed_capture('float_values','id','v',16)");
}

#[pg_test(error = "invalid_capture_value")]
fn an_int4_feed_rejects_a_stored_event_outside_int4() {
    capture_fixture("CREATE TABLE narrow_corrupt(id int4 PRIMARY KEY,v int4); INSERT INTO narrow_corrupt VALUES(1,1); SELECT syntra_seed_capture('narrow_corrupt','id','v',16); SELECT syntra_apply_capture('narrow_corrupt'); UPDATE narrow_corrupt SET v=2; UPDATE syntra.capture_events SET after_value=5000000000 WHERE source_relation='narrow_corrupt'::regclass");
    Spi::run("SELECT syntra_apply_capture('narrow_corrupt')").unwrap();
}

#[pg_test(error = "capture configuration is missing or changed; reseed before writing")]
fn a_binding_width_that_disagrees_with_the_column_fails_closed() {
    capture_fixture("CREATE TABLE width_tamper(id int4 PRIMARY KEY,v int4); SELECT syntra_seed_capture('width_tamper','id','v',16); UPDATE syntra.capture_bindings SET value_width=8 WHERE source_relation='width_tamper'::regclass");
    Spi::run("INSERT INTO width_tamper VALUES(1,1)").unwrap();
}
