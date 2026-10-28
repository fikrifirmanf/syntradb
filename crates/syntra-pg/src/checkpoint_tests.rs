#[pg_test]
fn checkpoint_applies_key_moves_nulls_and_retries_without_advancing_twice() {
    capture_fixture("CREATE TABLE checkpoint_values(id int4 PRIMARY KEY,v int4); INSERT INTO checkpoint_values(id,v) VALUES(1,10),(2,NULL); SELECT syntra_seed_capture('checkpoint_values','id','v',4)");
    assert_eq!(
        Spi::get_one::<i64>("SELECT syntra_apply_capture('checkpoint_values')").unwrap(),
        Some(0)
    );
    Spi::run("UPDATE checkpoint_values SET id=3,v=11 WHERE id=1; INSERT INTO checkpoint_values(id,v) VALUES(4,NULL); DELETE FROM checkpoint_values WHERE id=2").unwrap();
    assert_eq!(
        Spi::get_one::<i64>("SELECT syntra_apply_capture('checkpoint_values')").unwrap(),
        Some(4)
    );
    assert_eq!(
        Spi::get_one::<i64>("SELECT syntra_apply_capture('checkpoint_values')").unwrap(),
        Some(4)
    );
    assert_eq!(Spi::get_one::<bool>("SELECT generation=2 AND applied=4 FROM syntra.capture_checkpoints WHERE source_relation='checkpoint_values'::regclass").unwrap(),Some(true));
    // Base segments plus partition deltas; a base segment alone may be older.
    assert_eq!(
        tail_snapshot("checkpoint_values"),
        Ok(((2, 1, Some(11), Some(11), Some(11)), false))
    );
    assert_eq!(
        Spi::get_one::<i64>("SELECT syntra_prune_capture('checkpoint_values')").unwrap(),
        Some(4)
    );
    Spi::run("UPDATE checkpoint_values SET v=12 WHERE id=3").unwrap();
    assert_eq!(
        Spi::get_one::<i64>("SELECT syntra_apply_capture('checkpoint_values')").unwrap(),
        Some(5)
    );
    assert_eq!(Spi::get_one::<bool>("SELECT h.emitted=5 AND h.pruned=4 AND h.live_rows=2 AND c.applied=5 FROM capture_position AS h JOIN syntra.capture_checkpoints AS c USING(source_relation,epoch) WHERE h.source_relation='checkpoint_values'::regclass").unwrap(),Some(true));
    assert_eq!(
        tail_snapshot("checkpoint_values"),
        Ok(((2, 1, Some(12), Some(12), Some(12)), false))
    );
}

#[pg_test]
fn checkpoint_apply_and_prune_roll_back_with_their_progress_and_rows() {
    capture_fixture("CREATE TABLE checkpoint_abort(id int4 PRIMARY KEY,v int4); INSERT INTO checkpoint_abort(id,v) VALUES(1,10); SELECT syntra_seed_capture('checkpoint_abort','id','v',4); SELECT syntra_apply_capture('checkpoint_abort'); UPDATE checkpoint_abort SET v=11 WHERE id=1");
    Spi::run("DO $$ BEGIN PERFORM syntra_apply_capture('checkpoint_abort'); PERFORM syntra_prune_capture('checkpoint_abort'); RAISE EXCEPTION 'abort apply'; EXCEPTION WHEN raise_exception THEN NULL; END $$").unwrap();
    assert_eq!(Spi::get_one::<bool>("SELECT c.applied=0 AND c.generation=1 AND s.sum=10 AND h.emitted=1 AND h.pruned=0 AND (SELECT count(*) FROM syntra.capture_events WHERE source_relation=h.source_relation)=1 FROM syntra.capture_checkpoints AS c JOIN capture_position AS h USING(source_relation,epoch),LATERAL (SELECT sum(t.rows)::bigint AS rows,sum(t.non_nulls)::bigint AS non_nulls,sum(t.sum)::bigint AS sum,min(t.min) AS min,max(t.max) AS max FROM syntra.capture_chunks AS k,LATERAL syntra_segment_summary(k.segment) AS t WHERE k.source_relation=c.source_relation) AS s WHERE c.source_relation='checkpoint_abort'::regclass LIMIT 1").unwrap(),Some(true));
    assert_eq!(
        Spi::get_one::<i64>("SELECT syntra_apply_capture('checkpoint_abort')").unwrap(),
        Some(1)
    );
}

#[pg_test(error = "checkpoint_history_gap")]
fn checkpoint_rejects_missing_events_instead_of_skipping_progress() {
    capture_fixture("CREATE TABLE checkpoint_gap(id int4 PRIMARY KEY,v int4); INSERT INTO checkpoint_gap(id,v) VALUES(1,10); SELECT syntra_seed_capture('checkpoint_gap','id','v',4); SELECT syntra_apply_capture('checkpoint_gap'); UPDATE checkpoint_gap SET v=11 WHERE id=1; DELETE FROM syntra.capture_events WHERE source_relation='checkpoint_gap'::regclass; SELECT syntra_apply_capture('checkpoint_gap')");
}

#[pg_test(error = "checkpoint replay rejected the captured event prefix")]
fn checkpoint_rejects_incorrect_before_values() {
    capture_fixture("CREATE TABLE checkpoint_before(id int4 PRIMARY KEY,v int4); INSERT INTO checkpoint_before(id,v) VALUES(1,10); SELECT syntra_seed_capture('checkpoint_before','id','v',4); SELECT syntra_apply_capture('checkpoint_before'); UPDATE checkpoint_before SET v=11 WHERE id=1; UPDATE syntra.capture_events SET before_value=9 WHERE source_relation='checkpoint_before'::regclass; SELECT syntra_apply_capture('checkpoint_before')");
}

#[pg_test(error = "invalid_checkpoint")]
fn checkpoint_rejects_corrupt_keyed_state_before_pruning() {
    capture_fixture("CREATE TABLE checkpoint_damage(id int4 PRIMARY KEY,v int4); INSERT INTO checkpoint_damage(id,v) VALUES(1,10); SELECT syntra_seed_capture('checkpoint_damage','id','v',4); SELECT syntra_apply_capture('checkpoint_damage'); UPDATE syntra.capture_chunks SET state=set_byte(state,112,get_byte(state,112) # 1) WHERE source_relation='checkpoint_damage'::regclass AND row_count>0; SELECT syntra_prune_capture('checkpoint_damage')");
}

#[pg_test(error = "checkpoint maintenance requires a superuser")]
fn checkpoint_apply_checks_superuser_after_an_execute_grant() {
    capture_fixture("CREATE ROLE checkpoint_denied NOLOGIN; CREATE TABLE public.checkpoint_private(id int4 PRIMARY KEY,v int4); GRANT EXECUTE ON FUNCTION syntra_apply_capture(regclass) TO checkpoint_denied; SET LOCAL ROLE checkpoint_denied; SELECT syntra_apply_capture('public.checkpoint_private')");
}

#[pg_test]
fn checkpoint_retirement_reseed_and_replica_drop_clear_all_artifacts() {
    capture_fixture("CREATE TABLE checkpoint_clear(id int4 PRIMARY KEY,v int4); INSERT INTO checkpoint_clear(id,v) VALUES(1,1); SELECT syntra_seed_capture('checkpoint_clear','id','v',4); SELECT syntra_apply_capture('checkpoint_clear'); SELECT syntra_seed_capture('checkpoint_clear','id','v',4)");
    assert_eq!(
        Spi::get_one::<i64>("SELECT (SELECT count(*) FROM syntra.capture_checkpoints)+(SELECT count(*) FROM syntra.capture_chunks)").unwrap(),
        Some(0)
    );
    Spi::run("SELECT syntra_apply_capture('checkpoint_clear'); SELECT syntra_stop_capture('checkpoint_clear')").unwrap();
    assert_eq!(
        Spi::get_one::<i64>("SELECT (SELECT count(*) FROM syntra.capture_checkpoints)+(SELECT count(*) FROM syntra.capture_chunks)").unwrap(),
        Some(0)
    );
    Spi::run("SELECT syntra_seed_capture('checkpoint_clear','id','v',4); SELECT syntra_apply_capture('checkpoint_clear'); SET LOCAL session_replication_role=replica; DROP TABLE checkpoint_clear").unwrap();
    assert_eq!(
        Spi::get_one::<i64>("SELECT (SELECT count(*) FROM syntra.capture_checkpoints)+(SELECT count(*) FROM syntra.capture_chunks)").unwrap(),
        Some(0)
    );
}

#[pg_test]
fn checkpoint_pruning_keeps_live_storage_bounded_after_ordinal_4096() {
    capture_fixture("CREATE TABLE checkpoint_cycles(id int4 PRIMARY KEY,v int4); INSERT INTO checkpoint_cycles(id,v) VALUES(1,0); SELECT syntra_seed_capture('checkpoint_cycles','id','v',64); SELECT syntra_apply_capture('checkpoint_cycles')");
    Spi::run("DO $$ BEGIN FOR n IN 1..4100 LOOP UPDATE checkpoint_cycles SET v=v+1 WHERE id=1; IF n%32=0 THEN PERFORM syntra_apply_capture('checkpoint_cycles'); PERFORM syntra_prune_capture('checkpoint_cycles'); END IF; END LOOP; END $$").unwrap();
    assert_eq!(Spi::get_one::<bool>("SELECT emitted=4100 AND pruned=4096 AND live_rows=1 AND (SELECT count(*) FROM syntra.capture_events WHERE source_relation='checkpoint_cycles'::regclass)=4 AND (SELECT count(*) FROM syntra.capture_marks WHERE source_relation='checkpoint_cycles'::regclass)=4 AND (SELECT emitted=4096 FROM syntra.capture_heads WHERE source_relation='checkpoint_cycles'::regclass) FROM capture_position WHERE source_relation='checkpoint_cycles'::regclass LIMIT 1").unwrap(),Some(true));
    assert_eq!(
        Spi::get_one::<i64>("SELECT syntra_apply_capture('checkpoint_cycles')").unwrap(),
        Some(4100)
    );
    assert_eq!(
        Spi::get_one::<i64>("SELECT syntra_prune_capture('checkpoint_cycles')").unwrap(),
        Some(4)
    );
}

#[pg_test]
fn checkpoint_pruning_preserves_events_newer_than_the_applied_prefix() {
    capture_fixture("CREATE TABLE checkpoint_prefix(id int4 PRIMARY KEY,v int4); SELECT syntra_seed_capture('checkpoint_prefix','id','v',4); SELECT syntra_apply_capture('checkpoint_prefix')");
    assert_eq!(Spi::get_one::<bool>("SELECT s.rows=0 AND s.non_nulls=0 AND s.sum IS NULL AND s.min IS NULL AND s.max IS NULL FROM syntra.capture_checkpoints AS c,LATERAL (SELECT sum(t.rows)::bigint AS rows,sum(t.non_nulls)::bigint AS non_nulls,sum(t.sum)::bigint AS sum,min(t.min) AS min,max(t.max) AS max FROM syntra.capture_chunks AS k,LATERAL syntra_segment_summary(k.segment) AS t WHERE k.source_relation=c.source_relation) AS s WHERE c.source_relation='checkpoint_prefix'::regclass LIMIT 1").unwrap(),Some(true));
    Spi::run("INSERT INTO checkpoint_prefix VALUES(1,NULL); SELECT syntra_apply_capture('checkpoint_prefix'); UPDATE checkpoint_prefix SET v=12 WHERE id=1").unwrap();
    assert_eq!(
        Spi::get_one::<i64>("SELECT syntra_prune_capture('checkpoint_prefix')").unwrap(),
        Some(1)
    );
    assert_eq!(Spi::get_one::<bool>("SELECT h.emitted=2 AND h.pruned=1 AND c.applied=1 AND (SELECT count(*)=1 AND min(ordinal)=2 FROM syntra.capture_events WHERE source_relation=h.source_relation) FROM capture_position AS h JOIN syntra.capture_checkpoints AS c USING(source_relation,epoch) WHERE h.source_relation='checkpoint_prefix'::regclass LIMIT 1").unwrap(),Some(true));
    assert_eq!(
        Spi::get_one::<i64>("SELECT syntra_apply_capture('checkpoint_prefix')").unwrap(),
        Some(2)
    );
    assert_eq!(
        tail_snapshot("checkpoint_prefix"),
        Ok(((1, 1, Some(12), Some(12), Some(12)), false))
    );
}

#[pg_test]
fn chunked_apply_rewrites_only_the_changed_partition() {
    capture_fixture("CREATE TABLE checkpoint_dirty(id int4 PRIMARY KEY,v int4); INSERT INTO checkpoint_dirty SELECT n,n FROM generate_series(1,1000) AS n; SELECT syntra_seed_capture('checkpoint_dirty','id','v',8); SELECT syntra_apply_capture('checkpoint_dirty'); CREATE TEMP TABLE prior_chunks AS SELECT partition,ctid::text AS tuple,revision,state,segment FROM syntra.capture_chunks WHERE source_relation='checkpoint_dirty'::regclass");
    Spi::run("UPDATE checkpoint_dirty SET v=44 WHERE id=1; SELECT syntra_apply_capture('checkpoint_dirty')").unwrap();
    assert_eq!(Spi::get_one::<bool>("SELECT count(*)=64 AND count(*) FILTER(WHERE c.ctid::text<>p.tuple)=1 AND bool_and(CASE WHEN c.partition=55 THEN c.revision=1 ELSE c.revision=0 AND c.state=p.state AND c.segment=p.segment END) FROM syntra.capture_chunks AS c JOIN prior_chunks AS p USING(partition) WHERE c.source_relation='checkpoint_dirty'::regclass").unwrap(),Some(true));
    let bytes=Spi::get_one::<Vec<u8>>("SELECT manifest FROM syntra.capture_checkpoints WHERE source_relation='checkpoint_dirty'::regclass LIMIT 1").unwrap().unwrap();
    let manifest = syntra_storage::chunks::Manifest::decode(&bytes).unwrap();
    assert_eq!(
        (
            manifest.applied,
            manifest.generation,
            manifest.rows().unwrap()
        ),
        (1, 2, 1000)
    );
    assert_eq!(manifest.chunks[55].revision, 1);
}

#[pg_test]
fn chunked_publication_rolls_back_chunks_when_manifest_update_fails() {
    capture_fixture("CREATE TABLE checkpoint_publish_abort(id int4 PRIMARY KEY,v int4); INSERT INTO checkpoint_publish_abort VALUES(1,10); SELECT syntra_seed_capture('checkpoint_publish_abort','id','v',8); SELECT syntra_apply_capture('checkpoint_publish_abort'); CREATE TEMP TABLE prior_abort_chunks AS SELECT partition,state,segment,revision FROM syntra.capture_chunks WHERE source_relation='checkpoint_publish_abort'::regclass; UPDATE checkpoint_publish_abort SET id=2,v=11 WHERE id=1; CREATE FUNCTION reject_checkpoint_manifest() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected manifest rejection'; END $$; CREATE TRIGGER reject_manifest BEFORE UPDATE ON syntra.capture_checkpoints FOR EACH ROW EXECUTE FUNCTION reject_checkpoint_manifest()");
    Spi::run("DO $$ BEGIN PERFORM syntra_apply_capture('checkpoint_publish_abort'); RAISE EXCEPTION 'apply unexpectedly succeeded'; EXCEPTION WHEN raise_exception THEN IF SQLERRM <> 'injected manifest rejection' THEN RAISE; END IF; END $$").unwrap();
    assert_eq!(Spi::get_one::<bool>("SELECT count(*)=64 AND bool_and(c.state=p.state AND c.segment=p.segment AND c.revision=p.revision) FROM syntra.capture_chunks AS c JOIN prior_abort_chunks AS p USING(partition) WHERE c.source_relation='checkpoint_publish_abort'::regclass").unwrap(),Some(true));
    assert_eq!(Spi::get_one::<bool>("SELECT applied=0 AND generation=1 FROM syntra.capture_checkpoints WHERE source_relation='checkpoint_publish_abort'::regclass LIMIT 1").unwrap(),Some(true));
    Spi::run("DROP TRIGGER reject_manifest ON syntra.capture_checkpoints").unwrap();
    assert_eq!(
        Spi::get_one::<i64>("SELECT syntra_apply_capture('checkpoint_publish_abort')").unwrap(),
        Some(2)
    );
    assert_eq!(Spi::get_one::<bool>("SELECT count(*)=2 AND min(partition)=6 AND max(partition)=55 FROM syntra.capture_chunks WHERE source_relation='checkpoint_publish_abort'::regclass AND revision=1").unwrap(),Some(true));
}

#[pg_test(error = "invalid_checkpoint")]
fn chunked_pruning_rejects_a_missing_empty_partition() {
    capture_fixture("CREATE TABLE checkpoint_missing_chunk(id int4 PRIMARY KEY,v int4); SELECT syntra_seed_capture('checkpoint_missing_chunk','id','v',8); SELECT syntra_apply_capture('checkpoint_missing_chunk'); DELETE FROM syntra.capture_chunks WHERE source_relation='checkpoint_missing_chunk'::regclass AND partition=63; SELECT syntra_prune_capture('checkpoint_missing_chunk')");
}

#[pg_test(error = "invalid_checkpoint")]
fn chunked_pruning_rejects_a_revision_outside_the_manifest() {
    capture_fixture("CREATE TABLE checkpoint_bad_revision(id int4 PRIMARY KEY,v int4); SELECT syntra_seed_capture('checkpoint_bad_revision','id','v',8); SELECT syntra_apply_capture('checkpoint_bad_revision'); UPDATE syntra.capture_chunks SET revision=1 WHERE source_relation='checkpoint_bad_revision'::regclass AND partition=63; SELECT syntra_prune_capture('checkpoint_bad_revision')");
}

#[pg_test(error = "invalid_checkpoint")]
fn chunked_pruning_validates_untouched_bytes_after_incremental_apply() {
    capture_fixture("CREATE TABLE checkpoint_unread_damage(id int4 PRIMARY KEY,v int4); INSERT INTO checkpoint_unread_damage VALUES(1,10); SELECT syntra_seed_capture('checkpoint_unread_damage','id','v',8); SELECT syntra_apply_capture('checkpoint_unread_damage'); UPDATE syntra.capture_chunks SET segment=set_byte(segment,0,get_byte(segment,0) # 1) WHERE source_relation='checkpoint_unread_damage'::regclass AND partition=63; UPDATE checkpoint_unread_damage SET v=11 WHERE id=1");
    // Delta apply does not read the untouched payload. Prune must still reject it.
    assert_eq!(
        Spi::get_one::<i64>("SELECT syntra_apply_capture('checkpoint_unread_damage')").unwrap(),
        Some(1)
    );
    Spi::run("SELECT syntra_prune_capture('checkpoint_unread_damage')").unwrap();
}

#[pg_test]
fn chunked_initial_publication_includes_the_captured_prefix() {
    capture_fixture("CREATE TABLE checkpoint_first_prefix(id int4 PRIMARY KEY,v int4); INSERT INTO checkpoint_first_prefix VALUES(1,NULL),(2,-2147483648); SELECT syntra_seed_capture('checkpoint_first_prefix','id','v',8); UPDATE checkpoint_first_prefix SET id=3,v=2147483647 WHERE id=1; DELETE FROM checkpoint_first_prefix WHERE id=2");
    assert_eq!(
        Spi::get_one::<i64>("SELECT syntra_apply_capture('checkpoint_first_prefix')").unwrap(),
        Some(3)
    );
    let bytes=Spi::get_one::<Vec<u8>>("SELECT manifest FROM syntra.capture_checkpoints WHERE source_relation='checkpoint_first_prefix'::regclass LIMIT 1").unwrap().unwrap();
    let manifest = syntra_storage::chunks::Manifest::decode(&bytes).unwrap();
    assert_eq!(
        (
            manifest.applied,
            manifest.generation,
            manifest.rows().unwrap()
        ),
        (3, 1, 1)
    );
    assert!(manifest.chunks.iter().all(|c| c.revision == 0));
    assert_eq!(Spi::get_one::<i64>("SELECT sum(s.sum)::bigint FROM syntra.capture_chunks AS c,LATERAL syntra_segment_summary(c.segment) AS s WHERE c.source_relation='checkpoint_first_prefix'::regclass").unwrap(),Some(2147483647));
}

#[pg_test]
fn chunked_layout_handles_deliberately_colliding_keys() {
    capture_fixture("CREATE TABLE checkpoint_skew(id int4 PRIMARY KEY,v int4)");
    let keys: Vec<i32> = (0..100000)
        .filter(|key| syntra_storage::chunks::partition(*key) == 0)
        .take(1000)
        .collect();
    assert_eq!(keys.len(), 1000);
    Spi::run_with_args(
        "INSERT INTO checkpoint_skew SELECT k,k FROM unnest($1::int4[]) AS t(k)",
        &[keys.into()],
    )
    .unwrap();
    Spi::run("SELECT syntra_seed_capture('checkpoint_skew','id','v',8); SELECT syntra_apply_capture('checkpoint_skew'); UPDATE checkpoint_skew SET v=NULL WHERE id=0; SELECT syntra_apply_capture('checkpoint_skew')").unwrap();
    assert_eq!(Spi::get_one::<bool>("SELECT count(*)=64 AND max(row_count)=1000 AND count(*) FILTER(WHERE revision=1)=1 AND max(revision) FILTER(WHERE partition=0)=1 FROM syntra.capture_chunks WHERE source_relation='checkpoint_skew'::regclass").unwrap(),Some(true));
    // One changed row is a delta over partition 0's unchanged 1,000-row base.
    assert_eq!(Spi::get_one::<bool>("SELECT count(*) FILTER(WHERE delta IS NOT NULL)=1 AND bool_and(delta IS NULL) FILTER(WHERE partition<>0) AND (SELECT s.rows=1000 AND s.non_nulls=1000 FROM syntra.capture_chunks AS c,LATERAL syntra_segment_summary(c.segment) AS s WHERE c.source_relation='checkpoint_skew'::regclass AND partition=0) FROM syntra.capture_chunks WHERE source_relation='checkpoint_skew'::regclass").unwrap(),Some(true));
    let (summary, replayed) = tail_snapshot("checkpoint_skew").unwrap();
    assert_eq!((summary.0, summary.1, replayed), (1000, 999, false));
}

// pgrx test transactions have an assigned XID. Exercise the exact snapshot engine
// directly here; committed-session tests exercise its public own-XID guard.
fn tail_snapshot(source: &str) -> Result<(crate::source::WideSummary, bool), &'static str> {
    let oid = Spi::get_one_with_args::<pg_sys::Oid>("SELECT $1::regclass::oid", &[source.into()])
        .unwrap()
        .unwrap();
    // SAFETY: this test resolved an existing relation and keeps its read lock.
    let source = unsafe { pgrx::PgRelation::with_lock(oid, pg_sys::AccessShareLock as i32) };
    let _snapshot = crate::capture::Snapshot::latest();
    crate::checkpoint::snapshot_summary(&source, crate::source::source_snapshot(), 2, 4, true)
}

#[pg_test]
fn tail_replay_recomputes_extrema_nulls_and_empty_state_after_deletes() {
    capture_fixture("CREATE TABLE tail_extrema(id int4 PRIMARY KEY,v int4); INSERT INTO tail_extrema VALUES(1,-50),(2,NULL),(3,100),(4,7); SELECT syntra_seed_capture('tail_extrema','id','v',16); SELECT syntra_apply_capture('tail_extrema'); DELETE FROM tail_extrema WHERE id IN(1,3); UPDATE tail_extrema SET id=5,v=NULL WHERE id=4; UPDATE tail_extrema SET v=-2 WHERE id=2; INSERT INTO tail_extrema VALUES(6,8)");
    assert_eq!(
        tail_snapshot("tail_extrema"),
        Ok(((3, 2, Some(6), Some(-2), Some(8)), true))
    );
    Spi::run("DELETE FROM tail_extrema WHERE id IN(2,6)").unwrap();
    assert_eq!(
        tail_snapshot("tail_extrema"),
        Ok(((1, 0, None, None, None), true))
    );
    Spi::run("DELETE FROM tail_extrema WHERE id=5").unwrap();
    assert_eq!(
        tail_snapshot("tail_extrema"),
        Ok(((0, 0, None, None, None), true))
    );
    assert_eq!(Spi::get_one::<bool>("SELECT applied=0 AND generation=1 AND row_count=4 FROM syntra.capture_checkpoints WHERE source_relation='tail_extrema'::regclass LIMIT 1").unwrap(),Some(true));
}

#[pg_test]
fn tail_replay_preserves_repeated_key_order_and_rejects_bad_before_images() {
    capture_fixture("CREATE TABLE tail_order(id int4 PRIMARY KEY,v int4); INSERT INTO tail_order VALUES(1,1); SELECT syntra_seed_capture('tail_order','id','v',16); SELECT syntra_apply_capture('tail_order'); UPDATE tail_order SET v=2 WHERE id=1; DELETE FROM tail_order WHERE id=1; INSERT INTO tail_order VALUES(1,NULL); UPDATE tail_order SET v=-9 WHERE id=1");
    assert_eq!(
        tail_snapshot("tail_order"),
        Ok(((1, 1, Some(-9), Some(-9), Some(-9)), true))
    );
    Spi::run("UPDATE syntra.capture_events SET before_value=3 WHERE source_relation='tail_order'::regclass AND ordinal=2").unwrap();
    assert_eq!(tail_snapshot("tail_order"), Err("checkpoint_tail_rejected"));
}

#[pg_test]
fn tail_replay_rejects_missing_event_history() {
    capture_fixture("CREATE TABLE tail_gap(id int4 PRIMARY KEY,v int4); INSERT INTO tail_gap VALUES(1,1); SELECT syntra_seed_capture('tail_gap','id','v',16); SELECT syntra_apply_capture('tail_gap'); UPDATE tail_gap SET v=2 WHERE id=1; DELETE FROM syntra.capture_events WHERE source_relation='tail_gap'::regclass AND ordinal=1");
    assert_eq!(tail_snapshot("tail_gap"), Err("checkpoint_history_gap"));
}

#[pg_test]
fn tail_replay_accepts_256_events_and_rejects_257_without_publication() {
    capture_fixture("CREATE TABLE tail_events(id int4 PRIMARY KEY,v int4); INSERT INTO tail_events VALUES(1,0); SELECT syntra_seed_capture('tail_events','id','v',4096); SELECT syntra_apply_capture('tail_events'); DO $$ BEGIN FOR n IN 1..256 LOOP UPDATE tail_events SET v=v+1 WHERE id=1; END LOOP; END $$");
    assert_eq!(
        tail_snapshot("tail_events"),
        Ok(((1, 1, Some(256), Some(256), Some(256)), true))
    );
    Spi::run("UPDATE tail_events SET v=v+1 WHERE id=1").unwrap();
    assert_eq!(tail_snapshot("tail_events"), Err("checkpoint_tail_limit"));
    assert_eq!(Spi::get_one::<i64>("SELECT applied FROM syntra.capture_checkpoints WHERE source_relation='tail_events'::regclass LIMIT 1").unwrap(),Some(0));
}

#[pg_test]
fn tail_replay_row_budget_covers_skew_and_worst_case_insert_growth() {
    capture_fixture("CREATE TABLE tail_skew(id int4 PRIMARY KEY,v int4)");
    let keys: Vec<i32> = (0..10_000_000)
        .filter(|key| syntra_storage::chunks::partition(*key) == 0)
        .take(65535)
        .collect();
    assert_eq!(keys.len(), 65535);
    Spi::run_with_args(
        "INSERT INTO tail_skew SELECT k,k FROM unnest($1::int4[]) AS t(k)",
        &[keys.into()],
    )
    .unwrap();
    Spi::run("SELECT syntra_seed_capture('tail_skew','id','v',16); SELECT syntra_apply_capture('tail_skew'); UPDATE tail_skew SET v=NULL WHERE id=0").unwrap();
    let (summary, replay) = tail_snapshot("tail_skew").unwrap();
    assert_eq!((summary.0, summary.1, replay), (65535, 65534, true));
    Spi::run("UPDATE tail_skew SET v=1 WHERE id=0").unwrap();
    assert_eq!(tail_snapshot("tail_skew"), Err("checkpoint_tail_row_limit"));
}

#[pg_test]
fn tail_replay_rejects_damaged_touched_state_and_clean_segments() {
    capture_fixture("CREATE TABLE tail_damage(id int4 PRIMARY KEY,v int4); INSERT INTO tail_damage VALUES(1,1); SELECT syntra_seed_capture('tail_damage','id','v',16); SELECT syntra_apply_capture('tail_damage'); UPDATE tail_damage SET v=2 WHERE id=1; UPDATE syntra.capture_chunks SET state=set_byte(state,0,get_byte(state,0) # 1) WHERE source_relation='tail_damage'::regclass AND partition=55");
    assert_eq!(tail_snapshot("tail_damage"), Err("invalid_checkpoint"));
    Spi::run("UPDATE syntra.capture_chunks SET state=set_byte(state,0,get_byte(state,0) # 1) WHERE source_relation='tail_damage'::regclass AND partition=55; UPDATE syntra.capture_chunks SET segment=set_byte(segment,0,get_byte(segment,0) # 1) WHERE source_relation='tail_damage'::regclass AND partition=63").unwrap();
    assert_eq!(tail_snapshot("tail_damage"), Err("invalid_checkpoint"));
}

#[pg_test]
fn tail_input_byte_budget_checks_boundaries_and_overflow() {
    let limit = 8 * 1024 * 1024;
    assert_eq!(crate::checkpoint::input_bytes(0, limit, limit), Ok(limit));
    assert_eq!(
        crate::checkpoint::input_bytes(limit, 1, limit),
        Err("checkpoint_tail_byte_limit")
    );
    assert_eq!(
        crate::checkpoint::input_bytes(usize::MAX, 1, usize::MAX),
        Err("checkpoint_tail_byte_limit")
    );
}

#[pg_test]
fn tail_range_scan_skips_missing_already_applied_history() {
    capture_fixture("CREATE TABLE tail_range(id int4 PRIMARY KEY,v int4); INSERT INTO tail_range VALUES(1,0); SELECT syntra_seed_capture('tail_range','id','v',16); SELECT syntra_apply_capture('tail_range'); UPDATE tail_range SET v=1 WHERE id=1; SELECT syntra_apply_capture('tail_range'); UPDATE tail_range SET v=2 WHERE id=1; DELETE FROM syntra.capture_events WHERE source_relation='tail_range'::regclass AND ordinal=1");
    // The durable checkpoint covers ordinal 1. Reader replay needs only ordinal 2.
    assert_eq!(
        tail_snapshot("tail_range"),
        Ok(((1, 1, Some(2), Some(2), Some(2)), true))
    );
    Spi::run("DELETE FROM syntra.capture_events WHERE source_relation='tail_range'::regclass AND ordinal=2").unwrap();
    assert_eq!(tail_snapshot("tail_range"), Err("checkpoint_history_gap"));
}

#[pg_test]
fn revision_lookup_rescan_remaps_reordered_index_attributes() {
    capture_fixture("CREATE TABLE revision_lookup(id int4 PRIMARY KEY,v int4); INSERT INTO revision_lookup SELECT n,n FROM generate_series(1,1000) AS n; SELECT syntra_seed_capture('revision_lookup','id','v',16); SELECT syntra_apply_capture('revision_lookup'); DROP INDEX syntra.capture_chunks_version_index; CREATE INDEX capture_chunks_version_index ON syntra.capture_chunks(revision,partition,source_relation,epoch); UPDATE revision_lookup SET v=-1 WHERE id=1");
    assert_eq!(
        tail_snapshot("revision_lookup"),
        Ok(((1000, 1000, Some(500498), Some(-1), Some(1000)), true))
    );
    assert_eq!(
        Spi::get_one::<i64>("SELECT syntra_apply_capture('revision_lookup')").unwrap(),
        Some(1)
    );
    assert_eq!(
        Spi::get_one::<i64>("SELECT syntra_prune_capture('revision_lookup')").unwrap(),
        Some(1)
    );
    assert_eq!(
        tail_snapshot("revision_lookup"),
        Ok(((1000, 1000, Some(500498), Some(-1), Some(1000)), false))
    );
}

#[pg_test]
fn revision_lookup_fails_closed_when_its_index_is_missing() {
    capture_fixture("CREATE TABLE revision_missing(id int4 PRIMARY KEY,v int4); INSERT INTO revision_missing VALUES(1,1); SELECT syntra_seed_capture('revision_missing','id','v',16); SELECT syntra_apply_capture('revision_missing'); DROP INDEX syntra.capture_chunks_version_index; UPDATE revision_missing SET v=2 WHERE id=1");
    assert_eq!(
        tail_snapshot("revision_missing"),
        Err("checkpoint_chunk_index_unavailable")
    );
    Spi::run("CREATE INDEX capture_chunks_version_index ON syntra.capture_chunks(source_relation,epoch,partition,revision)").unwrap();
    assert_eq!(
        tail_snapshot("revision_missing"),
        Ok(((1, 1, Some(2), Some(2), Some(2)), true))
    );
}

#[pg_test]
fn metadata_reverse_scan_uses_index_order_and_keeps_heap_fallback() {
    Spi::run("CREATE TABLE syntra.reverse_probe(epoch bigint PRIMARY KEY); INSERT INTO syntra.reverse_probe VALUES(1),(3),(2)").unwrap();
    let mut scan = crate::native_catalog::CatalogScan::open(
        c"syntra",
        c"reverse_probe",
        c"reverse_probe_pkey",
        std::ptr::null_mut(),
        &[],
    )
    .unwrap();
    for expected in [3_i64, 2, 1] {
        assert_eq!(
            scan.next_back().unwrap().get::<i64>(1, "epoch"),
            Some(expected)
        );
    }
    assert!(scan.next_back().is_none());
    drop(scan);
    let mut scan = crate::native_catalog::CatalogScan::open(
        c"syntra",
        c"reverse_probe",
        c"missing_reverse_index",
        std::ptr::null_mut(),
        &[],
    )
    .unwrap();
    let mut values = Vec::new();
    while let Some(row) = scan.next_back() {
        values.push(row.get::<i64>(1, "epoch").unwrap());
    }
    values.sort_unstable();
    assert_eq!(values, vec![1, 2, 3]);
}

#[pg_test]
fn combined_maintenance_seeds_pending_events_and_retries_without_rewriting() {
    capture_fixture("CREATE TABLE combined_seed(id int4 PRIMARY KEY,v int4); INSERT INTO combined_seed VALUES(1,10),(2,NULL); SELECT syntra_seed_capture('combined_seed','id','v',16); UPDATE combined_seed SET id=3,v=-2 WHERE id=1");
    assert_eq!(
        Spi::get_one::<bool>(
            "SELECT applied=2 AND pruned=2 FROM syntra_apply_prune_capture('combined_seed')"
        )
        .unwrap(),
        Some(true)
    );
    assert_eq!(
        tail_snapshot("combined_seed"),
        Ok(((2, 1, Some(-2), Some(-2), Some(-2)), false))
    );
    Spi::run("CREATE TEMP TABLE combined_before AS SELECT ctid::text AS tuple,xmin::text AS xid,manifest FROM syntra.capture_checkpoints WHERE source_relation='combined_seed'::regclass").unwrap();
    assert_eq!(
        Spi::get_one::<bool>(
            "SELECT applied=2 AND pruned=0 FROM syntra_apply_prune_capture('combined_seed')"
        )
        .unwrap(),
        Some(true)
    );
    assert_eq!(Spi::get_one::<bool>("SELECT c.ctid::text=b.tuple AND c.xmin::text=b.xid AND c.manifest=b.manifest FROM syntra.capture_checkpoints AS c,combined_before AS b WHERE c.source_relation='combined_seed'::regclass LIMIT 1").unwrap(),Some(true));
    Spi::run("DELETE FROM combined_seed WHERE id IN (2,3)").unwrap();
    assert_eq!(
        Spi::get_one::<bool>(
            "SELECT applied=4 AND pruned=2 FROM syntra_apply_prune_capture('combined_seed')"
        )
        .unwrap(),
        Some(true)
    );
    assert_eq!(
        tail_snapshot("combined_seed"),
        Ok(((0, 0, None, None, None), false))
    );
}

#[pg_test]
fn combined_maintenance_rejects_clean_partition_damage_and_rolls_back_publication() {
    capture_fixture("CREATE TABLE combined_clean(id int4 PRIMARY KEY,v int4); INSERT INTO combined_clean VALUES(1,10); SELECT syntra_seed_capture('combined_clean','id','v',16); SELECT syntra_apply_capture('combined_clean'); UPDATE combined_clean SET v=11 WHERE id=1; UPDATE syntra.capture_chunks SET segment=set_byte(segment,0,get_byte(segment,0) # 1) WHERE source_relation='combined_clean'::regclass AND partition=0");
    Spi::run("DO $$ BEGIN PERFORM syntra_apply_prune_capture('combined_clean'); EXCEPTION WHEN object_not_in_prerequisite_state THEN NULL; END $$").unwrap();
    assert_eq!(Spi::get_one::<bool>("SELECT c.applied=0 AND c.generation=1 AND h.pruned=0 AND (SELECT count(*) FROM syntra.capture_events WHERE source_relation=h.source_relation)=1 FROM syntra.capture_heads AS h JOIN syntra.capture_checkpoints AS c USING(source_relation,epoch) WHERE h.source_relation='combined_clean'::regclass LIMIT 1").unwrap(),Some(true));
    Spi::run("UPDATE syntra.capture_chunks SET segment=set_byte(segment,0,get_byte(segment,0) # 1) WHERE source_relation='combined_clean'::regclass AND partition=0").unwrap();
    assert_eq!(
        Spi::get_one::<bool>(
            "SELECT applied=1 AND pruned=1 FROM syntra_apply_prune_capture('combined_clean')"
        )
        .unwrap(),
        Some(true)
    );
}

#[pg_test]
fn combined_maintenance_checks_bytes_altered_during_publication() {
    capture_fixture("CREATE TABLE combined_stored(id int4 PRIMARY KEY,v int4); INSERT INTO combined_stored VALUES(1,10); SELECT syntra_seed_capture('combined_stored','id','v',16); SELECT syntra_apply_capture('combined_stored'); UPDATE combined_stored SET v=11 WHERE id=1");
    Spi::run("CREATE FUNCTION combined_damage() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN NEW.segment=set_byte(NEW.segment,0,get_byte(NEW.segment,0) # 1); RETURN NEW; END $$; CREATE TRIGGER combined_damage BEFORE UPDATE ON syntra.capture_chunks FOR EACH ROW EXECUTE FUNCTION combined_damage(); DO $$ BEGIN PERFORM syntra_apply_prune_capture('combined_stored'); EXCEPTION WHEN object_not_in_prerequisite_state THEN NULL; END $$").unwrap();
    assert_eq!(Spi::get_one::<bool>("SELECT c.applied=0 AND c.generation=1 AND h.pruned=0 AND (SELECT count(*) FROM syntra.capture_events WHERE source_relation=h.source_relation)=1 FROM syntra.capture_heads AS h JOIN syntra.capture_checkpoints AS c USING(source_relation,epoch) WHERE h.source_relation='combined_stored'::regclass LIMIT 1").unwrap(),Some(true));
    Spi::run("DROP TRIGGER combined_damage ON syntra.capture_chunks").unwrap();
    assert_eq!(
        Spi::get_one::<bool>(
            "SELECT applied=1 AND pruned=1 FROM syntra_apply_prune_capture('combined_stored')"
        )
        .unwrap(),
        Some(true)
    );
    assert_eq!(
        tail_snapshot("combined_stored"),
        Ok(((1, 1, Some(11), Some(11), Some(11)), false))
    );
}

#[pg_test]
fn combined_maintenance_prune_failure_rolls_back_chunks_header_and_cursor() {
    capture_fixture("CREATE TABLE combined_abort(id int4 PRIMARY KEY,v int4); INSERT INTO combined_abort VALUES(1,10); SELECT syntra_seed_capture('combined_abort','id','v',16); SELECT syntra_apply_capture('combined_abort'); UPDATE combined_abort SET v=11 WHERE id=1");
    Spi::run("CREATE FUNCTION combined_block_prune() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION USING ERRCODE='P7501',MESSAGE='injected prune failure'; END $$; CREATE TRIGGER combined_block_prune BEFORE DELETE ON syntra.capture_events FOR EACH ROW EXECUTE FUNCTION combined_block_prune(); DO $$ BEGIN PERFORM syntra_apply_prune_capture('combined_abort'); EXCEPTION WHEN SQLSTATE 'P7501' THEN NULL; END $$").unwrap();
    assert_eq!(Spi::get_one::<bool>("SELECT c.applied=0 AND c.generation=1 AND h.pruned=0 AND (SELECT count(*) FROM syntra.capture_events WHERE source_relation=h.source_relation)=1 FROM syntra.capture_heads AS h JOIN syntra.capture_checkpoints AS c USING(source_relation,epoch) WHERE h.source_relation='combined_abort'::regclass LIMIT 1").unwrap(),Some(true));
    assert_eq!(Spi::get_one::<i64>("SELECT sum(s.sum)::bigint FROM syntra.capture_chunks AS c,LATERAL syntra_segment_summary(c.segment) AS s WHERE source_relation='combined_abort'::regclass").unwrap(),Some(10));
    Spi::run("DROP TRIGGER combined_block_prune ON syntra.capture_events").unwrap();
    assert_eq!(
        Spi::get_one::<bool>(
            "SELECT applied=1 AND pruned=1 FROM syntra_apply_prune_capture('combined_abort')"
        )
        .unwrap(),
        Some(true)
    );
}

#[pg_test(error = "invalid_checkpoint")]
fn combined_noop_still_validates_all_persisted_chunks() {
    capture_fixture("CREATE TABLE combined_noop(id int4 PRIMARY KEY,v int4); INSERT INTO combined_noop VALUES(1,10); SELECT syntra_seed_capture('combined_noop','id','v',16); SELECT syntra_apply_prune_capture('combined_noop'); UPDATE syntra.capture_chunks SET segment=set_byte(segment,0,get_byte(segment,0) # 1) WHERE source_relation='combined_noop'::regclass AND partition=0; SELECT syntra_apply_prune_capture('combined_noop')");
}

#[pg_test(error = "checkpoint maintenance requires a superuser")]
fn combined_maintenance_requires_superuser_even_after_execute_grant() {
    capture_fixture("CREATE ROLE combined_denied NOLOGIN; CREATE TABLE combined_acl(id int4 PRIMARY KEY,v int4)");
    assert_eq!(Spi::get_one::<bool>("SELECT has_function_privilege('combined_denied','syntra_apply_prune_capture(regclass)','EXECUTE')").unwrap(),Some(false));
    Spi::run("GRANT EXECUTE ON FUNCTION syntra_apply_prune_capture(regclass) TO combined_denied; SET LOCAL ROLE combined_denied; SELECT syntra_apply_prune_capture('combined_acl')").unwrap();
}

fn input_pass() -> (i64, i64, i64) {
    Spi::get_three::<i64, i64, i64>(
        "SELECT chunks,bytes_read,peak_bytes FROM syntra_checkpoint_input_bytes()",
    )
    .map(|(c, b, p)| (c.unwrap(), b.unwrap(), p.unwrap()))
    .unwrap()
}

#[pg_test]
fn chunk_passes_read_exact_stored_bytes_one_partition_at_a_time() {
    capture_fixture("CREATE TABLE input_pass(id int4 PRIMARY KEY,v int4); INSERT INTO input_pass SELECT n,CASE WHEN n%13=0 THEN NULL ELSE 7 END FROM generate_series(1,20000) AS n; SELECT syntra_seed_capture('input_pass','id','v',16); SELECT syntra_apply_prune_capture('input_pass')");
    // Repetitive values make TOAST compress states, so exact lengths are
    // checked through compressed datums rather than plain inline bytes.
    assert_eq!(Spi::get_one::<bool>("SELECT count(*) FILTER(WHERE pg_column_compression(state) IS NOT NULL)>0 FROM syntra.capture_chunks WHERE source_relation='input_pass'::regclass").unwrap(),Some(true));
    let (all, largest, segments) = Spi::get_three::<i64, i64, i64>("SELECT sum(octet_length(state)+octet_length(segment))::bigint,max(octet_length(state)+octet_length(segment))::bigint,sum(octet_length(segment))::bigint FROM syntra.capture_chunks WHERE source_relation='input_pass'::regclass").unwrap();
    let (all, largest, segments) = (all.unwrap(), largest.unwrap(), segments.unwrap());
    assert!(largest * 32 < all);
    Spi::run("SELECT syntra_apply_prune_capture('input_pass')").unwrap();
    assert_eq!(input_pass(), (64, all, largest));
    let (summary, replay) = tail_snapshot("input_pass").unwrap();
    assert_eq!((summary.0, replay), (20000, false));
    let (chunks, bytes, peak) = input_pass();
    assert_eq!((chunks, bytes), (64, segments));
    assert!(peak * 32 < segments);
}

#[pg_test]
fn chunk_pass_rejects_wrong_length_bytes_before_reading_them() {
    capture_fixture("CREATE TABLE input_length(id int4 PRIMARY KEY,v int4); INSERT INTO input_length SELECT n,n FROM generate_series(1,2000) AS n; SELECT syntra_seed_capture('input_length','id','v',16); SELECT syntra_apply_capture('input_length'); UPDATE syntra.capture_chunks SET segment=segment||'\\x00'::bytea WHERE source_relation='input_length'::regclass AND partition=40");
    let before = Spi::get_one::<i64>("SELECT sum(octet_length(segment))::bigint FROM syntra.capture_chunks WHERE source_relation='input_length'::regclass AND partition<40").unwrap().unwrap();
    assert_eq!(tail_snapshot("input_length"), Err("invalid_checkpoint"));
    // Partition 40's bytes are never fetched, so they are absent from the count.
    let (chunks, bytes, _) = input_pass();
    assert_eq!((chunks, bytes), (40, before));
    // Truncated state bytes stop maintenance before publication or pruning.
    Spi::run("UPDATE syntra.capture_chunks SET segment=substring(segment FROM 1 FOR octet_length(segment)-1),state=substring(state FROM 1 FOR octet_length(state)-1) WHERE source_relation='input_length'::regclass AND partition=40; UPDATE input_length SET v=0 WHERE id=1; DO $$ BEGIN PERFORM syntra_apply_prune_capture('input_length'); RAISE EXCEPTION 'maintenance unexpectedly succeeded'; EXCEPTION WHEN object_not_in_prerequisite_state THEN NULL; END $$").unwrap();
    assert_eq!(Spi::get_one::<bool>("SELECT c.applied=0 AND c.generation=1 AND h.pruned=0 AND h.emitted=1 FROM capture_position AS h JOIN syntra.capture_checkpoints AS c USING(source_relation,epoch) WHERE h.source_relation='input_length'::regclass LIMIT 1").unwrap(),Some(true));
}

#[pg_test]
fn cleanup_horizon_age_counts_transactions_started_after_the_oldest_snapshot() {
    capture_fixture("CREATE TABLE horizon_values(id int4 PRIMARY KEY,v int4); SELECT syntra_seed_capture('horizon_values','id','v',4); CREATE TABLE horizon_burn(n int4)");
    let before = Spi::get_one::<i64>("SELECT syntra_cleanup_horizon_age()")
        .unwrap()
        .unwrap();
    Spi::run("DO $$ BEGIN FOR i IN 1..50 LOOP BEGIN INSERT INTO horizon_burn VALUES(i); EXCEPTION WHEN OTHERS THEN NULL; END; END LOOP; END $$").unwrap();
    let after = Spi::get_one::<i64>("SELECT syntra_cleanup_horizon_age()")
        .unwrap()
        .unwrap();
    assert!((50..=52).contains(&(after - before)), "{before} -> {after}");
}

#[pg_test]
fn tail_replay_rejects_an_event_past_the_newest_mark() {
    capture_fixture("CREATE TABLE mark_lost(id int4 PRIMARY KEY,v int4); INSERT INTO mark_lost VALUES(1,1); SELECT syntra_seed_capture('mark_lost','id','v',16); SELECT syntra_apply_capture('mark_lost'); UPDATE mark_lost SET v=2 WHERE id=1; UPDATE mark_lost SET v=3 WHERE id=1");
    assert_eq!(
        tail_snapshot("mark_lost"),
        Ok(((1, 1, Some(3), Some(3), Some(3)), true))
    );
    Spi::run("DELETE FROM syntra.capture_marks WHERE source_relation='mark_lost'::regclass AND ordinal=2").unwrap();
    assert_eq!(tail_snapshot("mark_lost"), Err("checkpoint_history_gap"));
}

#[pg_test]
fn pruning_every_mark_folds_the_live_row_count_into_the_head() {
    capture_fixture("CREATE TABLE fold_rows(id int4 PRIMARY KEY,v int4); INSERT INTO fold_rows VALUES(1,1); SELECT syntra_seed_capture('fold_rows','id','v',16); SELECT syntra_apply_capture('fold_rows'); INSERT INTO fold_rows VALUES(2,2),(3,3); SELECT syntra_apply_prune_capture('fold_rows')");
    assert_eq!(Spi::get_one::<bool>("SELECT emitted=2 AND pruned=2 AND live_rows=3 AND NOT EXISTS(SELECT ordinal FROM syntra.capture_marks WHERE source_relation='fold_rows'::regclass) FROM syntra.capture_heads WHERE source_relation='fold_rows'::regclass").unwrap(),Some(true));
    Spi::run("DELETE FROM fold_rows WHERE id=1").unwrap();
    assert_eq!(
        tail_snapshot("fold_rows"),
        Ok(((2, 2, Some(5), Some(2), Some(3)), true))
    );
}

#[pg_test]
fn capture_lag_counts_events_the_checkpoint_has_not_applied() {
    capture_fixture("CREATE TABLE lag_values(id int4 PRIMARY KEY,v int4); INSERT INTO lag_values VALUES(1,1); SELECT syntra_seed_capture('lag_values','id','v',4)");
    let lag = || {
        Spi::get_one::<String>("SELECT concat_ws(',',emitted,coalesce(applied::text,'-'),pending_events,coalesce(generation::text,'-')) FROM syntra_capture_lag('lag_values')")
            .unwrap()
            .unwrap()
    };
    assert_eq!(lag(), "0,-,0,-");
    Spi::run("UPDATE lag_values SET v=2; UPDATE lag_values SET v=3").unwrap();
    assert_eq!(lag(), "2,-,2,-");
    Spi::get_one::<i64>("SELECT syntra_apply_capture('lag_values')").unwrap();
    assert_eq!(lag(), "2,2,0,1");
    Spi::run("UPDATE lag_values SET v=4").unwrap();
    assert_eq!(lag(), "3,2,1,1");
    Spi::get_one::<i64>("SELECT syntra_prune_capture('lag_values')").unwrap();
    assert_eq!(lag(), "3,2,1,1");
    Spi::get_one::<i64>("SELECT syntra_apply_capture('lag_values')").unwrap();
    assert_eq!(lag(), "3,3,0,2");
}

#[pg_test]
fn capture_lag_is_open_to_a_write_only_role() {
    capture_fixture("CREATE TABLE lag_acl(id int4 PRIMARY KEY,v int4); INSERT INTO lag_acl VALUES(1,1); SELECT syntra_seed_capture('lag_acl','id','v',4); UPDATE lag_acl SET v=2; CREATE ROLE lag_writer; GRANT INSERT ON lag_acl TO lag_writer");
    Spi::run("SET LOCAL ROLE lag_writer").unwrap();
    assert_eq!(
        Spi::get_one::<i64>("SELECT pending_events FROM syntra_capture_lag('lag_acl')").unwrap(),
        Some(1)
    );
}

#[pg_test(error = "capture lag requires schema USAGE and a table privilege")]
fn capture_lag_rejects_a_role_without_table_privileges() {
    capture_fixture("CREATE TABLE lag_closed(id int4 PRIMARY KEY,v int4); SELECT syntra_seed_capture('lag_closed','id','v',4); CREATE ROLE lag_stranger");
    Spi::run("SET LOCAL ROLE lag_stranger").unwrap();
    Spi::get_one::<i64>("SELECT pending_events FROM syntra_capture_lag('lag_closed')").unwrap();
}

#[pg_test(error = "uncaptured")]
fn capture_lag_rejects_a_table_that_is_not_captured() {
    capture_fixture("CREATE TABLE lag_plain(id int4 PRIMARY KEY,v int4)");
    Spi::get_one::<i64>("SELECT pending_events FROM syntra_capture_lag('lag_plain')").unwrap();
}

#[pg_test]
fn capture_status_reports_lag_and_headroom_for_every_captured_source() {
    capture_fixture("CREATE TABLE status_a(id int4 PRIMARY KEY,v int4); INSERT INTO status_a VALUES(1,1); SELECT syntra_seed_capture('status_a','id','v',8); SELECT syntra_apply_capture('status_a'); UPDATE status_a SET v=2; CREATE TABLE status_b(id int4 PRIMARY KEY,v int4); SELECT syntra_seed_capture('status_b','id','v',4); INSERT INTO status_b VALUES(1,1),(2,2)");
    assert_eq!(
        Spi::get_one::<String>("SELECT string_agg(concat_ws(',',source,emitted,coalesce(applied::text,'-'),pending_events,coalesce(generation::text,'-'),retained_events,max_events,free_events,coalesce(error,'-')),' ' ORDER BY source::text) FROM syntra_capture_status() WHERE source::text LIKE 'status_%'")
            .unwrap()
            .unwrap(),
        "status_a,1,0,1,1,1,8,7,- status_b,2,-,2,-,2,4,2,-"
    );
}

#[pg_test]
fn capture_status_leaves_out_sources_the_caller_cannot_inspect() {
    capture_fixture("CREATE TABLE status_open(id int4 PRIMARY KEY,v int4); SELECT syntra_seed_capture('status_open','id','v',4); CREATE TABLE status_hidden(id int4 PRIMARY KEY,v int4); SELECT syntra_seed_capture('status_hidden','id','v',4); CREATE ROLE status_watcher; GRANT SELECT ON status_open TO status_watcher");
    Spi::run("SET LOCAL ROLE status_watcher").unwrap();
    assert_eq!(
        Spi::get_one::<String>("SELECT string_agg(source::text,' ' ORDER BY source::text) FROM syntra_capture_status() WHERE source::text LIKE 'status_%'")
            .unwrap(),
        Some("status_open".to_string())
    );
}

#[pg_test]
fn capture_status_reports_a_broken_source_without_hiding_the_others() {
    capture_fixture("CREATE TABLE status_broken(id int4 PRIMARY KEY,v int4); INSERT INTO status_broken VALUES(1,1); SELECT syntra_seed_capture('status_broken','id','v',4); SELECT syntra_apply_capture('status_broken'); CREATE TABLE status_fine(id int4 PRIMARY KEY,v int4); SELECT syntra_seed_capture('status_fine','id','v',4); UPDATE syntra.capture_checkpoints SET applied=applied+100 WHERE source_relation='status_broken'::regclass");
    assert_eq!(
        Spi::get_one::<String>("SELECT string_agg(concat_ws(',',source,coalesce(pending_events::text,'-'),coalesce(free_events::text,'-'),coalesce(error,'-')),' ' ORDER BY source::text) FROM syntra_capture_status() WHERE source::text LIKE 'status_%'")
            .unwrap()
            .unwrap(),
        "status_broken,-,-,invalid_checkpoint status_fine,0,4,-"
    );
}
