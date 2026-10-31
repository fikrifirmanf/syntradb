#[pg_test]
fn time_summary_all_temporal_and_value_layouts_match_native() {
    Spi::run("SET LOCAL TIME ZONE 'UTC'").unwrap();
    Spi::run("CREATE TEMP TABLE time_layouts(d date,ts timestamp,tz timestamptz,v2 int2,v4 int4,v8 int8,n numeric(18,2));
        INSERT INTO time_layouts VALUES
        ('2026-01-01','2026-01-01','2026-01-01+00',-32768,-2147483648,9223372036854775807,9999999999999999.99),
        ('2026-01-01','2026-01-01 23:59:59.999999','2026-01-01 23:59:59.999999+00',32767,2147483647,9223372036854775807,0.01),
        ('2026-01-01','2026-01-01 12:00','2026-01-01 12:00+00',NULL,NULL,NULL,NULL),
        ('2026-01-02','2026-01-02','2026-01-02+00',8,8,8,8),
        (NULL,NULL,NULL,9,9,9,9)").unwrap();
    for (function, time, kind) in [
        ("syntra_heap_date_summary", "d", "date"),
        ("syntra_heap_timestamp_summary", "ts", "timestamp"),
        ("syntra_heap_timestamptz_summary", "tz", "timestamptz"),
    ] {
        for value in ["v2", "v4", "v8", "n"] {
            // unslop-ignore NO_SQL_INTERPOLATION -- fixture identifiers/types are fixed literals above, never caller data
            let sql = format!("SELECT ROW(s.rows,s.non_nulls,s.sum,s.min,s.max) IS NOT DISTINCT FROM ROW(n.rows,n.non_nulls,n.sum,n.min,n.max) AND s.rows=3 AND s.non_nulls=2 FROM {function}('time_layouts','{value}','{time}','2026-01-01'::{kind},'2026-01-02'::{kind}) AS s, (SELECT count(*) AS rows,count({value}) AS non_nulls,sum({value})::numeric AS sum,min({value})::numeric AS min,max({value})::numeric AS max FROM time_layouts WHERE {time}>='2026-01-01'::{kind} AND {time}<'2026-01-02'::{kind}) AS n LIMIT 1");
            assert_eq!(
                Spi::get_one::<bool>(&sql).unwrap(),
                Some(true),
                "{function}/{value}"
            );
        }
    }
}

#[pg_test]
fn timestamp_filter_empty_reversed_null_and_boundary_semantics() {
    Spi::run("CREATE TEMP TABLE time_edges(t timestamp,v int4); INSERT INTO time_edges VALUES
        ('2026-01-01',10),('2026-01-01 00:00:00.000001',20),('2026-01-01 00:00:00.000002',40),(NULL,80)").unwrap();
    assert_eq!(Spi::get_one::<bool>("SELECT rows=2 AND sum=30 AND min=10 AND max=20 FROM syntra_heap_timestamp_summary('time_edges','v','t','2026-01-01','2026-01-01 00:00:00.000002') LIMIT 1").unwrap(),Some(true));
    assert_eq!(Spi::get_one::<bool>("SELECT rows=0 AND non_nulls=0 AND sum IS NULL AND min IS NULL AND max IS NULL FROM syntra_heap_timestamp_summary('time_edges','v','t','2026-01-02','2026-01-01') LIMIT 1").unwrap(),Some(true));
    assert_eq!(Spi::get_one::<bool>("SELECT rows=0 AND sum IS NULL FROM syntra_heap_timestamp_summary('time_edges','v','t','2026-01-01','2026-01-01') LIMIT 1").unwrap(),Some(true));
    assert_eq!(Spi::get_one::<i64>("SELECT count(*) FROM syntra_heap_timestamp_summary('time_edges','v','t',NULL,'2026-01-02')").unwrap(),Some(0));
    Spi::run("TRUNCATE time_edges").unwrap();
    assert_eq!(Spi::get_one::<bool>("SELECT rows=0 AND non_nulls=0 AND sum IS NULL FROM syntra_heap_timestamp_summary('time_edges','v','t','-infinity','infinity') LIMIT 1").unwrap(),Some(true));
}

#[pg_test]
fn timestamptz_filter_preserves_dst_instants_across_session_timezones() {
    Spi::run("CREATE TEMP TABLE time_dst(t timestamptz,v numeric(10,2));
        INSERT INTO time_dst VALUES ('2026-11-01 01:30-04',1.25),('2026-11-01 01:30-05',2.50),('2026-11-01 02:00-05',8.00);
        SET LOCAL TIME ZONE 'America/New_York'").unwrap();
    let sql="SELECT rows=2 AND sum=3.75 AND min=1.25 AND max=2.50 FROM syntra_heap_timestamptz_summary('time_dst','v','t','2026-11-01 01:00-04','2026-11-01 02:00-05') LIMIT 1";
    assert_eq!(Spi::get_one::<bool>(sql).unwrap(), Some(true));
    Spi::run("SET LOCAL TIME ZONE 'Asia/Jakarta'").unwrap();
    assert_eq!(Spi::get_one::<bool>(sql).unwrap(), Some(true));
    assert_eq!(Spi::get_one::<bool>("SELECT s.sum IS NOT DISTINCT FROM n.sum FROM syntra_heap_timestamptz_summary('time_dst','v','t','2026-11-01 01:00-05','2026-11-01 02:00-05') s,(SELECT sum(v) FROM time_dst WHERE t>='2026-11-01 01:00-05'::timestamptz AND t<'2026-11-01 02:00-05'::timestamptz) n LIMIT 1").unwrap(),Some(true));
}

#[pg_test]
fn temporal_infinities_and_bc_dates_match_native_comparisons() {
    Spi::run("CREATE TEMP TABLE time_infinity(d date,t timestamp,z timestamptz,v int8);
        INSERT INTO time_infinity VALUES ('-infinity','-infinity','-infinity',1),('0001-01-01 BC','0001-01-01 BC','0001-01-01 BC',2),('infinity','infinity','infinity',8)").unwrap();
    assert_eq!(Spi::get_one::<bool>("SELECT rows=2 AND sum=3 FROM syntra_heap_date_summary('time_infinity','v','d','-infinity','infinity') LIMIT 1").unwrap(),Some(true));
    assert_eq!(Spi::get_one::<bool>("SELECT rows=2 AND sum=3 FROM syntra_heap_timestamp_summary('time_infinity','v','t','-infinity','infinity') LIMIT 1").unwrap(),Some(true));
    assert_eq!(Spi::get_one::<bool>("SELECT rows=2 AND sum=3 FROM syntra_heap_timestamptz_summary('time_infinity','v','z','-infinity','infinity') LIMIT 1").unwrap(),Some(true));
}

#[pg_test]
fn temporal_numeric_nan_and_null_aggregates_match_postgresql() {
    Spi::run("CREATE TEMP TABLE time_nan(t date,v numeric(18,2)); INSERT INTO time_nan VALUES ('2026-01-01','NaN'),('2026-01-01',-1.25),('2026-01-01',NULL),('2026-01-02','NaN'),('2026-01-03',NULL)").unwrap();
    for day in ["2026-01-01", "2026-01-02", "2026-01-03"] {
        // unslop-ignore NO_SQL_INTERPOLATION -- date is one of three fixed fixture literals
        let sql=format!("SELECT ROW(s.rows,s.non_nulls,s.sum,s.min,s.max) IS NOT DISTINCT FROM ROW(n.rows,n.non_nulls,n.sum,n.min,n.max) FROM syntra_heap_date_summary('time_nan','v','t','{day}'::date,'{day}'::date+1) s,(SELECT count(*) rows,count(v) non_nulls,sum(v),min(v),max(v) FROM time_nan WHERE t='{day}'::date) n LIMIT 1");
        assert_eq!(Spi::get_one::<bool>(&sql).unwrap(), Some(true), "{day}");
    }
}

#[pg_test]
fn temporal_reads_keep_statement_snapshot_and_see_prior_own_writes() {
    Spi::run(
        "CREATE TEMP TABLE time_own(t date,v int4); INSERT INTO time_own VALUES ('2026-01-01',10)",
    )
    .unwrap();
    assert_eq!(Spi::get_one::<bool>("WITH changed AS (UPDATE time_own SET v=20 WHERE v=10 RETURNING v) SELECT c.v=20 AND s.sum=10 FROM changed c,LATERAL syntra_heap_date_summary('time_own','v','t','2026-01-01','2026-01-02') s LIMIT 1").unwrap(),Some(true));
    assert_eq!(Spi::get_one::<i64>("SELECT sum::bigint FROM syntra_heap_date_summary('time_own','v','t','2026-01-01','2026-01-02') LIMIT 1").unwrap(),Some(20));
}

#[pg_test(error = "source capture requires schema USAGE and table SELECT privileges")]
fn temporal_scan_checks_source_acl_before_metadata() {
    Spi::run("CREATE ROLE time_denied NOLOGIN; CREATE TABLE public.time_private(t timestamp,v int4); SET LOCAL ROLE time_denied").unwrap();
    Spi::run("SELECT rows FROM syntra_heap_timestamp_summary('public.time_private','v','t','2026-01-01','2026-01-02') LIMIT 1").unwrap();
}

#[pg_test(error = "source capture does not support row-level security")]
fn temporal_scan_rejects_rls() {
    Spi::run(
        "CREATE TABLE time_rls(t date,v int4); ALTER TABLE time_rls ENABLE ROW LEVEL SECURITY",
    )
    .unwrap();
    Spi::run("SELECT rows FROM syntra_heap_date_summary('time_rls','v','t','2026-01-01','2026-01-02') LIMIT 1").unwrap();
}

#[pg_test(error = "time filter column must be stored and match the bounds' date or timestamp type")]
fn temporal_scan_refuses_implicit_timestamp_zone_casts() {
    Spi::run("CREATE TEMP TABLE time_wrong(t timestamp,v int4)").unwrap();
    Spi::run("SELECT rows FROM syntra_heap_timestamptz_summary('time_wrong','v','t','2026-01-01','2026-01-02') LIMIT 1").unwrap();
}

#[pg_test(error = "time summaries require a stored integer or fixed-scale numeric value column")]
fn temporal_scan_rejects_float_values() {
    Spi::run("CREATE TEMP TABLE time_float(t date,v float8)").unwrap();
    Spi::run("SELECT rows FROM syntra_heap_date_summary('time_float','v','t','2026-01-01','2026-01-02') LIMIT 1").unwrap();
}

#[pg_test(error = "time summaries require numeric(p,s) with 1 <= p <= 18 and 0 <= s <= p")]
fn temporal_scan_refuses_unbounded_numeric() {
    Spi::run("CREATE TEMP TABLE time_numeric(t date,v numeric)").unwrap();
    Spi::run("SELECT rows FROM syntra_heap_date_summary('time_numeric','v','t','2026-01-01','2026-01-02') LIMIT 1").unwrap();
}

#[pg_test(error = "time filter column must be stored and match the bounds' date or timestamp type")]
fn temporal_scan_refuses_generated_time_columns() {
    Spi::run("CREATE TEMP TABLE time_generated(v int4,t date GENERATED ALWAYS AS (date '2026-01-01') STORED)").unwrap();
    Spi::run("SELECT rows FROM syntra_heap_date_summary('time_generated','v','t','2026-01-01','2026-01-02') LIMIT 1").unwrap();
}
