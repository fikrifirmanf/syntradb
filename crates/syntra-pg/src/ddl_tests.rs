#[pg_test]
fn ddl_epochs_cover_an_owners_disable_write_enable_gap() {
    Spi::run("CREATE ROLE ddl_source_owner NOLOGIN; CREATE TABLE public.ddl_gap (v int4); INSERT INTO public.ddl_gap (v) VALUES (1); ALTER TABLE public.ddl_gap OWNER TO ddl_source_owner; SELECT syntra_track_source('public.ddl_gap', 'v')").unwrap();
    let before = Spi::get_one::<i64>(
        "SELECT epoch FROM syntra_source_tracking_status('public.ddl_gap') LIMIT 1",
    )
    .unwrap()
    .unwrap();
    Spi::run("SET LOCAL ROLE ddl_source_owner; ALTER TABLE public.ddl_gap DISABLE TRIGGER _syntra_source_changed; UPDATE public.ddl_gap SET v = 2 WHERE v = 1; ALTER TABLE public.ddl_gap ENABLE ALWAYS TRIGGER _syntra_source_changed; RESET ROLE").unwrap();
    let after = Spi::get_one::<i64>(
        "SELECT epoch FROM syntra_source_tracking_status('public.ddl_gap') LIMIT 1",
    )
    .unwrap()
    .unwrap();
    assert!(after > before);
    assert_eq!(Spi::get_one::<bool>("SELECT generation = 0 AND guard_config_valid FROM syntra_source_tracking_status('public.ddl_gap') LIMIT 1").unwrap(), Some(true));
    assert_eq!(
        Spi::get_one::<i32>("SELECT v FROM public.ddl_gap LIMIT 1").unwrap(),
        Some(2)
    );
}

#[pg_test]
fn ddl_epochs_only_touch_affected_sources() {
    Spi::run("CREATE TABLE ddl_first (v int4); CREATE TABLE ddl_second (v int4); SELECT syntra_track_source('ddl_first', 'v'); SELECT syntra_track_source('ddl_second', 'v')").unwrap();
    let first =
        Spi::get_one::<i64>("SELECT epoch FROM syntra_source_tracking_status('ddl_first') LIMIT 1")
            .unwrap()
            .unwrap();
    let second = Spi::get_one::<i64>(
        "SELECT epoch FROM syntra_source_tracking_status('ddl_second') LIMIT 1",
    )
    .unwrap()
    .unwrap();
    Spi::run(
        "CREATE TEMP TABLE ddl_unrelated (v int4); ALTER TABLE ddl_first ADD COLUMN extra int4",
    )
    .unwrap();
    assert!(
        Spi::get_one::<i64>("SELECT epoch FROM syntra_source_tracking_status('ddl_first') LIMIT 1")
            .unwrap()
            .unwrap()
            > first
    );
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT epoch FROM syntra_source_tracking_status('ddl_second') LIMIT 1"
        )
        .unwrap(),
        Some(second)
    );
}

#[pg_test]
fn ddl_drop_removes_source_registrations() {
    Spi::run("CREATE TABLE ddl_removed (v int4); SELECT syntra_track_source('ddl_removed', 'v')")
        .unwrap();
    let oid = Spi::get_one::<pg_sys::Oid>("SELECT 'ddl_removed'::regclass::oid")
        .unwrap()
        .unwrap();
    Spi::run("DROP TABLE ddl_removed").unwrap();
    assert_eq!(
        Spi::get_one_with_args::<i64>(
            "SELECT count(*) FROM syntra.tracked_sources WHERE source_relation = $1",
            &[oid.into()]
        )
        .unwrap(),
        Some(0)
    );
}

#[pg_test]
fn ddl_epoch_and_guard_changes_roll_back_together() {
    Spi::run("CREATE TABLE ddl_abort (v int4); SELECT syntra_track_source('ddl_abort', 'v')")
        .unwrap();
    let epoch =
        Spi::get_one::<i64>("SELECT epoch FROM syntra_source_tracking_status('ddl_abort') LIMIT 1")
            .unwrap()
            .unwrap();
    Spi::run("DO $$ BEGIN ALTER TABLE ddl_abort DISABLE TRIGGER _syntra_source_changed; RAISE EXCEPTION 'force rollback'; EXCEPTION WHEN raise_exception THEN RAISE NOTICE 'expected DDL rollback'; END $$").unwrap();
    assert_eq!(
        Spi::get_one::<i64>("SELECT epoch FROM syntra_source_tracking_status('ddl_abort') LIMIT 1")
            .unwrap(),
        Some(epoch)
    );
    Spi::run("INSERT INTO ddl_abort (v) VALUES (5)").unwrap();
    assert_eq!(Spi::get_one::<bool>("SELECT generation = 1 AND guard_config_valid FROM syntra_source_tracking_status('ddl_abort') LIMIT 1").unwrap(), Some(true));
}

#[pg_test]
fn ddl_guard_drop_advances_epoch_and_rebind_repairs_it() {
    Spi::run(
        "CREATE TABLE ddl_guard_drop (v int4); SELECT syntra_track_source('ddl_guard_drop', 'v')",
    )
    .unwrap();
    let before = Spi::get_one::<i64>(
        "SELECT epoch FROM syntra_source_tracking_status('ddl_guard_drop') LIMIT 1",
    )
    .unwrap()
    .unwrap();
    Spi::run("DROP TRIGGER _syntra_source_changed ON ddl_guard_drop").unwrap();
    assert!(
        Spi::get_one::<i64>(
            "SELECT epoch FROM syntra_source_tracking_status('ddl_guard_drop') LIMIT 1"
        )
        .unwrap()
        .unwrap()
            > before
    );
    assert_eq!(Spi::get_one::<bool>("SELECT guard_config_valid FROM syntra_source_tracking_status('ddl_guard_drop') LIMIT 1").unwrap(), Some(false));
    Spi::run("SELECT syntra_track_source('ddl_guard_drop', 'v'); INSERT INTO ddl_guard_drop (v) VALUES (4)").unwrap();
    assert_eq!(Spi::get_one::<bool>("SELECT generation = 1 AND guard_config_valid FROM syntra_source_tracking_status('ddl_guard_drop') LIMIT 1").unwrap(), Some(true));
}

#[pg_test]
fn ddl_epochs_fire_in_replica_mode() {
    Spi::run("CREATE TABLE ddl_replica (v int4); SELECT syntra_track_source('ddl_replica', 'v')")
        .unwrap();
    let before = Spi::get_one::<i64>(
        "SELECT epoch FROM syntra_source_tracking_status('ddl_replica') LIMIT 1",
    )
    .unwrap()
    .unwrap();
    Spi::run("SET LOCAL session_replication_role = replica; ALTER TABLE ddl_replica DISABLE TRIGGER _syntra_source_changed; INSERT INTO ddl_replica (v) VALUES (3); ALTER TABLE ddl_replica ENABLE ALWAYS TRIGGER _syntra_source_changed; SET LOCAL session_replication_role = origin").unwrap();
    assert!(
        Spi::get_one::<i64>(
            "SELECT epoch FROM syntra_source_tracking_status('ddl_replica') LIMIT 1"
        )
        .unwrap()
        .unwrap()
            > before
    );
    assert_eq!(Spi::get_one::<bool>("SELECT generation = 0 AND guard_config_valid FROM syntra_source_tracking_status('ddl_replica') LIMIT 1").unwrap(), Some(true));
}

#[pg_test(error = "DDL epoch guard requires an event trigger context")]
fn ddl_guard_rejects_direct_calls() {
    Spi::run("SELECT syntra_ddl_epoch_guard()").unwrap();
}

#[pg_test]
fn ddl_status_detects_disabled_event_guards_and_session_bypass() {
    Spi::run("CREATE TABLE ddl_guard_status (v int4); SELECT syntra_track_source('ddl_guard_status', 'v'); ALTER EVENT TRIGGER syntra_ddl_epoch_guard DISABLE").unwrap();
    assert_eq!(Spi::get_one::<bool>("SELECT guard_config_valid FROM syntra_source_tracking_status('ddl_guard_status') LIMIT 1").unwrap(), Some(false));
    Spi::run("ALTER EVENT TRIGGER syntra_ddl_epoch_guard ENABLE ALWAYS; SET LOCAL event_triggers = false").unwrap();
    assert_eq!(Spi::get_one::<bool>("SELECT guard_config_valid FROM syntra_source_tracking_status('ddl_guard_status') LIMIT 1").unwrap(), Some(false));
    Spi::run("SET LOCAL event_triggers = true").unwrap();
}

#[pg_test(error = "source tracking requires active DDL guards")]
fn ddl_disabled_guards_prevent_source_registration() {
    Spi::run("CREATE TABLE ddl_register_denied (v int4); ALTER EVENT TRIGGER syntra_ddl_drop_guard DISABLE").unwrap();
    Spi::run("SELECT syntra_track_source('ddl_register_denied', 'v')").unwrap();
}

#[pg_test]
fn ddl_schema_cascade_removes_all_affected_registrations() {
    Spi::run("CREATE SCHEMA ddl_cascade; CREATE TABLE ddl_cascade.first_source (v int4); CREATE TABLE ddl_cascade.second_source (v int4); SELECT syntra_track_source('ddl_cascade.first_source', 'v'); SELECT syntra_track_source('ddl_cascade.second_source', 'v')").unwrap();
    let (first, second) = Spi::get_two::<pg_sys::Oid, pg_sys::Oid>("SELECT 'ddl_cascade.first_source'::regclass::oid, 'ddl_cascade.second_source'::regclass::oid").unwrap();
    Spi::run("DROP SCHEMA ddl_cascade CASCADE").unwrap();
    assert_eq!(Spi::get_one_with_args::<i64>("SELECT count(*) FROM syntra.tracked_sources WHERE source_relation = $1 OR source_relation = $2", &[first.unwrap().into(), second.unwrap().into()]).unwrap(), Some(0));
}

#[pg_test]
fn ddl_source_drop_rollback_restores_registration_and_guard() {
    Spi::run("CREATE TABLE ddl_drop_abort (v int4); SELECT syntra_track_source('ddl_drop_abort', 'v'); DO $$ BEGIN DROP TABLE ddl_drop_abort; RAISE EXCEPTION 'force rollback'; EXCEPTION WHEN raise_exception THEN RAISE NOTICE 'expected drop rollback'; END $$; INSERT INTO ddl_drop_abort (v) VALUES (6)").unwrap();
    assert_eq!(Spi::get_one::<bool>("SELECT generation = 1 AND guard_config_valid FROM syntra_source_tracking_status('ddl_drop_abort') LIMIT 1").unwrap(), Some(true));
    assert_eq!(
        Spi::get_one::<i32>("SELECT v FROM ddl_drop_abort LIMIT 1").unwrap(),
        Some(6)
    );
}
