#[pg_test]
fn fresh_heap_aggregates_match_native_sql_for_nulls_bounds_and_ranges() {
    Spi::run("CREATE TEMP TABLE fresh_values (v int4); INSERT INTO fresh_values (v) VALUES (NULL), (-2147483648), (-1), (0), (7), (2147483647)").unwrap();
    assert_eq!(Spi::get_one::<bool>("SELECT ROW(s.rows,s.non_nulls,s.sum,s.min,s.max) IS NOT DISTINCT FROM ROW(n.rows,n.non_nulls,n.sum,n.min,n.max) FROM syntra_heap_int4_summary('fresh_values','v') AS s, (SELECT count(*) AS rows,count(v) AS non_nulls,sum(v),min(v),max(v) FROM fresh_values) AS n LIMIT 1").unwrap(), Some(true));
    assert_eq!(Spi::get_one::<bool>("SELECT ROW(s.rows,s.non_nulls,s.sum,s.min,s.max) IS NOT DISTINCT FROM ROW(n.rows,n.non_nulls,n.sum,n.min,n.max) FROM syntra_heap_int4_range_summary('fresh_values','v',-1,7) AS s, (SELECT count(*) AS rows,count(v) AS non_nulls,sum(v),min(v),max(v) FROM fresh_values WHERE v BETWEEN -1 AND 7) AS n LIMIT 1").unwrap(), Some(true));
    assert_eq!(Spi::get_one::<bool>("SELECT rows = 0 AND non_nulls = 0 AND sum IS NULL AND min IS NULL AND max IS NULL FROM syntra_heap_int4_range_summary('fresh_values','v',7,-1) LIMIT 1").unwrap(), Some(true));
}

#[pg_test]
fn fresh_heap_empty_null_and_strict_arguments_match_sql() {
    Spi::run("CREATE TEMP TABLE fresh_empty (v int4)").unwrap();
    assert_eq!(Spi::get_one::<bool>("SELECT rows = 0 AND non_nulls = 0 AND sum IS NULL AND min IS NULL AND max IS NULL FROM syntra_heap_int4_summary('fresh_empty','v') LIMIT 1").unwrap(), Some(true));
    Spi::run("INSERT INTO fresh_empty (v) VALUES (NULL), (NULL)").unwrap();
    assert_eq!(Spi::get_one::<bool>("SELECT rows = 2 AND non_nulls = 0 AND sum IS NULL FROM syntra_heap_int4_summary('fresh_empty','v') LIMIT 1").unwrap(), Some(true));
    assert_eq!(
        Spi::get_one::<i64>("SELECT count(*) FROM syntra_heap_int4_summary(NULL::regclass,'v')")
            .unwrap(),
        Some(0)
    );
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT count(*) FROM syntra_heap_int4_range_summary('fresh_empty','v',NULL,1)"
        )
        .unwrap(),
        Some(0)
    );
}

#[pg_test]
fn fresh_heap_sees_own_prior_writes_but_keeps_a_modifying_cte_snapshot() {
    Spi::run("CREATE TABLE fresh_commands (v int4); INSERT INTO fresh_commands (v) VALUES (10); SELECT syntra_track_source('fresh_commands','v')").unwrap();
    assert_eq!(Spi::get_one::<bool>("WITH changed AS (UPDATE fresh_commands SET v = 20 WHERE v = 10 RETURNING v) SELECT c.v = 20 AND s.sum = 10 AND (SELECT sum(v) FROM fresh_commands) = 10 FROM changed AS c, LATERAL syntra_heap_int4_summary('fresh_commands','v') AS s LIMIT 1").unwrap(), Some(true));
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT sum FROM syntra_heap_int4_summary('fresh_commands','v') LIMIT 1"
        )
        .unwrap(),
        Some(20)
    );
}

#[pg_test]
fn fresh_heap_streams_beyond_the_segment_row_cap() {
    Spi::run("CREATE TEMP TABLE fresh_large (v int4); INSERT INTO fresh_large (v) SELECT n FROM generate_series(1,1000001) AS n").unwrap();
    assert_eq!(Spi::get_one::<bool>("SELECT rows = 1000001 AND sum = 500001500001 AND min = 1 AND max = 1000001 FROM syntra_heap_int4_summary('fresh_large','v') LIMIT 1").unwrap(), Some(true));
    assert_eq!(Spi::get_one::<bool>("SELECT rows = 0 AND sum IS NULL FROM syntra_heap_int4_range_summary('fresh_large','v',-2147483648,-1) LIMIT 1").unwrap(), Some(true));
}

#[pg_test]
fn fresh_heap_native_range_keys_cover_extreme_values_and_nulls() {
    Spi::run("CREATE TEMP TABLE fresh_extremes (v int4); INSERT INTO fresh_extremes (v) VALUES (NULL), (-2147483648), (2147483647)").unwrap();
    assert_eq!(Spi::get_one::<bool>("SELECT rows = 1 AND sum = -2147483648 AND min = -2147483648 AND max = -2147483648 FROM syntra_heap_int4_range_summary('fresh_extremes','v',-2147483648,-2147483648) LIMIT 1").unwrap(), Some(true));
    assert_eq!(Spi::get_one::<bool>("SELECT rows = 1 AND sum = 2147483647 FROM syntra_heap_int4_range_summary('fresh_extremes','v',2147483647,2147483647) LIMIT 1").unwrap(), Some(true));
    assert_eq!(Spi::get_one::<bool>("SELECT rows = 2 AND non_nulls = 2 AND sum = -1 FROM syntra_heap_int4_range_summary('fresh_extremes','v',-2147483648,2147483647) LIMIT 1").unwrap(), Some(true));
}

#[pg_test(error = "source capture requires schema USAGE and table SELECT privileges")]
fn fresh_heap_cannot_bypass_source_permissions() {
    Spi::run("CREATE ROLE fresh_denied NOLOGIN; CREATE TABLE public.fresh_private (v int4); SET LOCAL ROLE fresh_denied").unwrap();
    Spi::run("SELECT rows FROM syntra_heap_int4_summary('public.fresh_private','v') LIMIT 1")
        .unwrap();
}

#[pg_test(error = "source capture does not support row-level security")]
fn fresh_heap_rejects_rls_even_for_the_owner() {
    Spi::run("CREATE TABLE fresh_rls (v int4); ALTER TABLE fresh_rls ENABLE ROW LEVEL SECURITY")
        .unwrap();
    Spi::run("SELECT rows FROM syntra_heap_int4_summary('fresh_rls','v') LIMIT 1").unwrap();
}

#[pg_test(error = "source capture requires a stored, non-generated int4 column")]
fn fresh_heap_rejects_unsupported_column_types() {
    Spi::run("CREATE TEMP TABLE fresh_bigint (v int8)").unwrap();
    Spi::run("SELECT rows FROM syntra_heap_int4_summary('fresh_bigint','v') LIMIT 1").unwrap();
}
