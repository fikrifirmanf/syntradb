#[pg_test]
fn managed_projection_refreshes_data_and_provenance_together() {
    Spi::run("CREATE TABLE managed_source (v int4); INSERT INTO managed_source (v) VALUES (2), (NULL), (7)").unwrap();
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT syntra_publish_fixed_projection('daily', 'managed_source', 'v')"
        )
        .unwrap(),
        Some(1)
    );
    Spi::run("UPDATE managed_source SET v = 20 WHERE v = 2").unwrap();
    assert_eq!(
        Spi::get_one::<i64>("SELECT sum FROM syntra_fixed_projection_summary('daily') LIMIT 1")
            .unwrap(),
        Some(9)
    );
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT syntra_publish_fixed_projection('daily', 'managed_source', 'v')"
        )
        .unwrap(),
        Some(2)
    );
    assert_eq!(Spi::get_one::<bool>("SELECT generation = 2 AND rows = 3 AND non_nulls = 2 AND sum = 27 AND min = 7 AND max = 20 FROM syntra_fixed_projection_summary('daily') LIMIT 1").unwrap(), Some(true));
    assert_eq!(Spi::get_one::<bool>("SELECT source_rows = 3 AND source_relation = 'managed_source'::regclass::oid AND source_column = 'v' AND source_snapshot <> '' FROM syntra.fixed_projections WHERE owner_oid = CURRENT_USER::regrole::oid AND name = 'daily' LIMIT 1").unwrap(), Some(true));
    assert_eq!(
        Spi::get_one::<bool>("SELECT syntra_delete_fixed_projection('daily')").unwrap(),
        Some(true)
    );
    assert_eq!(
        Spi::get_one::<bool>("SELECT syntra_delete_fixed_projection('daily')").unwrap(),
        Some(false)
    );
    assert_eq!(
        Spi::get_one::<i64>("SELECT count(*) FROM syntra_fixed_projection_summary('daily')")
            .unwrap(),
        Some(0)
    );
}

#[pg_test]
fn managed_projection_names_are_data_and_null_arguments_are_strict() {
    Spi::run("CREATE TEMP TABLE managed_quoted_source (v int4)").unwrap();
    assert_eq!(Spi::get_one::<i64>(r#"SELECT syntra_publish_fixed_projection('x''; DROP TABLE managed_quoted_source; --', 'managed_quoted_source', 'v')"#).unwrap(), Some(1));
    assert_eq!(Spi::get_one::<i64>(r#"SELECT rows FROM syntra_fixed_projection_summary('x''; DROP TABLE managed_quoted_source; --') LIMIT 1"#).unwrap(), Some(0));
    assert_eq!(
        Spi::get_one::<i64>("SELECT count(*) FROM managed_quoted_source").unwrap(),
        Some(0)
    );
    assert_eq!(
        Spi::get_one::<bool>(
            "SELECT syntra_publish_fixed_projection(NULL, 'managed_quoted_source', 'v') IS NULL"
        )
        .unwrap(),
        Some(true)
    );
    assert_eq!(
        Spi::get_one::<i64>("SELECT count(*) FROM syntra_fixed_projection_summary(NULL)").unwrap(),
        Some(0)
    );
}

#[pg_test]
fn managed_projection_roles_cannot_read_refresh_or_delete_each_others_copies() {
    Spi::run("CREATE ROLE managed_alice NOLOGIN; CREATE ROLE managed_bob NOLOGIN; CREATE TABLE public.managed_shared (v int4); INSERT INTO public.managed_shared (v) VALUES (8); GRANT SELECT ON public.managed_shared TO managed_alice, managed_bob").unwrap();
    Spi::run("SET LOCAL ROLE managed_alice").unwrap();
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT syntra_publish_fixed_projection('same', 'public.managed_shared', 'v')"
        )
        .unwrap(),
        Some(1)
    );
    Spi::run("RESET ROLE; SET LOCAL ROLE managed_bob").unwrap();
    assert_eq!(
        Spi::get_one::<i64>("SELECT count(*) FROM syntra.fixed_projections").unwrap(),
        Some(0)
    );
    assert_eq!(
        Spi::get_one::<i64>("SELECT count(*) FROM syntra_fixed_projection_summary('same')")
            .unwrap(),
        Some(0)
    );
    assert_eq!(
        Spi::get_one::<bool>("SELECT syntra_delete_fixed_projection('same')").unwrap(),
        Some(false)
    );
    Spi::run("UPDATE syntra.fixed_projections SET generation = 99 WHERE name = 'same'").unwrap();
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT syntra_publish_fixed_projection('same', 'public.managed_shared', 'v')"
        )
        .unwrap(),
        Some(1)
    );
    Spi::run("RESET ROLE; SET LOCAL ROLE managed_alice").unwrap();
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT generation FROM syntra_fixed_projection_summary('same') LIMIT 1"
        )
        .unwrap(),
        Some(1)
    );
    Spi::run("RESET ROLE").unwrap();
}

#[pg_test(error = "source capture requires schema USAGE and table SELECT privileges")]
fn managed_publish_checks_source_permissions_as_the_invoker() {
    Spi::run("CREATE ROLE managed_denied NOLOGIN; CREATE TABLE public.managed_secret (v int4); SET LOCAL ROLE managed_denied").unwrap();
    Spi::run("SELECT syntra_publish_fixed_projection('secret', 'public.managed_secret', 'v')")
        .unwrap();
}

#[pg_test(error = "new row violates row-level security policy for table \"fixed_projections\"")]
fn managed_projection_owner_cannot_be_forged() {
    Spi::run("CREATE ROLE managed_forger NOLOGIN; CREATE TABLE public.managed_forge_source (v int4); GRANT SELECT ON public.managed_forge_source TO managed_forger; SET LOCAL ROLE managed_forger").unwrap();
    Spi::run("INSERT INTO syntra.fixed_projections (owner_oid, name, generation, segment, source_database, source_relation, source_schema, source_table, source_column, source_attribute, source_type, source_snapshot, source_transaction, source_command, captured_at, source_rows) SELECT 1, 'forged', 1, segment, source_database, source_relation, source_schema, source_table, source_column, source_attribute, source_type, source_snapshot, source_transaction, source_command, captured_at, source_rows FROM syntra_capture_int4('public.managed_forge_source'::regclass, 'v')").unwrap();
}

#[pg_test(
    error = "new row for relation \"fixed_projections\" violates check constraint \"fixed_projections_name_check\""
)]
fn managed_projection_rejects_blank_names() {
    Spi::run("CREATE TEMP TABLE managed_blank_source (v int4)").unwrap();
    Spi::run("SELECT syntra_publish_fixed_projection('   ', 'managed_blank_source', 'v')").unwrap();
}

#[pg_test]
fn managed_fixed_copy_remains_readable_after_its_source_is_dropped() {
    Spi::run("CREATE TABLE managed_dropped_source (v int4); INSERT INTO managed_dropped_source (v) VALUES (5); SELECT syntra_track_source('managed_dropped_source', 'v'); SELECT syntra_publish_fixed_projection('retained', 'managed_dropped_source', 'v'); DROP TABLE managed_dropped_source").unwrap();
    assert_eq!(Spi::get_one::<bool>("SELECT generation = 1 AND rows = 1 AND sum = 5 FROM syntra_fixed_projection_summary('retained') LIMIT 1").unwrap(), Some(true));
}
