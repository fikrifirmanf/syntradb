// Each fixture compares the complete multiset against native PostgreSQL GROUP BY.
macro_rules! decimal_group_case {
    ($name:ident,$key:literal,$typ:literal,$function:literal,$seed:literal) => {
        #[pg_test]
        fn $name() {
            // unslop-ignore NO_SQL_INTERPOLATION -- Macro arguments are compile-time test literals; types and DDL cannot use SQL bindings.
            Spi::run(concat!("CREATE TEMP TABLE decimal_groups(k ",$key,",v ",$typ,"); INSERT INTO decimal_groups VALUES ",$seed)).unwrap();
            // unslop-ignore NO_SQL_INTERPOLATION -- Only compile-time function-name literals enter this native-oracle test query.
            assert_owned_groups_match(concat!("WITH actual AS (SELECT group_key,rows,non_nulls,sum,min,max FROM ",$function,"('decimal_groups','k','v')), expected AS (SELECT k group_key,count(*) rows,count(v) non_nulls,sum(v) sum,min(v) min,max(v) max FROM decimal_groups GROUP BY k) SELECT NOT EXISTS(SELECT group_key,rows,non_nulls,sum,min,max FROM actual EXCEPT ALL SELECT group_key,rows,non_nulls,sum,min,max FROM expected) AND NOT EXISTS(SELECT group_key,rows,non_nulls,sum,min,max FROM expected EXCEPT ALL SELECT group_key,rows,non_nulls,sum,min,max FROM actual)"));
        }
    };
}
decimal_group_case!(decimal_group_int2_scale_0,"int2","numeric(18,0)","syntra_heap_group_integer_numeric_summary","(1,999999999999999999),(1,999999999999999999),(1,NULL),(2,-999999999999999999),(2,-999999999999999999),(3,'NaN'),(3,1),(4,'NaN'),(4,NULL),(NULL,NULL),(NULL,NULL)");
decimal_group_case!(decimal_group_int2_scale_2,"int2","numeric(18,2)","syntra_heap_group_integer_numeric_summary","(1,9999999999999999.99),(1,9999999999999999.99),(1,NULL),(2,-9999999999999999.99),(2,-9999999999999999.99),(3,'NaN'),(3,0.02),(4,'NaN'),(4,NULL),(NULL,NULL),(NULL,NULL)");
decimal_group_case!(decimal_group_int2_scale_18,"int2","numeric(18,18)","syntra_heap_group_integer_numeric_summary","(1,0.999999999999999999),(1,0.999999999999999999),(1,NULL),(2,-0.999999999999999999),(2,-0.999999999999999999),(3,'NaN'),(3,0.000000000000000002),(4,'NaN'),(4,NULL),(NULL,NULL),(NULL,NULL)");
decimal_group_case!(decimal_group_int4_scale_0,"int4","numeric(18,0)","syntra_heap_group_integer_numeric_summary","(1,999999999999999999),(1,999999999999999999),(1,NULL),(2,-999999999999999999),(2,-999999999999999999),(3,'NaN'),(3,1),(4,'NaN'),(4,NULL),(NULL,NULL),(NULL,NULL)");
decimal_group_case!(decimal_group_int4_scale_2,"int4","numeric(18,2)","syntra_heap_group_integer_numeric_summary","(1,9999999999999999.99),(1,9999999999999999.99),(1,NULL),(2,-9999999999999999.99),(2,-9999999999999999.99),(3,'NaN'),(3,0.02),(4,'NaN'),(4,NULL),(NULL,NULL),(NULL,NULL)");
decimal_group_case!(decimal_group_int4_scale_18,"int4","numeric(18,18)","syntra_heap_group_integer_numeric_summary","(1,0.999999999999999999),(1,0.999999999999999999),(1,NULL),(2,-0.999999999999999999),(2,-0.999999999999999999),(3,'NaN'),(3,0.000000000000000002),(4,'NaN'),(4,NULL),(NULL,NULL),(NULL,NULL)");
decimal_group_case!(decimal_group_int8_scale_0,"int8","numeric(18,0)","syntra_heap_group_integer_numeric_summary","(9223372036854775807,999999999999999999),(9223372036854775807,999999999999999999),(9223372036854775807,NULL),(-9223372036854775808,-999999999999999999),(-9223372036854775808,-999999999999999999),(3,'NaN'),(3,1),(4,'NaN'),(4,NULL),(NULL,NULL),(NULL,NULL)");
decimal_group_case!(decimal_group_int8_scale_2,"int8","numeric(18,2)","syntra_heap_group_integer_numeric_summary","(9223372036854775807,9999999999999999.99),(9223372036854775807,9999999999999999.99),(9223372036854775807,NULL),(-9223372036854775808,-9999999999999999.99),(-9223372036854775808,-9999999999999999.99),(3,'NaN'),(3,0.02),(4,'NaN'),(4,NULL),(NULL,NULL),(NULL,NULL)");
decimal_group_case!(decimal_group_int8_scale_18,"int8","numeric(18,18)","syntra_heap_group_integer_numeric_summary","(9223372036854775807,0.999999999999999999),(9223372036854775807,0.999999999999999999),(9223372036854775807,NULL),(-9223372036854775808,-0.999999999999999999),(-9223372036854775808,-0.999999999999999999),(3,'NaN'),(3,0.000000000000000002),(4,'NaN'),(4,NULL),(NULL,NULL),(NULL,NULL)");
decimal_group_case!(decimal_group_text_scale_0,"text COLLATE \"C\"","numeric(18,0)","syntra_heap_group_text_numeric_summary","('é',999999999999999999),('é',999999999999999999),('é',NULL),('é',-999999999999999999),('é',-999999999999999999),('','NaN'),('',1),('a ','NaN'),('a ',NULL),(NULL,NULL),(NULL,NULL)");
decimal_group_case!(decimal_group_text_scale_2,"text COLLATE \"C\"","numeric(18,2)","syntra_heap_group_text_numeric_summary","('é',9999999999999999.99),('é',9999999999999999.99),('é',NULL),('é',-9999999999999999.99),('é',-9999999999999999.99),('','NaN'),('',0.02),('a ','NaN'),('a ',NULL),(NULL,NULL),(NULL,NULL)");
decimal_group_case!(decimal_group_text_scale_18,"text COLLATE \"C\"","numeric(18,18)","syntra_heap_group_text_numeric_summary","('é',0.999999999999999999),('é',0.999999999999999999),('é',NULL),('é',-0.999999999999999999),('é',-0.999999999999999999),('','NaN'),('',0.000000000000000002),('a ','NaN'),('a ',NULL),(NULL,NULL),(NULL,NULL)");
decimal_group_case!(decimal_group_uuid_scale_0,"uuid","numeric(18,0)","syntra_heap_group_uuid_numeric_summary","('ffffffff-ffff-ffff-ffff-ffffffffffff',999999999999999999),('ffffffff-ffff-ffff-ffff-ffffffffffff',999999999999999999),('ffffffff-ffff-ffff-ffff-ffffffffffff',NULL),('00000000-0000-0000-0000-000000000000',-999999999999999999),('00000000-0000-0000-0000-000000000000',-999999999999999999),('00000000-0000-0000-0000-000000000001','NaN'),('00000000-0000-0000-0000-000000000001',1),('00000000-0000-0000-0000-000000000002','NaN'),('00000000-0000-0000-0000-000000000002',NULL),(NULL,NULL),(NULL,NULL)");
decimal_group_case!(decimal_group_uuid_scale_2,"uuid","numeric(18,2)","syntra_heap_group_uuid_numeric_summary","('ffffffff-ffff-ffff-ffff-ffffffffffff',9999999999999999.99),('ffffffff-ffff-ffff-ffff-ffffffffffff',9999999999999999.99),('ffffffff-ffff-ffff-ffff-ffffffffffff',NULL),('00000000-0000-0000-0000-000000000000',-9999999999999999.99),('00000000-0000-0000-0000-000000000000',-9999999999999999.99),('00000000-0000-0000-0000-000000000001','NaN'),('00000000-0000-0000-0000-000000000001',0.02),('00000000-0000-0000-0000-000000000002','NaN'),('00000000-0000-0000-0000-000000000002',NULL),(NULL,NULL),(NULL,NULL)");
decimal_group_case!(decimal_group_uuid_scale_18,"uuid","numeric(18,18)","syntra_heap_group_uuid_numeric_summary","('ffffffff-ffff-ffff-ffff-ffffffffffff',0.999999999999999999),('ffffffff-ffff-ffff-ffff-ffffffffffff',0.999999999999999999),('ffffffff-ffff-ffff-ffff-ffffffffffff',NULL),('00000000-0000-0000-0000-000000000000',-0.999999999999999999),('00000000-0000-0000-0000-000000000000',-0.999999999999999999),('00000000-0000-0000-0000-000000000001','NaN'),('00000000-0000-0000-0000-000000000001',0.000000000000000002),('00000000-0000-0000-0000-000000000002','NaN'),('00000000-0000-0000-0000-000000000002',NULL),(NULL,NULL),(NULL,NULL)");

#[pg_test]
fn decimal_group_empty_strict_and_precision_one() {
    Spi::run("CREATE TEMP TABLE decimal_groups(k int2,v numeric(1,1))").unwrap();
    assert_owned_groups_match("SELECT count(*)=0 FROM syntra_heap_group_integer_numeric_summary('decimal_groups','k','v')");
    assert_owned_groups_match("SELECT count(*)=0 FROM syntra_heap_group_integer_numeric_summary('decimal_groups','k','v',NULL)");
    Spi::run("INSERT INTO decimal_groups VALUES (1,0.9),(1,0.2),(NULL,NULL)").unwrap();
    assert_owned_groups_match("SELECT sum=1.1 AND min=0.2 AND max=0.9 AND non_nulls=2 FROM syntra_heap_group_integer_numeric_summary('decimal_groups','k','v') WHERE group_key=1");
}

#[pg_test]
fn decimal_group_budgets_are_atomic_and_backend_remains_usable() {
    Spi::run("CREATE TEMP TABLE decimal_groups(k text,v numeric(18,2)); INSERT INTO decimal_groups VALUES ('é',0.99),('é',0.02),(NULL,NULL)").unwrap();
    assert_owned_groups_match("SELECT count(*)=2 AND sum(rows)=3 AND sum(sum)=1.01 FROM syntra_heap_group_text_numeric_summary('decimal_groups','k','v',2,2)");
    Spi::run("DO $test$ BEGIN BEGIN PERFORM group_key FROM syntra_heap_group_text_numeric_summary('decimal_groups','k','v',1,2) LIMIT 1; EXCEPTION WHEN SQLSTATE '54000' THEN RETURN; END; RAISE EXCEPTION 'partial group output'; END $test$").unwrap();
    Spi::run("INSERT INTO decimal_groups VALUES ('x',2.22)").unwrap();
    Spi::run("DO $test$ BEGIN BEGIN PERFORM group_key FROM syntra_heap_group_text_numeric_summary('decimal_groups','k','v',3,2) LIMIT 1; EXCEPTION WHEN SQLSTATE '54000' THEN RETURN; END; RAISE EXCEPTION 'partial byte output'; END $test$").unwrap();
    assert_owned_groups_match("SELECT count(*)=3 AND sum(sum)=3.23 FROM syntra_heap_group_text_numeric_summary('decimal_groups','k','v',3,3)");
    Spi::run("DO $test$ DECLARE n integer; rejected integer:=0; BEGIN FOREACH n IN ARRAY ARRAY[-1,0,67108865,2147483647] LOOP BEGIN PERFORM group_key FROM syntra_heap_group_text_numeric_summary('decimal_groups','k','v',3,n); EXCEPTION WHEN SQLSTATE '22023' THEN rejected:=rejected+1; END; END LOOP; IF rejected<>4 THEN RAISE EXCEPTION 'invalid byte limits'; END IF; END $test$").unwrap();
}

#[pg_test]
fn decimal_group_fixed_key_bytes_use_native_width_and_null_costs_zero() {
    Spi::run("CREATE TEMP TABLE decimal_groups(k int2,v numeric(5,2)); INSERT INTO decimal_groups VALUES (1,1.25),(1,2.50),(NULL,NULL)").unwrap();
    assert_owned_groups_match("SELECT count(*)=2 AND sum(sum)=3.75 FROM syntra_heap_group_integer_numeric_summary('decimal_groups','k','v',2,2)");
    Spi::run("DO $test$ BEGIN BEGIN PERFORM group_key FROM syntra_heap_group_integer_numeric_summary('decimal_groups','k','v',2,1); EXCEPTION WHEN SQLSTATE '54000' THEN RETURN; END; RAISE EXCEPTION 'int2 width not charged'; END $test$").unwrap();
    Spi::run("CREATE TEMP TABLE decimal_uuid(k uuid,v numeric(5,2)); INSERT INTO decimal_uuid VALUES ('00000000-0000-0000-0000-000000000001',1.25),(NULL,NULL)").unwrap();
    Spi::run("DO $test$ BEGIN BEGIN PERFORM group_key FROM syntra_heap_group_uuid_numeric_summary('decimal_uuid','k','v',2,15); EXCEPTION WHEN SQLSTATE '54000' THEN RETURN; END; RAISE EXCEPTION 'UUID width not charged'; END $test$").unwrap();
    assert_owned_groups_match("SELECT count(*)=2 AND sum(sum)=1.25 FROM syntra_heap_group_uuid_numeric_summary('decimal_uuid','k','v',2,16)");
}

#[pg_test]
fn decimal_group_checks_toasted_key_before_copying() {
    Spi::run("CREATE TEMP TABLE decimal_groups(k text,v numeric(5,2)); INSERT INTO decimal_groups VALUES (repeat('a',100000),1.25)").unwrap();
    Spi::run("DO $test$ BEGIN BEGIN PERFORM group_key FROM syntra_heap_group_text_numeric_summary('decimal_groups','k','v',1,8) LIMIT 1; EXCEPTION WHEN SQLSTATE '54000' THEN RETURN; END; RAISE EXCEPTION 'oversized toasted key'; END $test$").unwrap();
    assert_owned_groups_match("SELECT octet_length(group_key)=100000 AND sum=1.25 FROM syntra_heap_group_text_numeric_summary('decimal_groups','k','v',1,100000)");
}

#[pg_test]
fn decimal_group_own_writes_and_modifying_cte_use_command_snapshot() {
    Spi::run("CREATE TEMP TABLE decimal_groups(k text,v numeric(5,2)); INSERT INTO decimal_groups VALUES ('old',1.25)").unwrap();
    assert_owned_groups_match("WITH changed AS(UPDATE decimal_groups SET k='new',v=2.50 WHERE k='old' RETURNING v) SELECT c.v=2.50 AND g.group_key='old' AND g.sum=1.25 FROM changed c,LATERAL syntra_heap_group_text_numeric_summary('decimal_groups','k','v') g");
    assert_owned_groups_match("SELECT group_key='new' AND sum=2.50 FROM syntra_heap_group_text_numeric_summary('decimal_groups','k','v')");
}

#[pg_test(error = "decimal grouping requires numeric(p,s) with 1 <= p <= 18 and 0 <= s <= p")]
fn decimal_group_refuses_unconstrained_numeric() {
    Spi::run("CREATE TEMP TABLE decimal_groups(k int4,v numeric)").unwrap();
    Spi::run(
        "SELECT group_key FROM syntra_heap_group_integer_numeric_summary('decimal_groups','k','v')",
    )
    .unwrap();
}

#[pg_test(error = "decimal grouping requires numeric(p,s) with 1 <= p <= 18 and 0 <= s <= p")]
fn decimal_group_refuses_precision_nineteen() {
    Spi::run("CREATE TEMP TABLE decimal_groups(k uuid,v numeric(19,2))").unwrap();
    Spi::run(
        "SELECT group_key FROM syntra_heap_group_uuid_numeric_summary('decimal_groups','k','v')",
    )
    .unwrap();
}

#[pg_test(error = "decimal grouping requires numeric(p,s) with 1 <= p <= 18 and 0 <= s <= p")]
fn decimal_group_refuses_negative_scale() {
    Spi::run("CREATE TEMP TABLE decimal_groups(k text,v numeric(5,-1))").unwrap();
    Spi::run(
        "SELECT group_key FROM syntra_heap_group_text_numeric_summary('decimal_groups','k','v')",
    )
    .unwrap();
}

#[pg_test(error = "decimal grouping requires numeric(p,s) with 1 <= p <= 18 and 0 <= s <= p")]
fn decimal_group_refuses_scale_above_precision() {
    Spi::run("CREATE TEMP TABLE decimal_groups(k text,v numeric(5,6))").unwrap();
    Spi::run(
        "SELECT group_key FROM syntra_heap_group_text_numeric_summary('decimal_groups','k','v')",
    )
    .unwrap();
}

#[pg_test(error = "decimal grouping requires a stored, non-generated numeric value column")]
fn decimal_group_refuses_generated_measure() {
    Spi::run("CREATE TEMP TABLE decimal_groups(k int4,v numeric(5,2) GENERATED ALWAYS AS (k::numeric) STORED)").unwrap();
    Spi::run(
        "SELECT group_key FROM syntra_heap_group_integer_numeric_summary('decimal_groups','k','v')",
    )
    .unwrap();
}

#[pg_test(error = "source capture requires schema USAGE and table SELECT privileges")]
fn decimal_group_does_not_bypass_permissions() {
    Spi::run("CREATE ROLE decimal_denied NOLOGIN; CREATE TABLE public.decimal_groups(k text,v numeric(5,2)); GRANT SELECT(k) ON public.decimal_groups TO decimal_denied; SET LOCAL ROLE decimal_denied").unwrap();
    Spi::run("SELECT group_key FROM syntra_heap_group_text_numeric_summary('public.decimal_groups','k','v')").unwrap();
}

#[pg_test(error = "source capture does not support row-level security")]
fn decimal_group_refuses_rls() {
    Spi::run("CREATE TABLE decimal_groups(k uuid,v numeric(5,2)); ALTER TABLE decimal_groups ENABLE ROW LEVEL SECURITY").unwrap();
    Spi::run(
        "SELECT group_key FROM syntra_heap_group_uuid_numeric_summary('decimal_groups','k','v')",
    )
    .unwrap();
}

#[pg_test]
fn decimal_group_refuses_nondeterministic_text_equality() {
    Spi::run("CREATE COLLATION decimal_ci(provider=icu,locale='und-u-ks-level2',deterministic=false); CREATE TEMP TABLE decimal_groups(k text COLLATE decimal_ci,v numeric(5,2)); INSERT INTO decimal_groups VALUES ('a',1.25),('A',2.50)").unwrap();
    Spi::run("DO $test$ BEGIN BEGIN PERFORM group_key FROM syntra_heap_group_text_numeric_summary('decimal_groups','k','v'); EXCEPTION WHEN SQLSTATE '0A000' THEN RETURN; END; RAISE EXCEPTION 'wrong collation equality'; END $test$").unwrap();
}
