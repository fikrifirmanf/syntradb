// Partition deltas. The oracle is PostgreSQL's own aggregate over the source table
// (sql_oracle, int8_tests.rs); reads go through the snapshot engine directly.
fn chunk_md5(table: &str) -> String {
    Spi::get_one_with_args::<String>(
        "SELECT string_agg(partition||':'||md5(state)||md5(segment),',' ORDER BY partition) FROM syntra.capture_chunks WHERE source_relation=$1::regclass",
        &[table.into()],
    )
    .unwrap()
    .unwrap()
}
/// Partitions with a stored delta. Apply appends each delta as its own row and
/// deletes the one it replaces; it never stores one inline.
fn deltas(table: &str) -> i64 {
    Spi::get_one_with_args::<i64>(
        "SELECT CASE WHEN EXISTS(SELECT FROM syntra.capture_chunks WHERE source_relation=$1::regclass AND delta IS NOT NULL) THEN -1 ELSE (SELECT count(DISTINCT partition) FROM syntra.capture_chunk_deltas WHERE source_relation=$1::regclass) END",
        &[table.into()],
    )
    .unwrap()
    .unwrap()
}
fn chunk_rows(table: &str) -> String {
    Spi::get_one_with_args::<String>(
        "SELECT string_agg(partition||':'||ctid::text,',' ORDER BY partition) FROM syntra.capture_chunks WHERE source_relation=$1::regclass",
        &[table.into()],
    )
    .unwrap()
    .unwrap()
}

#[pg_test]
fn a_small_change_writes_deltas_and_keeps_base_bytes() {
    capture_fixture("CREATE TABLE delta_small(id int4 PRIMARY KEY,v int4); INSERT INTO delta_small SELECT g,g FROM generate_series(1,4000) AS g; SELECT syntra_seed_capture('delta_small','id','v',4096); SELECT syntra_apply_capture('delta_small')");
    let before = chunk_md5("delta_small");
    let rows = chunk_rows("delta_small");
    assert_eq!(deltas("delta_small"), 0);
    // The current maximum leaves, so a reader must rescan that partition's base.
    Spi::run("UPDATE delta_small SET v=NULL WHERE id IN (7,8); DELETE FROM delta_small WHERE id=4000; INSERT INTO delta_small VALUES(5000,-1),(5001,NULL); UPDATE delta_small SET id=6000 WHERE id=9").unwrap();
    Spi::run("SELECT syntra_apply_capture('delta_small')").unwrap();
    let written = deltas("delta_small");
    assert!(written > 0);
    assert_eq!(
        chunk_md5("delta_small"),
        before,
        "base bytes were rewritten"
    );
    // Not one chunk row gained a version: an old snapshot keeps none for
    // later lookups to walk.
    assert_eq!(chunk_rows("delta_small"), rows, "a chunk row was updated");
    // A second change to the same partitions replaces their delta rows.
    Spi::run(
        "UPDATE delta_small SET v=-2 WHERE id IN (7,8); SELECT syntra_apply_capture('delta_small')",
    )
    .unwrap();
    assert_eq!(chunk_rows("delta_small"), rows, "a chunk row was updated");
    assert_eq!(Spi::get_one::<i64>("SELECT count(*)-count(DISTINCT partition) FROM syntra.capture_chunk_deltas WHERE source_relation='delta_small'::regclass").unwrap(), Some(0));
    assert_eq!(Spi::get_one::<i32>("SELECT octet_length(manifest) FROM syntra.capture_checkpoints WHERE source_relation='delta_small'::regclass").unwrap(), Some(8272));
    assert_eq!(
        tail_snapshot("delta_small"),
        Ok((sql_oracle("delta_small"), false))
    );
    // Pruning validates every delta against its base.
    assert_eq!(
        Spi::get_one::<i64>("SELECT syntra_prune_capture('delta_small')").unwrap(),
        Some(9)
    );
    // Lagging reads replay the tail on top of base and delta.
    Spi::run("UPDATE delta_small SET v=v+1 WHERE id BETWEEN 100 AND 110").unwrap();
    assert_eq!(
        tail_snapshot("delta_small"),
        Ok((sql_oracle("delta_small"), true))
    );
}

/// Rounds of inserts, updates, NULLs, deletes of the current extremes and key
/// moves, applying and pruning after each. Each round must read exactly, and the
/// fold rule must both keep deltas and write new bases during the run.
fn churn(table: &str, kind: &str, width: i16, scale: i64) {
    capture_fixture(&format!("CREATE TABLE {table}(id {kind} PRIMARY KEY,v {kind}); INSERT INTO {table} SELECT g,g*{scale} FROM generate_series(1,2000) AS g; SELECT syntra_seed_capture('{table}','id','v',4096); SELECT syntra_apply_capture('{table}')"));
    let (mut saw_delta, mut saw_fold) = (false, false);
    let mut previous = chunk_md5(table);
    for round in 0..40_i64 {
        Spi::run(&format!(
            "INSERT INTO {table} SELECT 10000+{round}*100+g,(g-50)*{scale} FROM generate_series(1,60) AS g;
             UPDATE {table} SET v=CASE WHEN id%3=0 THEN NULL ELSE v*-1 END WHERE id%41={round}%41;
             DELETE FROM {table} WHERE v=(SELECT max(v) FROM {table}) OR v=(SELECT min(v) FROM {table});
             UPDATE {table} SET id=id+1000000 WHERE id IN (SELECT id FROM {table} WHERE id<1000000 ORDER BY id OFFSET {round} LIMIT 2)"
        ))
        .unwrap();
        Spi::run(&format!("SELECT syntra_apply_prune_capture('{table}')")).unwrap();
        let read = if width == 4 {
            tail_snapshot(table)
        } else {
            snapshot8(table, width)
        };
        assert_eq!(
            read,
            Ok((sql_oracle(table), false)),
            "{table} round {round}"
        );
        saw_delta |= deltas(table) > 0;
        let now = chunk_md5(table);
        saw_fold |= now != previous;
        previous = now;
    }
    assert!(
        saw_delta && saw_fold,
        "{table}: delta {saw_delta}, fold {saw_fold}"
    );
}

#[pg_test]
fn int4_churn_with_folds_matches_postgresql() {
    churn("delta_churn4", "int4", 4, 1);
}

#[pg_test]
fn int8_churn_with_folds_matches_postgresql() {
    churn("delta_churn8", "int8", 8, 3_000_000_000);
}

fn damaged_delta(
    table: &str,
    damage: &str,
) -> Result<(crate::source::WideSummary, bool), &'static str> {
    capture_fixture(&format!("CREATE TABLE {table}(id int4 PRIMARY KEY,v int4); INSERT INTO {table} SELECT g,g FROM generate_series(1,500) AS g; SELECT syntra_seed_capture('{table}','id','v',64); SELECT syntra_apply_capture('{table}'); UPDATE {table} SET v=-v WHERE id=1; SELECT syntra_apply_capture('{table}')"));
    assert_eq!(deltas(table), 1);
    assert_eq!(
        Spi::run(&damage.replace("$T", &format!("'{table}'::regclass"))).map(|_| ()),
        Ok(())
    );
    tail_snapshot(table)
}

#[pg_test]
fn readers_refuse_damaged_missing_or_unexpected_deltas() {
    assert_eq!(
        damaged_delta("delta_flip", "UPDATE syntra.capture_chunk_deltas SET delta=set_byte(delta,80,get_byte(delta,80) # 1) WHERE source_relation=$T"),
        Err("invalid_checkpoint")
    );
    assert_eq!(
        damaged_delta(
            "delta_dropped",
            "DELETE FROM syntra.capture_chunk_deltas WHERE source_relation=$T"
        ),
        Err("invalid_checkpoint")
    );
    // A valid delta copied inline into chunk rows, which then hold more than
    // their base.
    assert_eq!(
        damaged_delta("delta_copied", "UPDATE syntra.capture_chunks AS c SET delta=d.delta FROM syntra.capture_chunk_deltas AS d WHERE c.source_relation=$T AND d.source_relation=$T"),
        Err("invalid_checkpoint")
    );
    // The appended delta also copied inline into its own partition's chunk row.
    assert_eq!(
        damaged_delta("delta_doubled", "UPDATE syntra.capture_chunks AS c SET delta=d.delta FROM syntra.capture_chunk_deltas AS d WHERE c.source_relation=$T AND d.source_relation=$T AND d.partition=c.partition"),
        Err("invalid_checkpoint")
    );
    // A delta row moved to another revision no longer matches the manifest.
    assert_eq!(
        damaged_delta(
            "delta_rebased",
            "UPDATE syntra.capture_chunk_deltas SET revision=revision+1 WHERE source_relation=$T"
        ),
        Err("invalid_checkpoint")
    );
    // The same length with one entry's value changed fails its digest.
    assert_eq!(
        damaged_delta("delta_value", "UPDATE syntra.capture_chunk_deltas SET delta=overlay(delta placing '\\x7f'::bytea from 83) WHERE source_relation=$T"),
        Err("invalid_checkpoint")
    );
}

#[pg_test(error = "invalid_checkpoint")]
fn pruning_refuses_a_damaged_delta() {
    damaged_delta("delta_prune", "UPDATE syntra.capture_chunk_deltas SET delta=set_byte(delta,78,get_byte(delta,78) # 1) WHERE source_relation=$T").ok();
    Spi::run("SELECT syntra_prune_capture('delta_prune')").unwrap();
}

#[pg_test]
fn folding_applies_pending_history_and_writes_the_030_format() {
    capture_fixture("CREATE TABLE delta_fold(id int4 PRIMARY KEY,v int4); INSERT INTO delta_fold SELECT g,g FROM generate_series(1,4000) AS g; SELECT syntra_seed_capture('delta_fold','id','v',4096); SELECT syntra_apply_capture('delta_fold'); UPDATE delta_fold SET v=-v WHERE id%500=0; SELECT syntra_apply_capture('delta_fold')");
    let partitions = deltas("delta_fold");
    assert!(partitions > 0);
    let before = chunk_md5("delta_fold");
    // Pending history is applied before the fold, and one partition only has it.
    Spi::run(
        "UPDATE delta_fold SET v=NULL WHERE id IN (3,4); DELETE FROM delta_fold WHERE id=4000",
    )
    .unwrap();
    let folded = Spi::get_one::<i64>("SELECT syntra_fold_capture('delta_fold')")
        .unwrap()
        .unwrap();
    assert!(folded >= partitions, "folded {folded} of {partitions}");
    assert_eq!(deltas("delta_fold"), 0);
    assert_ne!(chunk_md5("delta_fold"), before);
    assert_eq!(Spi::get_one::<String>("SELECT octet_length(manifest)||','||(applied=(SELECT emitted FROM syntra_capture_lag('delta_fold'))) FROM syntra.capture_checkpoints WHERE source_relation='delta_fold'::regclass").unwrap(), Some("5200,true".to_string()));
    assert_eq!(
        tail_snapshot("delta_fold"),
        Ok((sql_oracle("delta_fold"), false))
    );
    assert_eq!(
        Spi::get_one::<i64>("SELECT syntra_fold_capture('delta_fold')").unwrap(),
        Some(0)
    );
    // Pruning validates the folded bases, and later applies write deltas again.
    Spi::run("SELECT syntra_prune_capture('delta_fold'); UPDATE delta_fold SET v=2 WHERE id=1; SELECT syntra_apply_capture('delta_fold')").unwrap();
    assert_eq!(deltas("delta_fold"), 1);
    assert_eq!(
        tail_snapshot("delta_fold"),
        Ok((sql_oracle("delta_fold"), false))
    );
}

#[pg_test]
fn folding_an_int8_feed_keeps_wide_values_exact() {
    capture_fixture("CREATE TABLE delta_fold8(id int8 PRIMARY KEY,v int8); INSERT INTO delta_fold8 SELECT g*5000000000,g*3000000000 FROM generate_series(1,2000) AS g; SELECT syntra_seed_capture('delta_fold8','id','v',4096); SELECT syntra_apply_capture('delta_fold8'); UPDATE delta_fold8 SET v=-9000000000000000000 WHERE id%(5000000000*300)=0; SELECT syntra_apply_capture('delta_fold8')");
    assert!(deltas("delta_fold8") > 0);
    Spi::run("SELECT syntra_fold_capture('delta_fold8')").unwrap();
    assert_eq!(deltas("delta_fold8"), 0);
    assert_eq!(
        snapshot8("delta_fold8", 8),
        Ok((sql_oracle("delta_fold8"), false))
    );
}

#[pg_test(error = "invalid_checkpoint")]
fn folding_refuses_a_damaged_delta() {
    damaged_delta("delta_fold_bad", "UPDATE syntra.capture_chunk_deltas SET delta=set_byte(delta,78,get_byte(delta,78) # 1) WHERE source_relation=$T").ok();
    Spi::run("SELECT syntra_fold_capture('delta_fold_bad')").unwrap();
}

/// Moves every appended delta back inline, as 0.6.0 stored them.
fn store_inline(table: &str) {
    Spi::run(&format!("UPDATE syntra.capture_chunks AS c SET revision=d.revision,row_count=d.row_count,delta=d.delta FROM syntra.capture_chunk_deltas AS d WHERE c.source_relation='{table}'::regclass AND d.source_relation=c.source_relation AND d.partition=c.partition; DELETE FROM syntra.capture_chunk_deltas WHERE source_relation='{table}'::regclass")).unwrap();
}

#[pg_test]
fn inline_deltas_from_060_are_read_and_moved_by_the_next_apply() {
    capture_fixture("CREATE TABLE delta_inline(id int4 PRIMARY KEY,v int4); INSERT INTO delta_inline SELECT g,g FROM generate_series(1,2000) AS g; SELECT syntra_seed_capture('delta_inline','id','v',4096); SELECT syntra_apply_capture('delta_inline'); UPDATE delta_inline SET v=-v WHERE id%300=0; SELECT syntra_apply_capture('delta_inline')");
    let partitions = deltas("delta_inline");
    assert!(partitions > 1);
    store_inline("delta_inline");
    assert_eq!(deltas("delta_inline"), -1);
    assert_eq!(
        tail_snapshot("delta_inline"),
        Ok((sql_oracle("delta_inline"), false))
    );
    assert_eq!(
        Spi::get_one::<i64>("SELECT syntra_prune_capture('delta_inline')").unwrap(),
        Some(6)
    );
    // One partition changes again: its delta moves to its own row and its chunk
    // row goes back to its base revision. The others stay inline.
    Spi::run(
        "UPDATE delta_inline SET v=7 WHERE id=300; SELECT syntra_apply_capture('delta_inline')",
    )
    .unwrap();
    assert_eq!(Spi::get_one_with_args::<bool>("SELECT (SELECT count(*) FROM syntra.capture_chunk_deltas WHERE source_relation='delta_inline'::regclass)=1 AND count(*) FILTER(WHERE delta IS NOT NULL)=$1-1 AND bool_and(revision=0) FILTER(WHERE delta IS NULL) FROM syntra.capture_chunks WHERE source_relation='delta_inline'::regclass", &[partitions.into()]).unwrap(), Some(true));
    assert_eq!(
        tail_snapshot("delta_inline"),
        Ok((sql_oracle("delta_inline"), false))
    );
    // A fold rewrites the remaining inline partitions as bases.
    Spi::run("SELECT syntra_fold_capture('delta_inline')").unwrap();
    assert_eq!(deltas("delta_inline"), 0);
    assert_eq!(
        tail_snapshot("delta_inline"),
        Ok((sql_oracle("delta_inline"), false))
    );
}

/// Inline deltas as 0.6.0 stored them, in a catalog without the delta table,
/// as between installing the 0.7.0 library and running ALTER EXTENSION UPDATE.
fn without_delta_table(table: &str) {
    capture_fixture(&format!("CREATE TABLE {table}(id int4 PRIMARY KEY,v int4); INSERT INTO {table} SELECT g,g FROM generate_series(1,500) AS g; SELECT syntra_seed_capture('{table}','id','v',64); SELECT syntra_apply_capture('{table}'); UPDATE {table} SET v=-v WHERE id=1; SELECT syntra_apply_capture('{table}')"));
    store_inline(table);
    Spi::run("SET LOCAL event_triggers=off; ALTER EXTENSION syntradb DROP TABLE syntra.capture_chunk_deltas; DROP TABLE syntra.capture_chunk_deltas; SET LOCAL event_triggers=on").unwrap();
}

#[pg_test]
fn before_the_update_creates_the_delta_table_inline_deltas_still_read() {
    without_delta_table("delta_early");
    assert_eq!(
        tail_snapshot("delta_early"),
        Ok((sql_oracle("delta_early"), false))
    );
    // Dropping an unrelated table runs the drop guard, which must not need it.
    Spi::run("CREATE TABLE delta_early_other(id int4); DROP TABLE delta_early_other").unwrap();
    // Stopping capture clears the feed without it.
    assert_eq!(
        Spi::get_one::<bool>("SELECT syntra_stop_capture('delta_early')").unwrap(),
        Some(true)
    );
}

#[pg_test(error = "relation \"syntra.capture_chunk_deltas\" does not exist")]
fn before_the_update_creates_the_delta_table_apply_fails() {
    without_delta_table("delta_early_apply");
    Spi::run("UPDATE delta_early_apply SET v=3 WHERE id=1; SELECT syntra_apply_capture('delta_early_apply')").unwrap();
}
