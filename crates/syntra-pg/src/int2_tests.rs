// int2 feeds, stored at 4 bytes. The oracle is PostgreSQL's own aggregate over the
// same table (sql_oracle, int8_tests.rs); sums run far past the int2 range.
#[pg_test]
fn int2_feed_matches_postgresql_at_the_type_limits() {
    capture_fixture("CREATE TABLE small_feed(id int2 PRIMARY KEY,v int2); INSERT INTO small_feed SELECT g,g FROM generate_series(1,2000) AS g; INSERT INTO small_feed VALUES(-32768,32767),(32767,-32768),(-1,NULL); SELECT syntra_seed_capture('small_feed','id','v',4096); SELECT syntra_apply_capture('small_feed')");
    assert_eq!(
        tail_snapshot("small_feed"),
        Ok((sql_oracle("small_feed"), false))
    );
    assert_eq!(tail_snapshot("small_feed").unwrap().0 .2, Some(2_000_999));
    Spi::run("UPDATE small_feed SET v=-v WHERE id%7=0 AND id BETWEEN 1 AND 700; DELETE FROM small_feed WHERE id=-32768; UPDATE small_feed SET id=-2 WHERE id=32767; INSERT INTO small_feed VALUES(30000,32767),(30001,NULL)").unwrap();
    assert_eq!(
        tail_snapshot("small_feed"),
        Ok((sql_oracle("small_feed"), true))
    );
    Spi::get_one::<i64>("SELECT applied FROM syntra_apply_prune_capture('small_feed')").unwrap();
    assert_eq!(
        tail_snapshot("small_feed"),
        Ok((sql_oracle("small_feed"), false))
    );
    assert_eq!(Spi::get_one::<String>("SELECT key_width||'/'||value_width FROM syntra.capture_bindings WHERE source_relation='small_feed'::regclass").unwrap(), Some("4/4".to_string()));
    // The SQL reader takes the heap inside this test's transaction and must still
    // match PostgreSQL's sum(int2), which is bigint, and min and max.
    assert_eq!(Spi::get_one::<bool>("SELECT ROW(s.rows,s.non_nulls,s.sum,s.min,s.max) IS NOT DISTINCT FROM ROW(n.rows,n.non_nulls,n.sum,n.min,n.max) AND s.route='heap' FROM syntra_checkpoint_int4_summary('small_feed','v',true) AS s,(SELECT count(*) AS rows,count(v) AS non_nulls,sum(v),min(v),max(v) FROM small_feed) AS n").unwrap(), Some(true));
}

#[pg_test]
fn int2_churn_with_folds_matches_postgresql() {
    capture_fixture("CREATE TABLE small_churn(id int2 PRIMARY KEY,v int2); INSERT INTO small_churn SELECT g,g FROM generate_series(1,2000) AS g; SELECT syntra_seed_capture('small_churn','id','v',4096); SELECT syntra_apply_capture('small_churn')");
    let (mut saw_delta, mut saw_fold) = (false, false);
    let mut previous = chunk_md5("small_churn");
    for round in 0..40_i64 {
        // Keys stay below 32,768: inserts reach 12,460 and moves add 20,000 once.
        // unslop-ignore NO_SQL_INTERPOLATION -- interpolates only the integer loop counter
        Spi::run(&format!(
            "INSERT INTO small_churn SELECT 10000+{round}*60+g,g-50 FROM generate_series(1,60) AS g;
             UPDATE small_churn SET v=CASE WHEN id%3=0 THEN NULL ELSE -v END WHERE id%41={round}%41;
             DELETE FROM small_churn WHERE v=(SELECT max(v) FROM small_churn) OR v=(SELECT min(v) FROM small_churn);
             UPDATE small_churn SET id=id+20000 WHERE id IN (SELECT id FROM small_churn WHERE id<12768 ORDER BY id OFFSET {round} LIMIT 2)"
        ))
        .unwrap();
        Spi::run("SELECT syntra_apply_prune_capture('small_churn')").unwrap();
        assert_eq!(
            tail_snapshot("small_churn"),
            Ok((sql_oracle("small_churn"), false)),
            "round {round}"
        );
        saw_delta |= deltas("small_churn") > 0;
        let now = chunk_md5("small_churn");
        saw_fold |= now != previous;
        previous = now;
    }
    assert!(saw_delta && saw_fold, "delta {saw_delta}, fold {saw_fold}");
}

#[pg_test]
fn mixed_int2_feeds_replay_and_checkpoint_exactly() {
    capture_fixture("CREATE TABLE small_key(id int2 PRIMARY KEY,v int8); INSERT INTO small_key SELECT g,g*5000000000 FROM generate_series(1,300) AS g; SELECT syntra_seed_capture('small_key','id','v',512); SELECT syntra_apply_capture('small_key'); CREATE TABLE small_value(id int8 PRIMARY KEY,v int2); INSERT INTO small_value SELECT g*5000000000,(g*97)%32767 FROM generate_series(1,300) AS g; SELECT syntra_seed_capture('small_value','id','v',512); SELECT syntra_apply_capture('small_value')");
    for (table, width) in [("small_key", 8), ("small_value", 4)] {
        assert_eq!(
            snapshot8(table, width),
            Ok((sql_oracle(table), false)),
            "{table}"
        );
    }
    Spi::run("UPDATE small_key SET v=-v WHERE id%7=0; DELETE FROM small_key WHERE id>290; UPDATE small_value SET v=NULL WHERE id<50000000000; UPDATE small_value SET v=-32768 WHERE id=100000000000; UPDATE small_value SET id=id+1 WHERE v=291").unwrap();
    for (table, width) in [("small_key", 8), ("small_value", 4)] {
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
    assert_eq!(Spi::get_one::<String>("SELECT string_agg(key_width||'/'||value_width,' ' ORDER BY source_relation::regclass::text) FROM syntra.capture_bindings WHERE source_relation IN('small_key'::regclass,'small_value'::regclass)").unwrap(), Some("4/8 8/4".to_string()));
}

// Stored at 4 bytes, an int2 feed must still refuse what its column cannot hold.
#[pg_test(error = "invalid_capture_value")]
fn an_int2_feed_rejects_a_stored_event_outside_int2() {
    capture_fixture("CREATE TABLE small_corrupt(id int2 PRIMARY KEY,v int2); INSERT INTO small_corrupt VALUES(1,1); SELECT syntra_seed_capture('small_corrupt','id','v',16); SELECT syntra_apply_capture('small_corrupt'); UPDATE small_corrupt SET v=2 WHERE id=1; UPDATE syntra.capture_events SET after_value=40000 WHERE source_relation='small_corrupt'::regclass");
    Spi::run("SELECT syntra_apply_capture('small_corrupt')").unwrap();
}

#[pg_test(error = "invalid_capture_events")]
fn an_int2_feed_rejects_a_stored_key_outside_int2() {
    capture_fixture("CREATE TABLE small_key_corrupt(id int2 PRIMARY KEY,v int2); INSERT INTO small_key_corrupt VALUES(1,1); SELECT syntra_seed_capture('small_key_corrupt','id','v',16); SELECT syntra_apply_capture('small_key_corrupt'); UPDATE small_key_corrupt SET v=2 WHERE id=1; UPDATE syntra.capture_events SET key=40000 WHERE source_relation='small_key_corrupt'::regclass");
    Spi::run("SELECT syntra_apply_capture('small_key_corrupt')").unwrap();
}

#[pg_test(error = "capture configuration is missing or changed; reseed before writing")]
fn widening_a_captured_int2_column_fails_closed() {
    capture_fixture("CREATE TABLE small_widen(id int4 PRIMARY KEY,v int2); INSERT INTO small_widen VALUES(1,1); SELECT syntra_seed_capture('small_widen','id','v',16); ALTER TABLE small_widen ALTER COLUMN v TYPE int4");
    Spi::run("INSERT INTO small_widen VALUES(2,70000)").unwrap();
}
