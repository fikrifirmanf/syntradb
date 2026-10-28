#[pg_test]
fn capture_records_source_and_matches_native_results() {
    Spi::run("CREATE TABLE syntra_capture_source (id int4, value int4)").unwrap();
    Spi::run("INSERT INTO syntra_capture_source (id, value) VALUES (1, NULL), (2, -4), (3, 10)")
        .unwrap();
    assert_eq!(Spi::get_one::<bool>(r#"
        WITH captured AS MATERIALIZED (SELECT segment, source_database, source_relation, source_schema,
            source_table, source_column, source_attribute, source_type, source_snapshot,
            source_transaction, source_command, captured_at, source_rows
            FROM syntra_capture_int4('syntra_capture_source'::regclass, 'value'))
        SELECT c.source_relation = 'syntra_capture_source'::regclass::oid
            AND c.source_database = (SELECT oid FROM pg_database WHERE datname = current_database())
            AND c.source_table = 'syntra_capture_source' AND c.source_column = 'value'
            AND c.source_attribute = 2 AND c.source_type = 'int4'::regtype::oid
            AND c.source_snapshot = pg_current_snapshot()::text AND c.source_command >= 0
            AND c.captured_at = statement_timestamp() AND c.source_rows = 3
            AND s.rows = 3 AND s.non_nulls = 2 AND s.sum = 6 AND s.min = -4 AND s.max = 10
        FROM captured AS c, LATERAL syntra_segment_summary(c.segment) AS s LIMIT 1
    "#).unwrap(), Some(true));
}

#[pg_test]
fn source_identity_matches_postgresql_storage_and_column_metadata() {
    Spi::run("CREATE TABLE identity_source (id int4, value int4) TABLESPACE pg_default").unwrap();
    assert_eq!(Spi::get_one::<bool>("SELECT cluster_identity = (SELECT system_identifier::text FROM pg_control_system()) AND source_database = (SELECT oid FROM pg_database WHERE datname = current_database()) AND source_relation = 'identity_source'::regclass::oid AND storage_database = source_database AND storage_file = pg_relation_filenode('identity_source') AND storage_tablespace = (SELECT oid FROM pg_tablespace WHERE spcname = 'pg_default') AND source_attribute = 2 AND source_type = 'int4'::regtype::oid AND type_modifier = -1 AND source_collation IS NULL AND access_method = (SELECT oid FROM pg_am WHERE amname = 'heap') FROM syntra_source_identity('identity_source', 'value') LIMIT 1").unwrap(), Some(true));
}

#[pg_test]
fn source_identity_changes_after_storage_replacement() {
    // A table created in this transaction can truncate in place. The exception
    // block creates a subtransaction, forcing PostgreSQL's storage rewrite path.
    Spi::run("CREATE TABLE identity_truncate (v int4); INSERT INTO identity_truncate (v) VALUES (1); CREATE TEMP TABLE identity_before AS SELECT storage_file FROM syntra_source_identity('identity_truncate', 'v'); DO $$ BEGIN TRUNCATE identity_truncate; EXCEPTION WHEN OTHERS THEN RAISE; END $$").unwrap();
    assert_eq!(Spi::get_one::<bool>("SELECT i.storage_file <> b.storage_file FROM syntra_source_identity('identity_truncate', 'v') AS i, identity_before AS b LIMIT 1").unwrap(), Some(true));
}

#[pg_test(error = "source capture requires schema USAGE and table SELECT privileges")]
fn source_identity_checks_invoker_permissions() {
    Spi::run("CREATE ROLE identity_denied NOLOGIN; CREATE TABLE public.identity_private (v int4); SET LOCAL ROLE identity_denied").unwrap();
    Spi::run(
        "SELECT storage_file FROM syntra_source_identity('public.identity_private', 'v') LIMIT 1",
    )
    .unwrap();
}

#[pg_test]
fn lifecycle_example_executes_and_deletes_its_projection() {
    Spi::run(include_str!("../../../examples/projection_lifecycle.sql")).unwrap();
    assert_eq!(
        Spi::get_one::<i64>("SELECT count(*) FROM lifecycle_projection").unwrap(),
        Some(0)
    );
    assert_eq!(
        Spi::get_one::<i32>("SELECT value FROM lifecycle_source WHERE id = 1 LIMIT 1").unwrap(),
        Some(20)
    );
}

#[pg_test]
fn capture_is_a_fixed_copy_and_refresh_is_an_atomic_sql_update() {
    Spi::run("CREATE TABLE syntra_refresh_source (value int4)").unwrap();
    Spi::run("INSERT INTO syntra_refresh_source (value) VALUES (1), (2)").unwrap();
    Spi::run(
        r#"CREATE TEMP TABLE syntra_captured_projection AS
        SELECT 'example'::text AS name, 1::bigint AS generation, segment, source_snapshot
        FROM syntra_capture_int4('syntra_refresh_source'::regclass, 'value')"#,
    )
    .unwrap();
    Spi::run("UPDATE syntra_refresh_source SET value = 10 WHERE value = 1").unwrap();
    assert_eq!(
        Spi::get_one::<i64>(
            r#"SELECT s.sum FROM syntra_captured_projection AS p,
        LATERAL syntra_segment_summary(p.segment) AS s WHERE p.name = 'example' LIMIT 1"#
        )
        .unwrap(),
        Some(3)
    );
    Spi::run(
        r#"UPDATE syntra_captured_projection AS p SET generation = p.generation + 1,
        segment = c.segment, source_snapshot = c.source_snapshot
        FROM syntra_capture_int4('syntra_refresh_source'::regclass, 'value') AS c
        WHERE p.name = 'example'"#,
    )
    .unwrap();
    assert_eq!(
        Spi::get_one::<bool>(
            r#"SELECT p.generation = 2 AND s.sum = 12
        FROM syntra_captured_projection AS p, LATERAL syntra_segment_summary(p.segment) AS s
        WHERE p.name = 'example' LIMIT 1"#
        )
        .unwrap(),
        Some(true)
    );
}

#[pg_test]
fn capture_handles_empty_sources_and_quoted_identifiers() {
    Spi::run("CREATE TEMP TABLE \"Syntra Source\" (\"odd'name\" int4)").unwrap();
    assert_eq!(
        Spi::get_one::<bool>(
            r#"SELECT source_rows = 0 AND source_column = 'odd''name'
        FROM syntra_capture_int4('"Syntra Source"'::regclass, 'odd''name') LIMIT 1"#
        )
        .unwrap(),
        Some(true)
    );
}

#[pg_test(error = "source capture does not support row-level security")]
fn capture_rejects_rls_even_for_a_bypassing_owner() {
    Spi::run("CREATE TABLE syntra_rls_source (value int4)").unwrap();
    Spi::run("ALTER TABLE syntra_rls_source ENABLE ROW LEVEL SECURITY").unwrap();
    Spi::run("SELECT source_rows FROM syntra_capture_int4('syntra_rls_source'::regclass, 'value') LIMIT 1").unwrap();
}

#[pg_test(error = "source capture requires a stored, non-generated int4 column")]
fn capture_rejects_unsupported_types() {
    Spi::run("CREATE TABLE syntra_bigint_source (value bigint)").unwrap();
    Spi::run("SELECT source_rows FROM syntra_capture_int4('syntra_bigint_source'::regclass, 'value') LIMIT 1").unwrap();
}

#[pg_test(error = "source capture requires a stored, non-generated int4 column")]
fn capture_rejects_generated_columns() {
    Spi::run("CREATE TABLE syntra_generated_source (id int4, value int4 GENERATED ALWAYS AS (id + 1) STORED)").unwrap();
    Spi::run("SELECT source_rows FROM syntra_capture_int4('syntra_generated_source'::regclass, 'value') LIMIT 1").unwrap();
}

#[pg_test(error = "source column does not exist")]
fn capture_rejects_missing_columns() {
    Spi::run("CREATE TABLE syntra_missing_source (value int4)").unwrap();
    Spi::run("SELECT source_rows FROM syntra_capture_int4('syntra_missing_source'::regclass, 'other') LIMIT 1").unwrap();
}

#[pg_test(
    error = "source capture supports only ordinary heap tables without partitions or descendants"
)]
fn capture_rejects_inheritance_parents() {
    Spi::run("CREATE TABLE syntra_parent_source (value int4)").unwrap();
    Spi::run("CREATE TABLE syntra_child_source () INHERITS (syntra_parent_source)").unwrap();
    Spi::run("SELECT source_rows FROM syntra_capture_int4('syntra_parent_source'::regclass, 'value') LIMIT 1").unwrap();
}

#[pg_test(error = "source capture requires schema USAGE and table SELECT privileges")]
fn capture_cannot_bypass_table_permissions() {
    Spi::run("CREATE ROLE syntra_capture_unprivileged NOLOGIN").unwrap();
    Spi::run("CREATE TABLE public.syntra_private_source (value int4)").unwrap();
    Spi::run("SET LOCAL ROLE syntra_capture_unprivileged").unwrap();
    Spi::run("SELECT source_rows FROM syntra_capture_int4('public.syntra_private_source'::regclass, 'value') LIMIT 1").unwrap();
}

#[pg_test]
fn capture_allows_a_caller_with_explicit_read_privileges() {
    Spi::run("CREATE ROLE syntra_capture_reader NOLOGIN").unwrap();
    Spi::run("CREATE TABLE public.syntra_granted_source (value int4)").unwrap();
    Spi::run("INSERT INTO public.syntra_granted_source (value) VALUES (1), (NULL)").unwrap();
    Spi::run("GRANT SELECT ON public.syntra_granted_source TO syntra_capture_reader").unwrap();
    Spi::run("SET LOCAL ROLE syntra_capture_reader").unwrap();
    assert_eq!(Spi::get_one::<i64>("SELECT source_rows FROM syntra_capture_int4('public.syntra_granted_source'::regclass, 'value') LIMIT 1").unwrap(), Some(2));
    Spi::run("RESET ROLE").unwrap();
}

#[pg_test(error = "source capture requires schema USAGE and table SELECT privileges")]
fn capture_cannot_bypass_schema_permissions_with_an_oid() {
    Spi::run("CREATE ROLE syntra_capture_no_schema NOLOGIN").unwrap();
    Spi::run("CREATE SCHEMA syntra_private_namespace").unwrap();
    Spi::run("CREATE TABLE syntra_private_namespace.secret_source (value int4)").unwrap();
    Spi::run("GRANT SELECT ON syntra_private_namespace.secret_source TO syntra_capture_no_schema")
        .unwrap();
    Spi::run("SET LOCAL ROLE syntra_capture_no_schema").unwrap();
    Spi::run(
        r#"SELECT source_rows FROM syntra_capture_int4(
        (SELECT c.oid::regclass FROM pg_class AS c JOIN pg_namespace AS n ON c.relnamespace = n.oid
         WHERE n.nspname = 'syntra_private_namespace' AND c.relname = 'secret_source' LIMIT 1),
        'value') LIMIT 1"#,
    )
    .unwrap();
}
