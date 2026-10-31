fn assert_heap_group_matches_native() {
    assert_eq!(
        Spi::get_one::<bool>(
            r#"WITH actual AS (
                SELECT group_key,rows,non_nulls,sum,min,max
                FROM syntra_heap_group_integer_summary('group_source','k','v',16)
            ), expected AS (
                SELECT k::bigint AS group_key,count(*) AS rows,count(v) AS non_nulls,
                       sum(v::bigint) AS sum,min(v)::bigint AS min,max(v)::bigint AS max
                FROM group_source GROUP BY k
            )
            SELECT NOT EXISTS (
                SELECT group_key,rows,non_nulls,sum,min,max FROM actual
                EXCEPT ALL SELECT group_key,rows,non_nulls,sum,min,max FROM expected
            ) AND NOT EXISTS (
                SELECT group_key,rows,non_nulls,sum,min,max FROM expected
                EXCEPT ALL SELECT group_key,rows,non_nulls,sum,min,max FROM actual
            )"#,
        )
        .unwrap(),
        Some(true)
    );
}

macro_rules! integer_group_case {
    ($name:ident, $key:literal, $value:literal) => {
        #[pg_test]
        fn $name() {
            // All fragments are compile-time integer type literals, not SQL input.
            Spi::run(concat!(
                "CREATE TEMP TABLE group_source(k ",
                $key,
                ",v ",
                $value,
                "); INSERT INTO group_source VALUES (1,10),(1,NULL),(NULL,5),(NULL,NULL),(-1,-2)"
            ))
            .unwrap();
            assert_heap_group_matches_native();
        }
    };
}

integer_group_case!(heap_group_int2_int2_matches_native, "int2", "int2");
integer_group_case!(heap_group_int2_int4_matches_native, "int2", "int4");
integer_group_case!(heap_group_int2_int8_matches_native, "int2", "int8");
integer_group_case!(heap_group_int4_int2_matches_native, "int4", "int2");
integer_group_case!(heap_group_int4_int4_matches_native, "int4", "int4");
integer_group_case!(heap_group_int4_int8_matches_native, "int4", "int8");
integer_group_case!(heap_group_int8_int2_matches_native, "int8", "int2");
integer_group_case!(heap_group_int8_int4_matches_native, "int8", "int4");
integer_group_case!(heap_group_int8_int8_matches_native, "int8", "int8");

#[pg_test]
fn heap_group_wide_endpoints_preserve_numeric_sums_and_null_groups() {
    Spi::run("CREATE TEMP TABLE group_source(k int8,v int8); INSERT INTO group_source VALUES(-9223372036854775808,9223372036854775807),(-9223372036854775808,9223372036854775807),(9223372036854775807,-9223372036854775808),(9223372036854775807,-9223372036854775808),(NULL,NULL),(NULL,0),(0,NULL)").unwrap();
    assert_heap_group_matches_native();
    assert_eq!(Spi::get_one::<String>("SELECT sum::text FROM syntra_heap_group_integer_summary('group_source','k','v') WHERE group_key=-9223372036854775808").unwrap(), Some("18446744073709551614".to_owned()));
}

#[pg_test]
fn heap_group_maximum_output_cardinality_is_complete() {
    Spi::run("CREATE TEMP TABLE group_source(k int4,v int4); INSERT INTO group_source SELECT g,g FROM generate_series(0,65535) AS g").unwrap();
    assert_eq!(Spi::get_one::<bool>("SELECT count(*)=65536 AND sum(rows)=65536 AND sum(sum)=2147450880 AND min(group_key)=0 AND max(group_key)=65535 FROM syntra_heap_group_integer_summary('group_source','k','v',65536)").unwrap(), Some(true));
}

#[pg_test]
fn heap_group_sees_prior_own_writes_and_preserves_a_modifying_cte_snapshot() {
    Spi::run(
        "CREATE TEMP TABLE group_source(k int4,v int4); INSERT INTO group_source VALUES(1,10)",
    )
    .unwrap();
    assert_eq!(Spi::get_one::<bool>("WITH changed AS (UPDATE group_source SET k=2,v=20 WHERE k=1 RETURNING k,v) SELECT c.k=2 AND c.v=20 AND g.group_key=1 AND g.sum=10 AND (SELECT sum(v) FROM group_source)=10 FROM changed AS c, LATERAL syntra_heap_group_integer_summary('group_source','k','v') AS g").unwrap(), Some(true));
    assert_eq!(Spi::get_one::<bool>("SELECT group_key=2 AND sum=20 FROM syntra_heap_group_integer_summary('group_source','k','v')").unwrap(), Some(true));
}

#[pg_test]
fn heap_group_empty_input_and_strict_arguments_produce_no_groups() {
    Spi::run("CREATE TEMP TABLE group_source(k int4,v int4)").unwrap();
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT count(*) FROM syntra_heap_group_integer_summary('group_source','k','v')"
        )
        .unwrap(),
        Some(0)
    );
    for sql in [
        "SELECT count(*) FROM syntra_heap_group_integer_summary(NULL::regclass,'k','v')",
        "SELECT count(*) FROM syntra_heap_group_integer_summary('group_source',NULL,'v')",
        "SELECT count(*) FROM syntra_heap_group_integer_summary('group_source','k',NULL)",
        "SELECT count(*) FROM syntra_heap_group_integer_summary('group_source','k','v',NULL)",
    ] {
        assert_eq!(Spi::get_one::<i64>(sql).unwrap(), Some(0));
    }
}

#[pg_test]
fn heap_group_default_limit_same_column_and_output_order_work() {
    Spi::run("CREATE TEMP TABLE group_source(k int4,v int4); INSERT INTO group_source VALUES(1,10),(1,NULL),(NULL,5),(NULL,NULL),(-1,-2)").unwrap();
    assert_eq!(Spi::get_one::<String>("SELECT string_agg(coalesce(group_key::text,'NULL'),',') FROM syntra_heap_group_integer_summary('group_source','k','v')").unwrap(), Some("NULL,-1,1".to_owned()));
    assert_eq!(Spi::get_one::<bool>("SELECT rows=2 AND non_nulls=2 AND sum=2 AND min=1 AND max=1 FROM syntra_heap_group_integer_summary('group_source','k','k') WHERE group_key=1").unwrap(), Some(true));
}

#[pg_test]
fn heap_group_rejects_invalid_limits_with_parameter_sqlstate() {
    Spi::run("CREATE TEMP TABLE group_source(k int4,v int4)").unwrap();
    Spi::run(r#"DO $check$ DECLARE bound integer; rejected integer := 0; BEGIN
        FOREACH bound IN ARRAY ARRAY[-1,0,65537,2147483647] LOOP
            BEGIN
                PERFORM group_key FROM syntra_heap_group_integer_summary('group_source','k','v',bound);
            EXCEPTION WHEN SQLSTATE '22023' THEN rejected := rejected + 1;
            END;
        END LOOP;
        IF rejected <> 4 THEN RAISE EXCEPTION 'invalid group limits were accepted'; END IF;
    END $check$"#).unwrap();
    assert_eq!(
        Spi::get_one::<i64>("SELECT count(*) FROM group_source").unwrap(),
        Some(0)
    );
}

#[pg_test]
fn heap_group_limit_error_precedes_output_even_with_limit_one() {
    Spi::run(
        "CREATE TEMP TABLE group_source(k int4,v int4); INSERT INTO group_source VALUES(1,1),(2,2)",
    )
    .unwrap();
    Spi::run(r#"DO $check$ BEGIN
        BEGIN
            PERFORM group_key FROM syntra_heap_group_integer_summary('group_source','k','v',1) LIMIT 1;
        EXCEPTION WHEN SQLSTATE '54000' THEN RETURN;
        END;
        RAISE EXCEPTION 'grouping returned partial output past its limit';
    END $check$"#).unwrap();
    assert_heap_group_matches_native();
}

#[pg_test(error = "source capture requires schema USAGE and table SELECT privileges")]
fn heap_group_cannot_bypass_column_only_permissions() {
    Spi::run("CREATE ROLE group_denied NOLOGIN; CREATE TABLE public.group_source(k int4,v int4); GRANT SELECT(k) ON public.group_source TO group_denied; SET LOCAL ROLE group_denied").unwrap();
    Spi::run(
        "SELECT group_key FROM syntra_heap_group_integer_summary('public.group_source','k','v')",
    )
    .unwrap();
}

#[pg_test]
fn heap_group_allows_a_reader_with_table_select() {
    Spi::run("CREATE ROLE group_reader NOLOGIN; CREATE TABLE public.group_source(k int4,v int4); INSERT INTO public.group_source VALUES(1,10); GRANT SELECT ON public.group_source TO group_reader; SET LOCAL ROLE group_reader").unwrap();
    assert_eq!(Spi::get_one::<bool>("SELECT group_key=1 AND rows=1 AND sum=10 FROM syntra_heap_group_integer_summary('public.group_source','k','v')").unwrap(), Some(true));
}

#[pg_test(error = "source capture does not support row-level security")]
fn heap_group_rejects_rls_even_for_its_owner() {
    Spi::run("CREATE TABLE group_source(k int4,v int4); ALTER TABLE group_source ENABLE ROW LEVEL SECURITY").unwrap();
    Spi::run("SELECT group_key FROM syntra_heap_group_integer_summary('group_source','k','v')")
        .unwrap();
}

#[pg_test(error = "heap grouping requires stored, non-generated int2, int4 or int8 columns")]
fn heap_group_rejects_a_non_integer_key() {
    Spi::run("CREATE TEMP TABLE group_source(k text,v int4)").unwrap();
    Spi::run("SELECT group_key FROM syntra_heap_group_integer_summary('group_source','k','v')")
        .unwrap();
}

#[pg_test(error = "heap grouping requires stored, non-generated int2, int4 or int8 columns")]
fn heap_group_rejects_a_non_integer_value() {
    Spi::run("CREATE TEMP TABLE group_source(k int4,v text)").unwrap();
    Spi::run("SELECT group_key FROM syntra_heap_group_integer_summary('group_source','k','v')")
        .unwrap();
}

#[pg_test(error = "heap grouping requires stored, non-generated int2, int4 or int8 columns")]
fn heap_group_rejects_generated_columns() {
    Spi::run("CREATE TEMP TABLE group_source(k int4,v int4 GENERATED ALWAYS AS(k*2) STORED)")
        .unwrap();
    Spi::run("SELECT group_key FROM syntra_heap_group_integer_summary('group_source','k','v')")
        .unwrap();
}

#[pg_test(
    error = "source capture supports only ordinary heap tables without partitions or descendants"
)]
fn heap_group_rejects_partitioned_tables() {
    Spi::run("CREATE TABLE group_source(k int4,v int4) PARTITION BY RANGE(k)").unwrap();
    Spi::run("SELECT group_key FROM syntra_heap_group_integer_summary('group_source','k','v')")
        .unwrap();
}

#[pg_test]
fn heap_group_uses_column_names_as_identifiers_without_dynamic_sql() {
    Spi::run(r#"CREATE TEMP TABLE group_quoted("k'"" odd" int4,v int4); INSERT INTO group_quoted VALUES(1,9),(1,NULL)"#).unwrap();
    assert_eq!(Spi::get_one::<bool>(r#"SELECT group_key=1 AND rows=2 AND non_nulls=1 AND sum=9 FROM syntra_heap_group_integer_summary('group_quoted','k''" odd','v')"#).unwrap(), Some(true));
}
