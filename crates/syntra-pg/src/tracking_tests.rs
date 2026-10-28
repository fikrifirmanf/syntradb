#[pg_test]
fn tracking_counts_statements_including_truncate_and_replica_writes() {
    Spi::run("CREATE TABLE tracked_mutations (v int4); INSERT INTO tracked_mutations (v) VALUES (1), (2)").unwrap();
    let epoch = Spi::get_one::<i64>("SELECT syntra_track_source('tracked_mutations', 'v')")
        .unwrap()
        .unwrap();
    assert!(epoch > 0);
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT generation FROM syntra_source_tracking_status('tracked_mutations') LIMIT 1"
        )
        .unwrap(),
        Some(0)
    );
    Spi::run("INSERT INTO tracked_mutations (v) VALUES (3), (4); UPDATE tracked_mutations SET v = 5 WHERE v = 1; DELETE FROM tracked_mutations WHERE v = 2; TRUNCATE tracked_mutations; SET LOCAL session_replication_role = replica; INSERT INTO tracked_mutations (v) VALUES (9); SET LOCAL session_replication_role = origin").unwrap();
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT generation FROM syntra_source_tracking_status('tracked_mutations') LIMIT 1"
        )
        .unwrap(),
        Some(5)
    );
    assert_eq!(
        Spi::get_one::<i32>("SELECT v FROM tracked_mutations LIMIT 1").unwrap(),
        Some(9)
    );
}

#[pg_test]
fn tracking_rolls_back_with_failed_source_changes() {
    Spi::run("CREATE TABLE tracked_abort (v int4); INSERT INTO tracked_abort (v) VALUES (7); SELECT syntra_track_source('tracked_abort', 'v')").unwrap();
    Spi::run("DO $$ BEGIN UPDATE tracked_abort SET v = 8 WHERE v = 7; RAISE EXCEPTION 'force rollback'; EXCEPTION WHEN raise_exception THEN RAISE NOTICE 'expected rollback'; END $$").unwrap();
    assert_eq!(
        Spi::get_one::<i32>("SELECT v FROM tracked_abort LIMIT 1").unwrap(),
        Some(7)
    );
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT generation FROM syntra_source_tracking_status('tracked_abort') LIMIT 1"
        )
        .unwrap(),
        Some(0)
    );
}

#[pg_test]
fn tracking_fires_for_an_unprivileged_writer_without_catalog_access() {
    Spi::run("CREATE ROLE tracked_writer NOLOGIN; CREATE TABLE public.tracked_privileges (v int4); SELECT syntra_track_source('public.tracked_privileges', 'v'); GRANT INSERT ON public.tracked_privileges TO tracked_writer; SET LOCAL ROLE tracked_writer; INSERT INTO public.tracked_privileges (v) VALUES (11); RESET ROLE").unwrap();
    assert_eq!(Spi::get_one::<i64>("SELECT generation FROM syntra_source_tracking_status('public.tracked_privileges') LIMIT 1").unwrap(), Some(1));
    assert_eq!(
        Spi::get_one::<i32>("SELECT v FROM public.tracked_privileges LIMIT 1").unwrap(),
        Some(11)
    );
}

#[pg_test(error = "permission denied for table tracked_sources")]
fn tracking_catalog_is_private() {
    Spi::run("CREATE ROLE tracked_outsider NOLOGIN; SET LOCAL ROLE tracked_outsider").unwrap();
    Spi::run("SELECT epoch FROM syntra.tracked_sources LIMIT 1").unwrap();
}

#[pg_test(error = "source tracking setup currently requires a superuser")]
fn tracking_checks_admin_privilege_even_after_an_execute_grant() {
    Spi::run("CREATE ROLE tracked_nonadmin NOLOGIN; CREATE TABLE public.tracked_denied (v int4); GRANT EXECUTE ON FUNCTION syntra_track_source(regclass, text) TO tracked_nonadmin; SET LOCAL ROLE tracked_nonadmin").unwrap();
    Spi::run("SELECT syntra_track_source('public.tracked_denied', 'v')").unwrap();
}

#[pg_test(error = "source tracking requires a permanent heap table without inheritance")]
fn tracking_rejects_temporary_tables() {
    Spi::run("CREATE TEMP TABLE tracked_temp (v int4)").unwrap();
    Spi::run("SELECT syntra_track_source('tracked_temp', 'v')").unwrap();
}

#[pg_test(error = "source tracking requires a permanent heap table without inheritance")]
fn tracking_rejects_unlogged_tables() {
    Spi::run("CREATE UNLOGGED TABLE tracked_unlogged (v int4)").unwrap();
    Spi::run("SELECT syntra_track_source('tracked_unlogged', 'v')").unwrap();
}

#[pg_test(error = "source tracking requires a permanent heap table without inheritance")]
fn tracking_rejects_inheritance_children() {
    Spi::run("CREATE TABLE tracked_parent (v int4); CREATE TABLE tracked_child () INHERITS (tracked_parent)").unwrap();
    Spi::run("SELECT syntra_track_source('tracked_child', 'v')").unwrap();
}

#[pg_test(
    error = "source tracking registration is missing; stop writes and repair the registration"
)]
fn tracking_fails_closed_if_its_private_registration_disappears() {
    Spi::run("CREATE TABLE tracked_missing (v int4); SELECT syntra_track_source('tracked_missing', 'v'); DELETE FROM syntra.tracked_sources WHERE source_relation = 'tracked_missing'::regclass::oid").unwrap();
    Spi::run("INSERT INTO tracked_missing (v) VALUES (1)").unwrap();
}

#[pg_test]
fn tracking_installs_on_quoted_identifiers_without_dynamic_sql() {
    Spi::run(r#"CREATE TABLE "tracked'; odd" ("value'" int4)"#).unwrap();
    assert!(
        Spi::get_one::<i64>(r#"SELECT syntra_track_source('"tracked''; odd"', 'value''')"#)
            .unwrap()
            .unwrap()
            > 0
    );
    Spi::run(r#"INSERT INTO "tracked'; odd" ("value'") VALUES (13)"#).unwrap();
    assert_eq!(
        Spi::get_one::<i64>(
            r#"SELECT generation FROM syntra_source_tracking_status('"tracked''; odd"') LIMIT 1"#
        )
        .unwrap(),
        Some(1)
    );
}

#[pg_test]
fn tracking_health_detects_disabled_dropped_and_rls_guards() {
    Spi::run(
        "CREATE TABLE tracked_health (v int4); SELECT syntra_track_source('tracked_health', 'v')",
    )
    .unwrap();
    assert_eq!(
        Spi::get_one::<bool>(
            "SELECT guard_config_valid FROM syntra_source_tracking_status('tracked_health') LIMIT 1"
        )
        .unwrap(),
        Some(true)
    );
    Spi::run("ALTER TABLE tracked_health DISABLE TRIGGER _syntra_source_changed; INSERT INTO tracked_health (v) VALUES (1)").unwrap();
    assert_eq!(
        Spi::get_one::<bool>(
            "SELECT guard_config_valid FROM syntra_source_tracking_status('tracked_health') LIMIT 1"
        )
        .unwrap(),
        Some(false)
    );
    Spi::run("ALTER TABLE tracked_health ENABLE ALWAYS TRIGGER _syntra_source_changed; ALTER TABLE tracked_health ENABLE ROW LEVEL SECURITY").unwrap();
    assert_eq!(
        Spi::get_one::<bool>(
            "SELECT guard_config_valid FROM syntra_source_tracking_status('tracked_health') LIMIT 1"
        )
        .unwrap(),
        Some(false)
    );
    Spi::run("ALTER TABLE tracked_health DISABLE ROW LEVEL SECURITY; DROP TRIGGER _syntra_source_changed ON tracked_health").unwrap();
    assert_eq!(
        Spi::get_one::<bool>(
            "SELECT guard_config_valid FROM syntra_source_tracking_status('tracked_health') LIMIT 1"
        )
        .unwrap(),
        Some(false)
    );
}

#[pg_test]
fn tracking_rebind_uses_a_new_epoch_and_resets_the_counter() {
    Spi::run("CREATE TABLE tracked_rebind (v int4)").unwrap();
    let first = Spi::get_one::<i64>("SELECT syntra_track_source('tracked_rebind', 'v')")
        .unwrap()
        .unwrap();
    Spi::run("INSERT INTO tracked_rebind (v) VALUES (1)").unwrap();
    let second = Spi::get_one::<i64>("SELECT syntra_track_source('tracked_rebind', 'v')")
        .unwrap()
        .unwrap();
    assert!(second > first);
    assert_eq!(Spi::get_one::<bool>("SELECT generation = 0 AND guard_config_valid FROM syntra_source_tracking_status('tracked_rebind') LIMIT 1").unwrap(), Some(true));
    Spi::run("INSERT INTO tracked_rebind (v) VALUES (2)").unwrap();
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT generation FROM syntra_source_tracking_status('tracked_rebind') LIMIT 1"
        )
        .unwrap(),
        Some(1)
    );
}

#[pg_test(
    error = "existing source tracking trigger is incompatible; repair it before registration"
)]
fn tracking_does_not_adopt_a_disabled_trigger() {
    Spi::run("CREATE TABLE tracked_incompatible (v int4); SELECT syntra_track_source('tracked_incompatible', 'v'); ALTER TABLE tracked_incompatible DISABLE TRIGGER _syntra_source_changed").unwrap();
    Spi::run("SELECT syntra_track_source('tracked_incompatible', 'v')").unwrap();
}

#[pg_test]
fn tracking_initializes_a_bounded_vector_and_updates_one_transaction_shard() {
    Spi::run("CREATE TABLE tracked_vector (v int4); SELECT syntra_track_source('tracked_vector', 'v'); INSERT INTO tracked_vector (v) VALUES (1); UPDATE tracked_vector SET v = 2 WHERE v = 1").unwrap();
    assert_eq!(Spi::get_one::<bool>("SELECT generation = 2 AND guard_config_valid AND array_length(generation_vector, 1) = 16 AND (SELECT count(*) FROM unnest(generation_vector) AS v WHERE v = 2) = 1 AND (SELECT count(*) FROM unnest(generation_vector) AS v WHERE v = 0) = 15 FROM syntra_source_tracking_status('tracked_vector') LIMIT 1").unwrap(), Some(true));
}

#[pg_test]
fn tracking_rebind_repairs_a_missing_shard_and_resets_the_vector() {
    Spi::run("CREATE TABLE tracked_shard_repair (v int4); SELECT syntra_track_source('tracked_shard_repair', 'v'); DELETE FROM syntra.source_generation_shards WHERE source_relation = 'tracked_shard_repair'::regclass::oid AND shard = 0").unwrap();
    assert_eq!(Spi::get_one::<bool>("SELECT guard_config_valid FROM syntra_source_tracking_status('tracked_shard_repair') LIMIT 1").unwrap(), Some(false));
    Spi::run("SELECT syntra_track_source('tracked_shard_repair', 'v')").unwrap();
    assert_eq!(Spi::get_one::<bool>("SELECT generation = 0 AND guard_config_valid AND array_length(generation_vector, 1) = 16 FROM syntra_source_tracking_status('tracked_shard_repair') LIMIT 1").unwrap(), Some(true));
}

#[pg_test]
fn tracking_counter_exhaustion_rejects_and_rolls_back_the_source_write() {
    Spi::run("CREATE TABLE tracked_exhausted (v int4); SELECT syntra_track_source('tracked_exhausted', 'v'); UPDATE syntra.source_generation_shards SET generation = 576460752303423487 WHERE source_relation = 'tracked_exhausted'::regclass::oid").unwrap();
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT generation FROM syntra_source_tracking_status('tracked_exhausted') LIMIT 1"
        )
        .unwrap(),
        Some(9223372036854775792)
    );
    Spi::run("DO $$ BEGIN INSERT INTO tracked_exhausted (v) VALUES (1); RAISE EXCEPTION 'counter exhaustion was ignored'; EXCEPTION WHEN check_violation THEN RAISE NOTICE 'expected counter exhaustion'; END $$").unwrap();
    assert_eq!(
        Spi::get_one::<i64>("SELECT count(*) FROM tracked_exhausted").unwrap(),
        Some(0)
    );
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT generation FROM syntra_source_tracking_status('tracked_exhausted') LIMIT 1"
        )
        .unwrap(),
        Some(9223372036854775792)
    );
}

#[pg_test]
fn tracking_drop_cleans_shards_even_when_replica_mode_skips_foreign_keys() {
    Spi::run("CREATE TABLE tracked_replica_drop (v int4); SELECT syntra_track_source('tracked_replica_drop', 'v')").unwrap();
    let source = Spi::get_one::<pg_sys::Oid>("SELECT 'tracked_replica_drop'::regclass::oid")
        .unwrap()
        .unwrap();
    Spi::run("SET LOCAL session_replication_role = replica; DROP TABLE tracked_replica_drop; SET LOCAL session_replication_role = origin").unwrap();
    assert_eq!(Spi::get_one_with_args::<bool>("SELECT NOT EXISTS (SELECT source_relation FROM syntra.tracked_sources WHERE source_relation = $1) AND NOT EXISTS (SELECT source_relation FROM syntra.source_generation_shards WHERE source_relation = $1)", &[source.into()]).unwrap(), Some(true));
}

#[pg_test(error = "permission denied for table source_generation_shards")]
fn tracking_shard_catalog_is_private() {
    Spi::run("CREATE ROLE tracked_shard_outsider NOLOGIN; SET LOCAL ROLE tracked_shard_outsider")
        .unwrap();
    Spi::run("SELECT generation FROM syntra.source_generation_shards LIMIT 1").unwrap();
}
