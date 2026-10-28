// numeric(p,s) feeds, stored as value * 10^s. The oracle is PostgreSQL's own
// aggregate over the same table, scaled by 10^s in SQL to compare with the stored
// integers that the snapshot engine sums.
fn numeric_oracle(table: &str, scale: u32) -> crate::source::WideSummary {
    let factor = 10_i64.pow(scale);
    // unslop-ignore NO_SQL_INTERPOLATION -- interpolates fixed test table names and an integer factor
    let (rows, non_nulls, sum) = Spi::get_three::<i64, i64, String>(&format!(
        "SELECT count(*),count(v),coalesce((sum(v)*{factor})::numeric(40,0)::text,'') FROM {table}"
    ))
    .unwrap();
    // unslop-ignore NO_SQL_INTERPOLATION -- interpolates fixed test table names and an integer factor
    let (min, max) = Spi::get_two::<i64, i64>(&format!(
        "SELECT (min(v)*{factor})::int8,(max(v)*{factor})::int8 FROM {table}"
    ))
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

// pgrx test transactions have an XID, so the SQL reader takes the heap route.
// Call the snapshot engine directly; width 0 selects the numeric reader.
fn numeric_snapshot(table: &str) -> Result<(crate::source::WideSummary, bool), &'static str> {
    let oid = Spi::get_one_with_args::<pg_sys::Oid>("SELECT $1::regclass::oid", &[table.into()])
        .unwrap()
        .unwrap();
    // SAFETY: this test resolved an existing relation and keeps its read lock.
    let source = unsafe { pgrx::PgRelation::with_lock(oid, pg_sys::AccessShareLock as i32) };
    let _snapshot = crate::capture::Snapshot::latest();
    crate::checkpoint::snapshot_summary(&source, crate::source::source_snapshot(), 2, 0, true)
}

// The SQL reader's numeric outputs, compared with PostgreSQL's in SQL, including
// the scale of the sum.
fn numeric_sql_matches(table: &str) -> Option<String> {
    // unslop-ignore NO_SQL_INTERPOLATION -- interpolates fixed test table names
    Spi::get_one::<String>(&format!("SELECT s.route||':'||(ROW(s.rows,s.non_nulls,s.sum,s.min,s.max) IS NOT DISTINCT FROM ROW(n.rows,n.non_nulls,n.sum,n.min,n.max))||':'||coalesce(scale(s.sum)=scale(n.sum),true) FROM syntra_checkpoint_numeric_summary('{table}','v',true) AS s,(SELECT count(*) AS rows,count(v) AS non_nulls,sum(v),min(v),max(v) FROM {table}) AS n")).unwrap()
}

#[pg_test]
fn numeric_feed_matches_postgresql_with_fractions_and_limits() {
    capture_fixture("CREATE TABLE money_feed(id int4 PRIMARY KEY,v numeric(10,2)); INSERT INTO money_feed SELECT g,g*1.25-1000 FROM generate_series(1,2000) AS g; INSERT INTO money_feed VALUES(-1,99999999.99),(-2,-99999999.99),(-3,NULL),(-4,0.01); SELECT syntra_seed_capture('money_feed','id','v',4096); SELECT syntra_apply_capture('money_feed')");
    assert_eq!(
        numeric_snapshot("money_feed"),
        Ok((numeric_oracle("money_feed", 2), false))
    );
    Spi::run("UPDATE money_feed SET v=-v WHERE id%7=0 AND id<=700; UPDATE money_feed SET v=NULL WHERE id%11=0 AND id<200; DELETE FROM money_feed WHERE id=-1; UPDATE money_feed SET id=5000 WHERE id=-2; INSERT INTO money_feed VALUES(6000,-0.05),(6001,12.3)").unwrap();
    assert_eq!(
        numeric_snapshot("money_feed"),
        Ok((numeric_oracle("money_feed", 2), true))
    );
    Spi::get_one::<i64>("SELECT applied FROM syntra_apply_prune_capture('money_feed')").unwrap();
    assert_eq!(
        numeric_snapshot("money_feed"),
        Ok((numeric_oracle("money_feed", 2), false))
    );
    assert_eq!(Spi::get_one::<String>("SELECT key_width||'/'||value_width FROM syntra.capture_bindings WHERE source_relation='money_feed'::regclass").unwrap(), Some("4/8".to_string()));
    assert_eq!(
        numeric_sql_matches("money_feed"),
        Some("heap:true:true".to_string())
    );
}

#[pg_test]
fn numeric_sums_past_int8_stay_exact() {
    capture_fixture("CREATE TABLE big_numeric(id int8 PRIMARY KEY,v numeric(18,0)); INSERT INTO big_numeric SELECT g,999999999999999999 FROM generate_series(1,30) AS g; INSERT INTO big_numeric VALUES(5000000000,-999999999999999999); SELECT syntra_seed_capture('big_numeric','id','v',64); SELECT syntra_apply_capture('big_numeric')");
    let checkpoint = numeric_snapshot("big_numeric").unwrap();
    assert_eq!(checkpoint, (numeric_oracle("big_numeric", 0), false));
    assert_eq!(checkpoint.0 .2, Some(28_999_999_999_999_999_971));
    Spi::run("UPDATE big_numeric SET v=-v WHERE id<=10; INSERT INTO big_numeric VALUES(-9000000000000000000,1)").unwrap();
    assert_eq!(
        numeric_snapshot("big_numeric"),
        Ok((numeric_oracle("big_numeric", 0), true))
    );
    assert_eq!(
        numeric_sql_matches("big_numeric"),
        Some("heap:true:true".to_string())
    );
}

#[pg_test]
fn numeric_churn_with_folds_matches_postgresql() {
    capture_fixture("CREATE TABLE numeric_churn(id int4 PRIMARY KEY,v numeric(12,3)); INSERT INTO numeric_churn SELECT g,g*1.001 FROM generate_series(1,2000) AS g; SELECT syntra_seed_capture('numeric_churn','id','v',4096); SELECT syntra_apply_capture('numeric_churn')");
    let (mut saw_delta, mut saw_fold) = (false, false);
    let mut previous = chunk_md5("numeric_churn");
    for round in 0..40_i64 {
        // unslop-ignore NO_SQL_INTERPOLATION -- interpolates only the integer loop counter
        Spi::run(&format!(
            "INSERT INTO numeric_churn SELECT 10000+{round}*100+g,(g-50)*0.125 FROM generate_series(1,60) AS g;
             UPDATE numeric_churn SET v=CASE WHEN id%3=0 THEN NULL ELSE -v END WHERE id%41={round}%41;
             DELETE FROM numeric_churn WHERE v=(SELECT max(v) FROM numeric_churn) OR v=(SELECT min(v) FROM numeric_churn);
             UPDATE numeric_churn SET id=id+1000000 WHERE id IN (SELECT id FROM numeric_churn WHERE id<1000000 ORDER BY id OFFSET {round} LIMIT 2)"
        ))
        .unwrap();
        Spi::run("SELECT syntra_apply_prune_capture('numeric_churn')").unwrap();
        assert_eq!(
            numeric_snapshot("numeric_churn"),
            Ok((numeric_oracle("numeric_churn", 3), false)),
            "round {round}"
        );
        saw_delta |= deltas("numeric_churn") > 0;
        let now = chunk_md5("numeric_churn");
        saw_fold |= now != previous;
        previous = now;
    }
    assert!(saw_delta && saw_fold, "delta {saw_delta}, fold {saw_fold}");
}

#[pg_test]
fn the_numeric_heap_route_returns_nan_as_postgresql_does() {
    Spi::run("CREATE TABLE nan_values(id int4 PRIMARY KEY,v numeric(6,2)); INSERT INTO nan_values VALUES(1,'NaN'),(2,-3.5),(3,NULL),(4,7.25)").unwrap();
    assert_eq!(
        numeric_sql_matches("nan_values"),
        Some("heap:true:true".to_string())
    );
    assert_eq!(Spi::get_one::<String>("SELECT concat_ws(',',non_nulls,sum,min,max,reason) FROM syntra_checkpoint_numeric_summary('nan_values','v')").unwrap(), Some("3,NaN,-3.50,NaN,transaction_has_xid".to_string()));
    Spi::run("DELETE FROM nan_values WHERE id<>1").unwrap();
    assert_eq!(
        numeric_sql_matches("nan_values"),
        Some("heap:true:true".to_string())
    );
}

#[pg_test(error = "capture cannot store NaN in a numeric column")]
fn a_numeric_feed_refuses_a_nan_write() {
    capture_fixture("CREATE TABLE nan_feed(id int4 PRIMARY KEY,v numeric(6,2)); INSERT INTO nan_feed VALUES(1,1); SELECT syntra_seed_capture('nan_feed','id','v',16)");
    Spi::run("UPDATE nan_feed SET v='NaN' WHERE id=1").unwrap();
}

#[pg_test(error = "capture cannot store NaN in a numeric column")]
fn seeding_refuses_a_stored_nan() {
    capture_fixture("CREATE TABLE nan_seed(id int4 PRIMARY KEY,v numeric(6,2)); INSERT INTO nan_seed VALUES(1,'NaN')");
    Spi::run("SELECT syntra_seed_capture('nan_seed','id','v',16)").unwrap();
}

#[pg_test(error = "capture requires numeric(p,s) with p from 1 to 18 and s from 0 to p")]
fn seeding_refuses_unconstrained_numeric() {
    capture_fixture("CREATE TABLE loose_numeric(id int4 PRIMARY KEY,v numeric)");
    Spi::run("SELECT syntra_seed_capture('loose_numeric','id','v',16)").unwrap();
}

#[pg_test(error = "capture requires numeric(p,s) with p from 1 to 18 and s from 0 to p")]
fn seeding_refuses_numeric_past_18_digits() {
    capture_fixture("CREATE TABLE wide_numeric(id int4 PRIMARY KEY,v numeric(19,0))");
    Spi::run("SELECT syntra_seed_capture('wide_numeric','id','v',16)").unwrap();
}

#[pg_test(
    error = "capture requires stored, non-generated int2, int4 or int8 key and value columns"
)]
fn seeding_refuses_a_numeric_key() {
    capture_fixture("CREATE TABLE numeric_key(id numeric(10,0) PRIMARY KEY,v int4)");
    Spi::run("SELECT syntra_seed_capture('numeric_key','id','v',16)").unwrap();
}

// A stored value is held to the column's precision: 100.00 cannot be numeric(4,2).
#[pg_test(error = "invalid_capture_value")]
fn a_numeric_feed_rejects_a_stored_value_past_its_precision() {
    capture_fixture("CREATE TABLE numeric_corrupt(id int4 PRIMARY KEY,v numeric(4,2)); INSERT INTO numeric_corrupt VALUES(1,1); SELECT syntra_seed_capture('numeric_corrupt','id','v',16); SELECT syntra_apply_capture('numeric_corrupt'); UPDATE numeric_corrupt SET v=2 WHERE id=1; UPDATE syntra.capture_events SET after_value=10000 WHERE source_relation='numeric_corrupt'::regclass");
    Spi::run("SELECT syntra_apply_capture('numeric_corrupt')").unwrap();
}

// Raising precision needs no table rewrite in PostgreSQL, but it changes the
// column's typmod, so the feed fails closed until it is reseeded.
#[pg_test(error = "capture configuration is missing or changed; reseed before writing")]
fn widening_a_captured_numeric_column_fails_closed() {
    capture_fixture("CREATE TABLE numeric_widen(id int4 PRIMARY KEY,v numeric(6,2)); INSERT INTO numeric_widen VALUES(1,1); SELECT syntra_seed_capture('numeric_widen','id','v',16); ALTER TABLE numeric_widen ALTER COLUMN v TYPE numeric(8,2)");
    Spi::run("INSERT INTO numeric_widen VALUES(2,123456.78)").unwrap();
}
