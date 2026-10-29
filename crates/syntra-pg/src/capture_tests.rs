fn capture_fixture(sql: &str) {
    // Serialize private-catalog fixtures against parallel pgrx DDL cleanup.
    Spi::run("LOCK TABLE syntra.capture_checkpoints IN SHARE ROW EXCLUSIVE MODE").unwrap();
    Spi::run("LOCK TABLE syntra.capture_heads IN SHARE ROW EXCLUSIVE MODE").unwrap();
    // A pg_test never commits, so the deferred capture trigger would never fire.
    // Firing it per statement checks the same trigger logic; tests of deferral
    // itself switch back with SET CONSTRAINTS ALL DEFERRED.
    Spi::run("SET CONSTRAINTS ALL IMMEDIATE").unwrap();
    // Test oracle for the feed position, derived in SQL apart from the engine:
    // the newest mark above the head's folded floor, else the floor.
    Spi::run("CREATE OR REPLACE TEMP VIEW capture_position AS SELECT h.source_relation,h.epoch,h.base_rows,h.pruned,coalesce(m.ordinal,h.emitted) AS emitted,coalesce(m.live_rows,h.live_rows) AS live_rows FROM syntra.capture_heads AS h LEFT JOIN LATERAL (SELECT ordinal,live_rows FROM syntra.capture_marks WHERE epoch=h.epoch AND ordinal>h.emitted ORDER BY ordinal DESC LIMIT 1) AS m ON true").unwrap();
    Spi::run(sql).unwrap();
}

#[pg_test]
fn capture_seed_and_events_preserve_nulls_keys_and_transaction_provenance() {
    capture_fixture("CREATE TABLE capture_values(id int4 PRIMARY KEY,v int4); INSERT INTO capture_values(id,v) VALUES(1,10),(2,NULL); SELECT syntra_seed_capture('capture_values','id','v',16)");
    assert_eq!(Spi::get_one::<bool>("SELECT base_rows=2 AND emitted=0 AND (SELECT count(*) FROM syntra.capture_base_rows WHERE source_relation='capture_values'::regclass)=2 FROM capture_position WHERE source_relation='capture_values'::regclass LIMIT 1").unwrap(),Some(true));
    Spi::run("UPDATE capture_values SET id=3,v=11 WHERE id=1; INSERT INTO capture_values(id,v) VALUES(4,NULL); DELETE FROM capture_values WHERE id=2").unwrap();
    assert_eq!(Spi::get_one::<bool>("SELECT count(*)=4 AND array_agg(key ORDER BY ordinal)=ARRAY[1,3,4,2]::int8[] AND bool_and(origin_xid=pg_current_xact_id()) AND min(command_id)>=0 FROM syntra.capture_events WHERE source_relation='capture_values'::regclass").unwrap(),Some(true));
    assert_eq!(Spi::get_one::<bool>("SELECT NOT before_present AND before_value IS NULL AND after_present AND after_value IS NULL FROM syntra.capture_events WHERE source_relation='capture_values'::regclass AND ordinal=3 LIMIT 1").unwrap(),Some(true));
}

#[pg_test]
fn capture_failure_rolls_back_source_event_reservation_and_all_event_rows() {
    capture_fixture("CREATE TABLE capture_bounded(id int4 PRIMARY KEY,v int4); INSERT INTO capture_bounded(id,v) VALUES(1,5); SELECT syntra_seed_capture('capture_bounded','id','v',2); UPDATE capture_bounded SET id=2 WHERE id=1");
    Spi::run("DO $$ BEGIN INSERT INTO capture_bounded(id,v) VALUES(3,9); EXCEPTION WHEN program_limit_exceeded THEN NULL; END $$").unwrap();
    assert_eq!(Spi::get_one::<bool>("SELECT (SELECT count(*)=1 AND min(id)=2 AND sum(v)=5 FROM capture_bounded) AND (SELECT emitted=2 FROM capture_position WHERE source_relation='capture_bounded'::regclass) AND (SELECT count(*)=2 FROM syntra.capture_events WHERE source_relation='capture_bounded'::regclass)").unwrap(),Some(true));
    Spi::run("DO $$ BEGIN DELETE FROM capture_bounded WHERE id=2; RAISE EXCEPTION 'rollback capture'; EXCEPTION WHEN program_limit_exceeded OR raise_exception THEN NULL; END $$").unwrap();
    assert_eq!(
        Spi::get_one::<i64>("SELECT count(*) FROM capture_bounded").unwrap(),
        Some(1)
    );
}

#[pg_test]
fn capture_reseed_replaces_base_and_epoch_and_resets_the_bounded_log() {
    capture_fixture("CREATE TABLE capture_reset(id int4 PRIMARY KEY,v int4); INSERT INTO capture_reset(id,v) VALUES(1,5); SELECT syntra_seed_capture('capture_reset','id','v',2); UPDATE capture_reset SET v=6 WHERE id=1");
    let before=Spi::get_one::<i64>("SELECT epoch FROM syntra.capture_heads WHERE source_relation='capture_reset'::regclass LIMIT 1").unwrap().unwrap();
    let after = Spi::get_one::<i64>("SELECT syntra_seed_capture('capture_reset','id','v',2)")
        .unwrap()
        .unwrap();
    assert!(after > before);
    assert_eq!(Spi::get_one::<bool>("SELECT emitted=0 AND (SELECT value=6 FROM syntra.capture_base_rows WHERE source_relation='capture_reset'::regclass LIMIT 1) AND NOT EXISTS(SELECT ordinal FROM syntra.capture_events WHERE source_relation='capture_reset'::regclass) FROM capture_position WHERE source_relation='capture_reset'::regclass LIMIT 1").unwrap(),Some(true));
}

#[pg_test]
fn capture_unprivileged_writes_append_private_events_in_replica_mode() {
    capture_fixture("CREATE ROLE capture_writer NOLOGIN; CREATE TABLE public.capture_permissions(id int4 PRIMARY KEY,v int4); GRANT INSERT,UPDATE,DELETE,SELECT ON public.capture_permissions TO capture_writer; SELECT syntra_seed_capture('public.capture_permissions','id','v',8); SET LOCAL ROLE capture_writer; INSERT INTO public.capture_permissions(id,v) VALUES(1,NULL); RESET ROLE; SET LOCAL session_replication_role=replica; UPDATE public.capture_permissions SET v=7 WHERE id=1");
    assert_eq!(Spi::get_one::<bool>("SELECT count(*)=2 AND bool_and(after_present) FROM syntra.capture_events WHERE source_relation='public.capture_permissions'::regclass").unwrap(),Some(true));
    assert_eq!(Spi::get_one::<bool>("SELECT NOT has_table_privilege('capture_writer','syntra.capture_heads','SELECT') AND NOT has_table_privilege('capture_writer','syntra.capture_events','INSERT') AND NOT has_table_privilege('capture_writer','syntra.capture_base_rows','SELECT')").unwrap(),Some(true));
}

#[pg_test(error = "capture requires a single nondeferrable int4 or int8 primary key")]
fn capture_rejects_sources_without_a_stable_single_key() {
    capture_fixture("CREATE TABLE capture_no_key(id int4,v int4); SELECT syntra_seed_capture('capture_no_key','id','v',4)");
}

#[pg_test(error = "capture seeding currently requires a superuser")]
fn capture_checks_superuser_after_an_accidental_execute_grant() {
    capture_fixture("CREATE ROLE capture_denied NOLOGIN; CREATE TABLE public.capture_private(id int4 PRIMARY KEY,v int4); GRANT SELECT ON public.capture_private TO capture_denied; GRANT EXECUTE ON FUNCTION syntra_seed_capture(regclass,text,text,int4) TO capture_denied; SET LOCAL ROLE capture_denied; SELECT syntra_seed_capture('public.capture_private','id','v',4)");
}

#[pg_test(error = "TRUNCATE is not supported while capture is installed")]
fn capture_rejects_truncate_instead_of_omitting_its_changes() {
    capture_fixture("CREATE TABLE capture_truncate(id int4 PRIMARY KEY,v int4); SELECT syntra_seed_capture('capture_truncate','id','v',4); TRUNCATE capture_truncate");
}

#[pg_test(error = "capture configuration is missing or changed; reseed before writing")]
fn capture_rejects_a_disable_write_enable_gap() {
    capture_fixture("CREATE TABLE capture_gap(id int4 PRIMARY KEY,v int4); INSERT INTO capture_gap(id,v) VALUES(1,1); SELECT syntra_seed_capture('capture_gap','id','v',8); ALTER TABLE capture_gap DISABLE TRIGGER _syntra_capture_row; UPDATE capture_gap SET v=2 WHERE id=1; ALTER TABLE capture_gap ENABLE ALWAYS TRIGGER _syntra_capture_row; UPDATE capture_gap SET v=3 WHERE id=1");
}

#[pg_test]
fn capture_source_drop_cleans_base_and_log_when_foreign_key_actions_are_disabled() {
    capture_fixture("CREATE TABLE capture_drop(id int4 PRIMARY KEY,v int4); SELECT syntra_seed_capture('capture_drop','id','v',4); INSERT INTO capture_drop(id,v) VALUES(1,1); SET LOCAL session_replication_role=replica; DROP TABLE capture_drop");
    assert_eq!(Spi::get_one::<bool>("SELECT (SELECT count(*) FROM syntra.capture_heads)=0 AND (SELECT count(*) FROM syntra.capture_base_rows)=0 AND (SELECT count(*) FROM syntra.capture_events)=0").unwrap(),Some(true));
}

#[pg_test(error = "capture seed exceeds 100000 rows")]
fn capture_seed_rejects_an_oversized_base() {
    capture_fixture("CREATE TABLE capture_large(id int4 PRIMARY KEY,v int4); INSERT INTO capture_large(id,v) SELECT n,n FROM generate_series(1,100001) AS n; SELECT syntra_seed_capture('capture_large','id','v',4)");
}

#[pg_test(error = "capture event limit must be between 1 and 4096")]
fn capture_rejects_an_unbounded_event_limit() {
    capture_fixture("CREATE TABLE capture_limit(id int4 PRIMARY KEY,v int4); SELECT syntra_seed_capture('capture_limit','id','v',4097)");
}

#[pg_test(
    error = "cannot seed capture \"capture_active\" because it is being used by active queries in this session"
)]
fn capture_cannot_install_mid_statement_and_miss_remaining_rows() {
    capture_fixture("CREATE TABLE capture_active(id int4 PRIMARY KEY,v int4); INSERT INTO capture_active(id,v) VALUES(1,1); UPDATE capture_active SET v=syntra_seed_capture('capture_active','id','v',8)::int4 WHERE id=1");
}

#[pg_test(error = "capture limit of 16 sources reached")]
fn capture_bounds_the_number_of_live_sources() {
    capture_fixture("CREATE TABLE capture_slots(id int4 PRIMARY KEY,v int4); SELECT syntra_seed_capture('capture_slots','id','v',4)");
    // Private admin fixture fills inactive slots with OIDs outside this test DB's
    // assigned range. Public roles cannot insert authoritative registrations.
    Spi::run("INSERT INTO syntra.tracked_sources(source_relation,trigger_oid) SELECT (3000000000::bigint+n)::oid,(3100000000::bigint+n)::oid FROM generate_series(1,15) AS n; INSERT INTO syntra.capture_heads(source_relation,tracking_epoch,key_attribute,value_attribute,row_trigger,truncate_trigger,max_events,source_identity,base_snapshot,base_rows) SELECT s.source_relation,s.epoch,1,2,s.trigger_oid,s.trigger_oid,4,'fixture','1:2:',0 FROM syntra.tracked_sources AS s WHERE s.source_relation BETWEEN 3000000001::oid AND 3000000015::oid").unwrap();
    assert_eq!(
        Spi::get_one::<i64>("SELECT count(*) FROM syntra.capture_heads").unwrap(),
        Some(16)
    );
    Spi::run("CREATE TABLE capture_no_slot(id int4 PRIMARY KEY,v int4); SELECT syntra_seed_capture('capture_no_slot','id','v',4)").unwrap();
}

#[pg_test]
fn capture_retirement_preserves_source_and_statement_tracking() {
    capture_fixture("CREATE TABLE capture_retire(id int4 PRIMARY KEY,v int4); INSERT INTO capture_retire(id,v) VALUES(1,1); SELECT syntra_seed_capture('capture_retire','id','v',1); UPDATE capture_retire SET v=2 WHERE id=1");
    Spi::run("DO $$ BEGIN PERFORM syntra_stop_capture('capture_retire'); RAISE EXCEPTION 'rollback retirement'; EXCEPTION WHEN raise_exception THEN NULL; END $$; DO $$ BEGIN UPDATE capture_retire SET v=99 WHERE id=1; EXCEPTION WHEN program_limit_exceeded THEN NULL; END $$").unwrap();
    assert_eq!(
        Spi::get_one::<i32>("SELECT v FROM capture_retire LIMIT 1").unwrap(),
        Some(2)
    );
    assert_eq!(Spi::get_one::<i64>("SELECT emitted FROM capture_position WHERE source_relation='capture_retire'::regclass LIMIT 1").unwrap(),Some(1));
    assert_eq!(
        Spi::get_one::<bool>("SELECT syntra_stop_capture('capture_retire')").unwrap(),
        Some(true)
    );
    assert_eq!(
        Spi::get_one::<bool>("SELECT syntra_stop_capture('capture_retire')").unwrap(),
        Some(false)
    );
    Spi::run("UPDATE capture_retire SET v=3 WHERE id=1").unwrap();
    assert_eq!(
        Spi::get_one::<i32>("SELECT v FROM capture_retire LIMIT 1").unwrap(),
        Some(3)
    );
    // Since 0.5.0 the captured update leaves the counter rows alone; only the
    // update after retirement counts.
    assert_eq!(Spi::get_one::<bool>("SELECT guard_config_valid AND generation=1 AND NOT EXISTS(SELECT epoch FROM syntra.capture_heads WHERE source_relation='capture_retire'::regclass) AND NOT EXISTS(SELECT ordinal FROM syntra.capture_events WHERE source_relation='capture_retire'::regclass) AND NOT EXISTS(SELECT key FROM syntra.capture_base_rows WHERE source_relation='capture_retire'::regclass) FROM syntra_source_tracking_status('capture_retire') LIMIT 1").unwrap(),Some(true));
    Spi::run("TRUNCATE capture_retire").unwrap();
    assert_eq!(
        Spi::get_one::<i64>("SELECT count(*) FROM capture_retire").unwrap(),
        Some(0)
    );
}

#[pg_test(error = "capture retirement currently requires a superuser")]
fn capture_retirement_checks_superuser_after_an_execute_grant() {
    capture_fixture("CREATE ROLE capture_retire_denied NOLOGIN; CREATE TABLE public.capture_retire_private(id int4 PRIMARY KEY,v int4); GRANT EXECUTE ON FUNCTION syntra_stop_capture(regclass) TO capture_retire_denied; SET LOCAL ROLE capture_retire_denied; SELECT syntra_stop_capture('public.capture_retire_private')");
}

#[pg_test(error = "capture configuration is missing or changed; reseed before writing")]
fn capture_rejects_a_source_that_gains_inheritance_descendants() {
    capture_fixture("CREATE TABLE capture_parent(id int4 PRIMARY KEY,v int4); SELECT syntra_seed_capture('capture_parent','id','v',8); CREATE TABLE capture_child() INHERITS(capture_parent); INSERT INTO capture_parent(id,v) VALUES(1,1)");
}

#[pg_test(error = "capture does not support other source triggers or foreign-key triggers")]
fn capture_rejects_nested_after_row_writes_with_ambiguous_callback_order() {
    capture_fixture(
        r#"CREATE TABLE capture_nested(id int4 PRIMARY KEY,v int4);
        CREATE FUNCTION capture_nested_write() RETURNS trigger LANGUAGE plpgsql AS $$
        BEGIN IF pg_trigger_depth()=1 THEN UPDATE capture_nested SET v=v+1 WHERE id=NEW.id; END IF; RETURN NEW; END $$;
        CREATE TRIGGER "!before_capture" AFTER UPDATE ON capture_nested FOR EACH ROW EXECUTE FUNCTION capture_nested_write();
        SELECT syntra_seed_capture('capture_nested','id','v',8)"#,
    );
}

#[pg_test(error = "capture does not support other source triggers or foreign-key triggers")]
fn capture_rejects_foreign_key_trigger_dependencies() {
    capture_fixture("CREATE TABLE capture_referenced(id int4 PRIMARY KEY); CREATE TABLE capture_foreign(id int4 PRIMARY KEY,v int4 REFERENCES capture_referenced(id)); SELECT syntra_seed_capture('capture_foreign','id','v',8)");
}

#[pg_test]
fn capture_headroom_counts_committed_events_down_to_zero_and_recovers_after_prune() {
    capture_fixture("CREATE TABLE headroom_values(id int4 PRIMARY KEY,v int4); INSERT INTO headroom_values VALUES(1,1); SELECT syntra_seed_capture('headroom_values','id','v',4)");
    let room = || {
        Spi::get_one::<String>("SELECT concat_ws(',',retained_events,max_events,free_events,live_rows) FROM syntra_capture_headroom('headroom_values')")
            .unwrap()
            .unwrap()
    };
    assert_eq!(room(), "0,4,4,1");
    Spi::run("UPDATE headroom_values SET v=2; UPDATE headroom_values SET v=3; UPDATE headroom_values SET v=4").unwrap();
    assert_eq!(room(), "3,4,1,1");
    Spi::run("UPDATE headroom_values SET v=5").unwrap();
    assert_eq!(room(), "4,4,0,1");
    Spi::get_one::<i64>("SELECT syntra_apply_capture('headroom_values')").unwrap();
    Spi::get_one::<i64>("SELECT syntra_prune_capture('headroom_values')").unwrap();
    assert_eq!(room(), "0,4,4,1");
}

#[pg_test]
fn capture_headroom_is_open_to_a_write_only_role() {
    capture_fixture("CREATE TABLE headroom_acl(id int4 PRIMARY KEY,v int4); SELECT syntra_seed_capture('headroom_acl','id','v',4); CREATE ROLE headroom_writer; GRANT INSERT ON headroom_acl TO headroom_writer");
    Spi::run("SET LOCAL ROLE headroom_writer").unwrap();
    assert_eq!(
        Spi::get_one::<i64>("SELECT free_events FROM syntra_capture_headroom('headroom_acl')")
            .unwrap(),
        Some(4)
    );
}

#[pg_test(error = "capture headroom requires schema USAGE and a table privilege")]
fn capture_headroom_rejects_a_role_without_table_privileges() {
    capture_fixture("CREATE TABLE headroom_closed(id int4 PRIMARY KEY,v int4); SELECT syntra_seed_capture('headroom_closed','id','v',4); CREATE ROLE headroom_stranger");
    Spi::run("SET LOCAL ROLE headroom_stranger").unwrap();
    Spi::get_one::<i64>("SELECT free_events FROM syntra_capture_headroom('headroom_closed')")
        .unwrap();
}

#[pg_test]
fn capture_binding_copies_the_head_configuration_and_leaves_with_the_feed() {
    capture_fixture("CREATE TABLE binding_values(id int4 PRIMARY KEY,v int4); INSERT INTO binding_values VALUES(1,1); SELECT syntra_seed_capture('binding_values','id','v',4)");
    assert_eq!(
        Spi::get_one::<bool>("SELECT (b.epoch,b.tracking_epoch,b.key_attribute,b.value_attribute,b.row_trigger,b.truncate_trigger,b.source_identity)=(h.epoch,h.tracking_epoch,h.key_attribute,h.value_attribute,h.row_trigger,h.truncate_trigger,h.source_identity) FROM syntra.capture_bindings AS b JOIN syntra.capture_heads AS h USING(source_relation) WHERE source_relation='binding_values'::regclass")
            .unwrap(),
        Some(true)
    );
    Spi::run("UPDATE binding_values SET v=2").unwrap();
    Spi::run("SELECT syntra_stop_capture('binding_values')").unwrap();
    assert_eq!(
        Spi::get_one::<i64>("SELECT count(*) FROM syntra.capture_bindings WHERE source_relation='binding_values'::regclass")
            .unwrap(),
        Some(0)
    );
}

#[pg_test(error = "capture configuration is missing or changed; reseed before writing")]
fn capture_writer_rejects_a_binding_that_no_longer_matches_its_trigger() {
    capture_fixture("CREATE TABLE binding_tampered(id int4 PRIMARY KEY,v int4); INSERT INTO binding_tampered VALUES(1,1); SELECT syntra_seed_capture('binding_tampered','id','v',4)");
    Spi::run("UPDATE syntra.capture_bindings SET row_trigger=0 WHERE source_relation='binding_tampered'::regclass").unwrap();
    Spi::run("UPDATE binding_tampered SET v=2").unwrap();
}

#[pg_test]
fn capture_writes_append_marks_and_leave_the_head_row_unversioned() {
    capture_fixture("CREATE TABLE mark_values(id int4 PRIMARY KEY,v int4); INSERT INTO mark_values VALUES(1,1),(2,2); SELECT syntra_seed_capture('mark_values','id','v',16)");
    let head =
        "SELECT ctid::text FROM syntra.capture_heads WHERE source_relation='mark_values'::regclass";
    let before = Spi::get_one::<String>(head).unwrap();
    Spi::run("UPDATE mark_values SET v=v+1; UPDATE mark_values SET id=3 WHERE id=1; INSERT INTO mark_values VALUES(4,4); DELETE FROM mark_values WHERE id=2; DO $$ BEGIN UPDATE mark_values SET v=100 WHERE id=3; RAISE EXCEPTION 'undo'; EXCEPTION WHEN raise_exception THEN NULL; END $$; UPDATE mark_values SET v=9 WHERE id=4").unwrap();
    assert_eq!(Spi::get_one::<String>(head).unwrap(), before);
    assert_eq!(
        Spi::get_one::<String>("SELECT string_agg(ordinal||':'||live_rows,',' ORDER BY ordinal) FROM syntra.capture_marks WHERE source_relation='mark_values'::regclass")
            .unwrap()
            .as_deref(),
        Some("1:2,2:2,4:2,5:3,6:2,7:2")
    );
    assert_eq!(
        Spi::get_one::<String>("SELECT concat_ws(',',(SELECT count(*) FROM syntra.capture_events WHERE source_relation='mark_values'::regclass),p.emitted,p.live_rows,r.retained_events,r.live_rows) FROM capture_position AS p,syntra_capture_headroom('mark_values') AS r WHERE p.source_relation='mark_values'::regclass")
            .unwrap()
            .as_deref(),
        Some("7,7,2,7,2")
    );
}

#[pg_test]
fn capture_row_trigger_is_a_deferred_constraint_trigger() {
    capture_fixture("CREATE TABLE capture_shape(id int4 PRIMARY KEY,v int4); SELECT syntra_seed_capture('capture_shape','id','v',8)");
    assert_eq!(Spi::get_one::<bool>("SELECT t.tgdeferrable AND t.tginitdeferred AND c.contype='t' AND c.conrelid=t.tgrelid AND c.condeferrable AND c.condeferred FROM pg_trigger AS t JOIN pg_constraint AS c ON c.oid=t.tgconstraint WHERE t.tgrelid='capture_shape'::regclass AND t.tgname='_syntra_capture_row'").unwrap(),Some(true));
    assert_eq!(Spi::get_one::<bool>("SELECT NOT tgdeferrable AND tgconstraint=0 FROM pg_trigger WHERE tgrelid='capture_shape'::regclass AND tgname='_syntra_capture_no_truncate'").unwrap(),Some(true));
}

#[pg_test]
fn deferred_capture_writes_events_in_change_order_only_when_fired() {
    capture_fixture("CREATE TABLE capture_deferred(id int4 PRIMARY KEY,v int4); INSERT INTO capture_deferred(id,v) VALUES(1,10),(2,20); SELECT syntra_seed_capture('capture_deferred','id','v',16); SET CONSTRAINTS ALL DEFERRED");
    Spi::run("UPDATE capture_deferred SET v=11 WHERE id=1; UPDATE capture_deferred SET v=12 WHERE id=1; UPDATE capture_deferred SET id=3 WHERE id=2").unwrap();
    // The trigger is the only code that locks the head row, so no events means
    // no lock was taken yet.
    assert_eq!(Spi::get_one::<i64>("SELECT count(*) FROM syntra.capture_events WHERE source_relation='capture_deferred'::regclass").unwrap(),Some(0));
    // A rolled-back subtransaction's queued firings leave with it.
    Spi::run("DO $$ BEGIN DELETE FROM capture_deferred WHERE id=1; RAISE EXCEPTION 'discard'; EXCEPTION WHEN raise_exception THEN NULL; END $$").unwrap();
    Spi::run("SET CONSTRAINTS ALL IMMEDIATE").unwrap();
    assert_eq!(Spi::get_one::<bool>("SELECT array_agg(key ORDER BY ordinal)=ARRAY[1,1,2,3]::int8[] AND array_agg(before_value ORDER BY ordinal)=ARRAY[10,11,20,NULL]::int8[] AND array_agg(after_value ORDER BY ordinal)=ARRAY[11,12,NULL,20]::int8[] AND bool_and(origin_xid=pg_current_xact_id()) FROM syntra.capture_events WHERE source_relation='capture_deferred'::regclass").unwrap(),Some(true));
    assert_eq!(Spi::get_one::<bool>("SELECT emitted=4 AND live_rows=2 FROM capture_position WHERE source_relation='capture_deferred'::regclass").unwrap(),Some(true));
}

#[pg_test(
    error = "capture log is full or registration changed; apply and prune or reseed before writing"
)]
fn deferred_capture_refuses_a_full_log_when_it_fires() {
    capture_fixture("CREATE TABLE capture_deferred_full(id int4 PRIMARY KEY,v int4); INSERT INTO capture_deferred_full(id,v) VALUES(1,10); SELECT syntra_seed_capture('capture_deferred_full','id','v',2); SET CONSTRAINTS ALL DEFERRED");
    Spi::run("UPDATE capture_deferred_full SET v=v+1; UPDATE capture_deferred_full SET v=v+1; UPDATE capture_deferred_full SET v=v+1").unwrap();
    Spi::run("SET CONSTRAINTS ALL IMMEDIATE").unwrap();
}

#[pg_test]
fn capture_keeps_an_immediate_row_trigger_from_before_the_upgrade() {
    // 0.5.0 installed this trigger; it fires per statement even when deferral is asked for.
    capture_fixture("CREATE TABLE capture_immediate(id int4 PRIMARY KEY,v int4); CREATE TRIGGER _syntra_capture_row AFTER INSERT OR UPDATE OR DELETE ON capture_immediate FOR EACH ROW EXECUTE FUNCTION public.syntra_capture_row(); ALTER TABLE capture_immediate ENABLE ALWAYS TRIGGER _syntra_capture_row; SELECT syntra_seed_capture('capture_immediate','id','v',8); SET CONSTRAINTS ALL DEFERRED");
    assert_eq!(Spi::get_one::<bool>("SELECT h.row_trigger=t.oid AND NOT t.tgdeferrable FROM syntra.capture_heads AS h JOIN pg_trigger AS t ON t.tgrelid=h.source_relation AND t.tgname='_syntra_capture_row' WHERE h.source_relation='capture_immediate'::regclass").unwrap(),Some(true));
    Spi::run("INSERT INTO capture_immediate(id,v) VALUES(1,5)").unwrap();
    assert_eq!(Spi::get_one::<bool>("SELECT count(*)=1 AND bool_and(after_value=5) FROM syntra.capture_events WHERE source_relation='capture_immediate'::regclass").unwrap(),Some(true));
}

#[pg_test(error = "existing capture trigger is incompatible; repair it before seeding")]
fn capture_refuses_a_deferrable_row_trigger_that_starts_immediate() {
    capture_fixture("CREATE TABLE capture_half_deferred(id int4 PRIMARY KEY,v int4); CREATE CONSTRAINT TRIGGER _syntra_capture_row AFTER INSERT OR UPDATE OR DELETE ON capture_half_deferred DEFERRABLE INITIALLY IMMEDIATE FOR EACH ROW EXECUTE FUNCTION public.syntra_capture_row(); ALTER TABLE capture_half_deferred ENABLE ALWAYS TRIGGER _syntra_capture_row");
    Spi::run("SELECT syntra_seed_capture('capture_half_deferred','id','v',8)").unwrap();
}
