fn assert_owned_groups_match(sql: &str) {
    assert_eq!(Spi::get_one::<bool>(sql).unwrap(), Some(true));
}

macro_rules! owned_group_case {
    ($name:ident,$kind:literal,$value:literal,$function:literal,$seed:literal) => {
        #[pg_test]
        fn $name() {
            Spi::run(concat!("CREATE TEMP TABLE owned_groups(k ",$kind,",v ",$value,"); INSERT INTO owned_groups VALUES ",$seed)).unwrap();
            assert_owned_groups_match(concat!("WITH actual AS (SELECT group_key,rows,non_nulls,sum,min,max FROM ",$function,"('owned_groups','k','v',16)), expected AS (SELECT k group_key,count(*) rows,count(v) non_nulls,sum(v::int8) sum,min(v)::int8 min,max(v)::int8 max FROM owned_groups GROUP BY k) SELECT NOT EXISTS(SELECT group_key,rows,non_nulls,sum,min,max FROM actual EXCEPT ALL SELECT group_key,rows,non_nulls,sum,min,max FROM expected) AND NOT EXISTS(SELECT group_key,rows,non_nulls,sum,min,max FROM expected EXCEPT ALL SELECT group_key,rows,non_nulls,sum,min,max FROM actual)"));
        }
    };
}
owned_group_case!(
    text_group_int2_native,
    "text COLLATE \"C\"",
    "int2",
    "syntra_heap_group_text_summary",
    "('a',10),('a',NULL),('',-2),(NULL,5),(NULL,NULL),('A',9),('é',1),(U&'e\\0301',2),('a ',4)"
);
owned_group_case!(
    text_group_int4_native,
    "text COLLATE \"C\"",
    "int4",
    "syntra_heap_group_text_summary",
    "('a',10),('a',NULL),('',-2),(NULL,5),(NULL,NULL),('A',9),('é',1),(U&'e\\0301',2),('a ',4)"
);
owned_group_case!(text_group_int8_native,"text COLLATE \"C\"","int8","syntra_heap_group_text_summary","('a',9223372036854775807),('a',9223372036854775807),('',-2),(NULL,5),(NULL,NULL),('A',9),('é',1),(U&'e\\0301',2),('a ',4)");
owned_group_case!(uuid_group_int2_native,"uuid","int2","syntra_heap_group_uuid_summary","('00000000-0000-0000-0000-000000000000',10),('00000000-0000-0000-0000-000000000000',NULL),('ffffffff-ffff-ffff-ffff-ffffffffffff',-2),(NULL,5),(NULL,NULL)");
owned_group_case!(uuid_group_int4_native,"uuid","int4","syntra_heap_group_uuid_summary","('00000000-0000-0000-0000-000000000000',10),('00000000-0000-0000-0000-000000000000',NULL),('ffffffff-ffff-ffff-ffff-ffffffffffff',-2),(NULL,5),(NULL,NULL)");
owned_group_case!(uuid_group_int8_native,"uuid","int8","syntra_heap_group_uuid_summary","('ffffffff-ffff-ffff-ffff-ffffffffffff',9223372036854775807),('FFFFFFFF-FFFF-FFFF-FFFF-FFFFFFFFFFFF',9223372036854775807),('00000000-0000-0000-0000-000000000000',-9223372036854775808),(NULL,NULL)");

#[pg_test]
fn text_group_preserves_deterministic_icu_byte_equality() {
    Spi::run("CREATE COLLATION owned_det(provider=icu,locale='und-u-ks-level2',deterministic=true); CREATE TEMP TABLE owned_groups(k text COLLATE owned_det,v int4); INSERT INTO owned_groups VALUES ('a',1),('A',2),('é',3),(U&'e\\0301',4)").unwrap();
    assert_owned_groups_match("SELECT count(*)=4 AND sum(rows)=4 AND sum(sum)=10 FROM syntra_heap_group_text_summary('owned_groups','k','v')");
    assert_eq!(
        Spi::get_one::<i64>("SELECT count(*) FROM (SELECT k FROM owned_groups GROUP BY k) n")
            .unwrap(),
        Some(4)
    );
}

#[pg_test]
fn text_group_refuses_nondeterministic_collations_instead_of_wrong_groups() {
    Spi::run("CREATE COLLATION owned_ci(provider=icu,locale='und-u-ks-level2',deterministic=false); CREATE TEMP TABLE owned_groups(k text COLLATE owned_ci,v int4); INSERT INTO owned_groups VALUES ('a',1),('A',2)").unwrap();
    assert_eq!(
        Spi::get_one::<i64>("SELECT count(*) FROM (SELECT k FROM owned_groups GROUP BY k) n")
            .unwrap(),
        Some(1)
    );
    Spi::run("DO $test$ BEGIN BEGIN PERFORM group_key FROM syntra_heap_group_text_summary('owned_groups','k','v') LIMIT 1; EXCEPTION WHEN SQLSTATE '0A000' THEN RETURN; END; RAISE EXCEPTION 'nondeterministic equality was silently replaced'; END $test$").unwrap();
}

#[pg_test]
fn owned_key_budgets_count_utf8_bytes_and_distinct_keys() {
    Spi::run("CREATE TEMP TABLE owned_groups(k text,v int4); INSERT INTO owned_groups VALUES ('é',1),('é',2),(NULL,NULL)").unwrap();
    assert_owned_groups_match("SELECT count(*)=2 AND sum(rows)=3 AND sum(sum)=3 FROM syntra_heap_group_text_summary('owned_groups','k','v',2,2)");
    Spi::run("INSERT INTO owned_groups VALUES ('x',4)").unwrap();
    Spi::run("DO $test$ BEGIN BEGIN PERFORM group_key FROM syntra_heap_group_text_summary('owned_groups','k','v',3,2) LIMIT 1; EXCEPTION WHEN SQLSTATE '54000' THEN RETURN; END; RAISE EXCEPTION 'distinct byte budget returned partial output'; END $test$").unwrap();
    assert_owned_groups_match("SELECT count(*)=3 AND sum(sum)=7 FROM syntra_heap_group_text_summary('owned_groups','k','v',3,3)");
}

#[pg_test]
fn text_group_checks_toasted_key_size_before_copying() {
    Spi::run("CREATE TEMP TABLE owned_groups(k text,v int4); INSERT INTO owned_groups VALUES (repeat('a',100000),7)").unwrap();
    Spi::run("DO $test$ BEGIN BEGIN PERFORM group_key FROM syntra_heap_group_text_summary('owned_groups','k','v',1,8) LIMIT 1; EXCEPTION WHEN SQLSTATE '54000' THEN RETURN; END; RAISE EXCEPTION 'oversized toasted key was admitted'; END $test$").unwrap();
    assert_owned_groups_match("SELECT octet_length(group_key)=100000 AND sum=7 FROM syntra_heap_group_text_summary('owned_groups','k','v',1,100000)");
}

#[pg_test]
fn owned_group_invalid_limits_and_null_group_caps_are_atomic() {
    Spi::run("CREATE TEMP TABLE owned_groups(k uuid,v int4); INSERT INTO owned_groups VALUES (NULL,1),('00000000-0000-0000-0000-000000000001',2)").unwrap();
    Spi::run("DO $test$ DECLARE n integer; rejected integer:=0; BEGIN FOREACH n IN ARRAY ARRAY[-1,0,67108865,2147483647] LOOP BEGIN PERFORM group_key FROM syntra_heap_group_uuid_summary('owned_groups','k','v',2,n); EXCEPTION WHEN SQLSTATE '22023' THEN rejected:=rejected+1; END; END LOOP; IF rejected<>4 THEN RAISE EXCEPTION 'invalid byte limits accepted'; END IF; BEGIN PERFORM group_key FROM syntra_heap_group_uuid_summary('owned_groups','k','v',1,16) LIMIT 1; EXCEPTION WHEN SQLSTATE '54000' THEN RETURN; END; RAISE EXCEPTION 'NULL group was not capped'; END $test$").unwrap();
    assert_owned_groups_match("SELECT count(*)=2 AND sum(sum)=3 FROM syntra_heap_group_uuid_summary('owned_groups','k','v',2,16)");
}

#[pg_test]
fn owned_group_empty_strict_and_all_null_values_match_sql() {
    Spi::run(
        "CREATE TEMP TABLE owned_text(k text,v int4); CREATE TEMP TABLE owned_uuid(k uuid,v int4)",
    )
    .unwrap();
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT count(*) FROM syntra_heap_group_text_summary('owned_text','k','v')"
        )
        .unwrap(),
        Some(0)
    );
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT count(*) FROM syntra_heap_group_uuid_summary('owned_uuid','k','v')"
        )
        .unwrap(),
        Some(0)
    );
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT count(*) FROM syntra_heap_group_text_summary('owned_text','k','v',NULL)"
        )
        .unwrap(),
        Some(0)
    );
    Spi::run("INSERT INTO owned_text VALUES ('',NULL),(NULL,NULL)").unwrap();
    assert_owned_groups_match("SELECT count(*)=2 AND sum(rows)=2 AND bool_and(non_nulls=0 AND sum IS NULL AND min IS NULL AND max IS NULL) FROM syntra_heap_group_text_summary('owned_text','k','v',2,1)");
}

#[pg_test]
fn owned_group_own_writes_and_modifying_cte_keep_snapshots() {
    Spi::run(
        "CREATE TEMP TABLE owned_groups(k text,v int4); INSERT INTO owned_groups VALUES ('old',10)",
    )
    .unwrap();
    assert_owned_groups_match("WITH changed AS(UPDATE owned_groups SET k='new',v=20 WHERE k='old' RETURNING v) SELECT c.v=20 AND g.group_key='old' AND g.sum=10 FROM changed c,LATERAL syntra_heap_group_text_summary('owned_groups','k','v') g");
    assert_owned_groups_match("SELECT group_key='new' AND sum=20 FROM syntra_heap_group_text_summary('owned_groups','k','v')");
}

#[pg_test(error = "source capture requires schema USAGE and table SELECT privileges")]
fn owned_group_does_not_bypass_source_permissions() {
    Spi::run("CREATE ROLE owned_denied NOLOGIN; CREATE TABLE public.owned_groups(k text,v int4); GRANT SELECT(k) ON public.owned_groups TO owned_denied; SET LOCAL ROLE owned_denied").unwrap();
    Spi::run("SELECT group_key FROM syntra_heap_group_text_summary('public.owned_groups','k','v')")
        .unwrap();
}

#[pg_test(error = "source capture does not support row-level security")]
fn owned_group_refuses_rls() {
    Spi::run("CREATE TABLE owned_groups(k uuid,v int4); ALTER TABLE owned_groups ENABLE ROW LEVEL SECURITY").unwrap();
    Spi::run("SELECT group_key FROM syntra_heap_group_uuid_summary('owned_groups','k','v')")
        .unwrap();
}

#[pg_test(error = "text grouping requires a stored, non-generated text key")]
fn text_group_refuses_bpchar_equality_semantics() {
    Spi::run("CREATE TEMP TABLE owned_groups(k char(3),v int4)").unwrap();
    Spi::run("SELECT group_key FROM syntra_heap_group_text_summary('owned_groups','k','v')")
        .unwrap();
}
