//! Atomic durable keyed state with explicit apply/prune and snapshot-safe reads.
use crate::{
    capture::{current_configuration, Configuration, Snapshot},
    native_catalog::{int8_key, oid_key, CatalogScan, Int8Comparison},
    source::WideSummary,
    Column,
};
use pgrx::prelude::*;
use pgrx::PgRelation;
use std::ffi::CStr;
use syntra_storage::chunks::Manifest;
use syntra_storage::replay::{Batch, Binding, Change, Limits, ReplayState, Row};
mod chunks;
type Checked<T> = std::result::Result<T, &'static str>;
const LIMITS: Limits = Limits {
    rows: 1_000_000,
    changes: 4096,
    buffer_bytes: 32 * 1024 * 1024,
};
pub(super) fn input_bytes(current: usize, next: usize, limit: usize) -> Checked<usize> {
    current
        .checked_add(next)
        .filter(|n| *n <= limit)
        .ok_or("checkpoint_tail_byte_limit")
}
fn keep() -> bool {
    pgrx::check_for_interrupts!();
    true
}
fn fail(reason: &'static str) -> ! {
    pgrx::ereport!(
        ERROR,
        pgrx::PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
        reason
    );
}
fn binding(config: &Configuration) -> Binding {
    Binding::derive(
        format!("{}:{}:{}", config.identity, config.key, config.value).as_bytes(),
        config.epoch as u64,
    )
}
fn open(
    source: &PgRelation,
    table: &'static CStr,
    index: &'static CStr,
    snapshot: pg_sys::Snapshot,
) -> Checked<CatalogScan> {
    CatalogScan::open(
        c"syntra",
        table,
        index,
        snapshot,
        &[oid_key(1, "source_relation", source.oid())],
    )
    .ok_or("checkpoint_metadata_missing")
}
struct Head {
    epoch: i64,
    emitted: i64,
    pruned: i64,
    rows: i64,
    base_rows: i64,
    value: i16,
}
fn head(source: &PgRelation, snapshot: pg_sys::Snapshot) -> Checked<Head> {
    let mut scan = open(source, c"capture_heads", c"capture_heads_pkey", snapshot)?;
    let row = scan.next_back().ok_or("uncaptured")?;
    let mut h = Head {
        epoch: row.get(2, "epoch").ok_or("invalid_capture_head")?,
        value: row
            .get(5, "value_attribute")
            .ok_or("invalid_capture_head")?,
        emitted: row.get(9, "emitted").ok_or("invalid_capture_head")?,
        base_rows: row.get(12, "base_rows").ok_or("invalid_capture_head")?,
        pruned: row.get(15, "pruned").ok_or("invalid_capture_head")?,
        rows: row.get(16, "live_rows").ok_or("invalid_capture_head")?,
    };
    drop(scan);
    (h.emitted, h.rows) = crate::capture::position(source, snapshot, h.epoch, h.emitted, h.rows)
        .ok_or("invalid_capture_head")?;
    // The newest mark bounds the log. An event past it means a lost mark.
    let mut beyond = CatalogScan::open(
        c"syntra",
        c"capture_events",
        c"capture_events_pkey",
        snapshot,
        &[
            int8_key(2, "epoch", h.epoch, Int8Comparison::Equal),
            int8_key(3, "ordinal", h.emitted, Int8Comparison::After),
        ],
    )
    .ok_or("checkpoint_metadata_missing")?;
    if beyond.next().is_some() {
        return Err("checkpoint_history_gap");
    }
    if h.epoch <= 0
        || h.pruned < 0
        || h.emitted < h.pruned
        || h.emitted - h.pruned > 4096
        || !(0..=1_000_000).contains(&h.rows)
        || !(0..=100000).contains(&h.base_rows)
    {
        return Err("invalid_capture_head");
    }
    Ok(h)
}
struct Checkpoint {
    epoch: i64,
    applied: i64,
    rows: i64,
    manifest: Manifest,
}
fn checkpoint(source: &PgRelation, snapshot: pg_sys::Snapshot) -> Checked<Option<Checkpoint>> {
    let mut scan = open(
        source,
        c"capture_checkpoints",
        c"capture_checkpoints_pkey",
        snapshot,
    )?;
    let Some(row) = scan.next_back() else {
        return Ok(None);
    };
    let bytes: Vec<u8> = row.get(6, "manifest").ok_or("invalid_checkpoint")?;
    let manifest = Manifest::decode(&bytes).map_err(|_| "invalid_checkpoint")?;
    let c = Checkpoint {
        epoch: row.get(2, "epoch").ok_or("invalid_checkpoint")?,
        applied: row.get(3, "applied").ok_or("invalid_checkpoint")?,
        rows: row.get(5, "row_count").ok_or("invalid_checkpoint")?,
        manifest,
    };
    if c.epoch != c.manifest.binding.epoch as i64
        || c.applied != c.manifest.applied as i64
        || row.get::<i64>(4, "generation") != Some(c.manifest.generation as i64)
        || c.rows != i64::from(c.manifest.rows().map_err(|_| "invalid_checkpoint")?)
    {
        return Err("invalid_checkpoint");
    }
    Ok(Some(c))
}
/// Catalogs store keys and values as bigint; a value outside the column's own
/// type cannot come from the source and is rejected as corrupt. `width` is the
/// binding's catalog width, so an int2 column stored at 4 bytes is still held to
/// the int2 range.
fn narrow<V: Column>(value: i64, width: i16) -> Option<V> {
    if width == 2 && i16::try_from(value).is_err() {
        return None;
    }
    V::try_from(value).ok()
}
/// A stored value of the feed's value column. A numeric column's values are also
/// held to its precision.
fn nullable<V: Column>(
    row: &crate::native_catalog::Row<'_>,
    attribute: i16,
    name: &str,
    config: &Configuration,
) -> Checked<Option<V>> {
    if row.is_null(attribute, name, pg_sys::INT8OID) {
        Ok(None)
    } else {
        row.get::<i64>(attribute, name)
            .filter(|&v| {
                config
                    .value_scale
                    .is_none_or(|s| crate::numeric::fits(v, s))
            })
            .and_then(|v| narrow(v, config.value_width))
            .map(Some)
            .ok_or("invalid_capture_value")
    }
}
fn base<K: Column, V: Column>(
    source: &PgRelation,
    snapshot: pg_sys::Snapshot,
    h: &Head,
    config: &Configuration,
) -> Checked<ReplayState<K, V>> {
    if h.pruned != 0 {
        return Err("checkpoint_history_gap");
    }
    let mut scan = open(
        source,
        c"capture_base_rows",
        c"capture_base_source_index",
        snapshot,
    )?;
    let mut rows = Vec::new();
    rows.try_reserve_exact(h.base_rows as usize)
        .map_err(|_| "checkpoint_allocation_failed")?;
    while let Some(row) = scan.next() {
        if rows.len() >= h.base_rows as usize || row.get::<i64>(2, "epoch") != Some(h.epoch) {
            return Err("invalid_capture_base");
        }
        rows.push(Row {
            key: row
                .get::<i64>(3, "key")
                .and_then(|k| narrow(k, config.key_width))
                .ok_or("invalid_capture_base")?,
            value: nullable(&row, 4, "value", config)?,
        });
    }
    if rows.len() != h.base_rows as usize {
        return Err("invalid_capture_base");
    }
    rows.sort_unstable_by_key(|r| r.key);
    ReplayState::seed(binding(config), 0, &rows, LIMITS, keep).map_err(|_| "invalid_capture_base")
}
fn changes<K: Column, V: Column>(
    source: &PgRelation,
    snapshot: pg_sys::Snapshot,
    config: &Configuration,
    h: &Head,
    after: i64,
) -> Checked<Vec<Change<K, V>>> {
    changes_from(source, snapshot, config, h, h.pruned, after)
}
fn changes_from<K: Column, V: Column>(
    source: &PgRelation,
    snapshot: pg_sys::Snapshot,
    config: &Configuration,
    h: &Head,
    first: i64,
    after: i64,
) -> Checked<Vec<Change<K, V>>> {
    if first < h.pruned || first > after || after > h.emitted {
        return Err("checkpoint_history_gap");
    }
    let mut scan = CatalogScan::open(
        c"syntra",
        c"capture_events",
        c"capture_events_pkey",
        snapshot,
        &[
            int8_key(2, "epoch", h.epoch, Int8Comparison::Equal),
            int8_key(3, "ordinal", first, Int8Comparison::After),
            int8_key(3, "ordinal", h.emitted, Int8Comparison::Through),
        ],
    )
    .ok_or("checkpoint_metadata_missing")?;
    let mut events = Vec::new();
    events
        .try_reserve_exact((h.emitted - first) as usize)
        .map_err(|_| "checkpoint_allocation_failed")?;
    while let Some(row) = scan.next() {
        if events.len() >= 4096
            || row.get::<i64>(2, "epoch") != Some(h.epoch)
            || row.get::<pg_sys::Oid>(1, "source_relation") != Some(source.oid())
        {
            return Err("invalid_capture_events");
        }
        let ordinal = row
            .get::<i64>(3, "ordinal")
            .ok_or("invalid_capture_events")?;
        let before = row
            .get::<bool>(7, "before_present")
            .ok_or("invalid_capture_events")?;
        let old = nullable(&row, 8, "before_value", config)?;
        let present = row
            .get::<bool>(9, "after_present")
            .ok_or("invalid_capture_events")?;
        let value = nullable(&row, 10, "after_value", config)?;
        if (!before && old.is_some()) || (!present && value.is_some()) || (!before && !present) {
            return Err("invalid_capture_events");
        }
        events.push((
            ordinal,
            Change {
                key: row
                    .get::<i64>(6, "key")
                    .and_then(|k| narrow(k, config.key_width))
                    .ok_or("invalid_capture_events")?,
                before: before.then_some(old),
                after: present.then_some(value),
            },
        ));
    }
    events.sort_unstable_by_key(|e| e.0);
    if events.len() as i64 != h.emitted - first {
        return Err("checkpoint_history_gap");
    }
    for (index, (ordinal, _)) in events.iter().enumerate() {
        if *ordinal != first + index as i64 + 1 {
            return Err("checkpoint_history_gap");
        }
    }
    Ok(events
        .into_iter()
        .filter(|e| e.0 > after)
        .map(|e| e.1)
        .collect())
}
fn admission(source: &PgRelation) {
    // SAFETY: backend-local settings; retain source/schema protection and serialize
    // maintenance publishers without blocking normal capture-head updates.
    unsafe {
        if !pg_sys::superuser() {
            pgrx::ereport!(
                ERROR,
                pgrx::PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
                "checkpoint maintenance requires a superuser"
            );
        }
        if pg_sys::XactReadOnly || pg_sys::XactIsoLevel >= pg_sys::XACT_REPEATABLE_READ as i32 {
            pgrx::ereport!(
                ERROR,
                pgrx::PgSqlErrorCode::ERRCODE_FEATURE_NOT_SUPPORTED,
                "checkpoint maintenance requires a writable read committed transaction"
            );
        }
        pg_sys::LockRelationOid(source.oid(), pg_sys::AccessShareLock as i32);
        let namespace = pg_sys::get_namespace_oid(c"syntra".as_ptr(), false);
        let oid = pg_sys::get_relname_relid(c"capture_checkpoints".as_ptr(), namespace);
        if !crate::native_catalog::try_admission_lock(oid, pg_sys::ShareRowExclusiveLock as i32) {
            pgrx::ereport!(
                ERROR,
                pgrx::PgSqlErrorCode::ERRCODE_LOCK_NOT_AVAILABLE,
                "checkpoint maintenance is busy; retry after its transaction ends"
            );
        }
    }
}
/// SQL aggregates over values of either width, polling interrupts every 1,024.
fn aggregate<V: Column>(
    values: impl IntoIterator<Item = Option<V>>,
) -> Result<WideSummary, syntra_core::AggregateError> {
    let mut s = syntra_core::Int8Summary::default();
    for (index, value) in values.into_iter().enumerate() {
        if index % 1024 == 0 {
            keep();
        }
        s.push(value.map(Into::into))?;
    }
    Ok((s.rows(), s.non_nulls(), s.sum(), s.min(), s.max()))
}
/// Opening the segment validates every group, including its zone map.
fn summary<V: Column>(segment: &[u8]) -> Checked<WideSummary> {
    let column =
        syntra_storage::Segment::<V>::open(segment, keep).map_err(|_| "invalid_checkpoint")?;
    let mut groups = Vec::new();
    for group in column.groups() {
        groups.push(group.map_err(|_| "invalid_checkpoint")?);
    }
    aggregate(groups.iter().flat_map(|g| g.values())).map_err(|_| "invalid_checkpoint")
}
fn representations<K: Column, V: Column>(state: &ReplayState<K, V>, segment: &[u8]) -> Checked<()> {
    let column =
        syntra_storage::Segment::<V>::open(segment, keep).map_err(|_| "invalid_checkpoint")?;
    if column.rows() as usize != state.rows().len() {
        return Err("invalid_checkpoint");
    }
    let mut rows = state.rows().iter();
    for group in column.groups() {
        keep();
        let group = group.map_err(|_| "invalid_checkpoint")?;
        for value in group.values() {
            if rows.next().map(|r| r.value) != Some(value) {
                return Err("invalid_checkpoint");
            }
        }
    }
    if rows.next().is_some() {
        return Err("invalid_checkpoint");
    }
    Ok(())
}

// False means the unchanged persisted checkpoint was fully validated in this call.
// True means publication occurred and stored bytes must be checked before prune.
fn apply_loaded<K: Column, V: Column>(
    source: &PgRelation,
    snapshot: pg_sys::Snapshot,
    config: &Configuration,
    h: &Head,
    old: Option<Checkpoint>,
) -> bool {
    if let Some(c) = old {
        if c.epoch != h.epoch || c.applied < h.pruned || c.applied > h.emitted {
            fail("checkpoint_history_gap");
        }
        if c.manifest.binding != binding(config) {
            fail("invalid_checkpoint");
        }
        if c.applied == h.emitted {
            if c.rows != h.rows {
                fail("invalid_checkpoint");
            }
            chunks::validate::<K, V>(source, snapshot, &c.manifest).unwrap_or_else(|e| fail(e));
            return false;
        }
        let delta =
            changes::<K, V>(source, snapshot, config, h, c.applied).unwrap_or_else(|e| fail(e));
        chunks::advance(source, snapshot, h, c.manifest, &delta, 0).unwrap_or_else(|e| fail(e));
    } else {
        let state = base::<K, V>(source, snapshot, h, config).unwrap_or_else(|e| fail(e));
        let delta = changes(source, snapshot, config, h, 0).unwrap_or_else(|e| fail(e));
        chunks::seed(source, config, h, state, &delta).unwrap_or_else(|e| fail(e));
    }
    true
}

#[pg_extern(volatile, parallel_unsafe)]
fn syntra_apply_capture(source: PgRelation) -> i64 {
    admission(&source);
    let _snapshot = Snapshot::latest();
    let snapshot = crate::source::source_snapshot();
    let config = current_configuration(&source).unwrap_or_else(|e| fail(e));
    let h = head(&source, snapshot).unwrap_or_else(|e| fail(e));
    if h.epoch != config.epoch {
        fail("capture_reseeded");
    }
    let old = checkpoint(&source, snapshot).unwrap_or_else(|e| fail(e));
    with_layout!(
        config.key_width,
        config.value_width,
        fail("capture_unavailable"),
        apply_loaded(&source, snapshot, &config, &h, old)
    );
    h.emitted
}

/// Full validation of a stored checkpoint under the feed's layout.
fn validate(
    source: &PgRelation,
    snapshot: pg_sys::Snapshot,
    config: &Configuration,
    manifest: &Manifest,
) {
    with_layout!(
        config.key_width,
        config.value_width,
        Err("capture_unavailable"),
        chunks::validate(source, snapshot, manifest)
    )
    .unwrap_or_else(|e| fail(e));
}

#[pg_extern(volatile, parallel_unsafe)]
fn syntra_apply_prune_capture(
    source: PgRelation,
) -> TableIterator<'static, (name!(applied, i64), name!(pruned, i64))> {
    admission(&source);
    let _snapshot = Snapshot::latest();
    let snapshot = crate::source::source_snapshot();
    let config = current_configuration(&source).unwrap_or_else(|e| fail(e));
    let h = head(&source, snapshot).unwrap_or_else(|e| fail(e));
    if h.epoch != config.epoch {
        fail("capture_reseeded");
    }
    let old = checkpoint(&source, snapshot).unwrap_or_else(|e| fail(e));
    let published = with_layout!(
        config.key_width,
        config.value_width,
        fail("capture_unavailable"),
        apply_loaded(&source, snapshot, &config, &h, old)
    );
    if published {
        // A fresh snapshot sees this command's SPI writes. Validate actual stored
        // bytes, including changes made by catalog triggers, before deleting logs.
        let _published_snapshot = Snapshot::latest();
        let fresh = crate::source::source_snapshot();
        let c = checkpoint(&source, fresh)
            .unwrap_or_else(|e| fail(e))
            .unwrap_or_else(|| fail("checkpoint_unavailable"));
        if c.epoch != h.epoch
            || c.applied != h.emitted
            || c.rows != h.rows
            || c.manifest.binding != binding(&config)
        {
            fail("invalid_checkpoint");
        }
        validate(&source, fresh, &config, &c.manifest);
    }
    let pruned = prune_verified(&source, &h, h.emitted);
    TableIterator::new(std::iter::once((h.emitted, pruned)))
}

/// Applies pending history, then folds every partition delta into a new base, so
/// the checkpoint is stored as 0.3.0 stores it, with a version 1 manifest. Returns
/// the number of partitions folded. A downgrade to 0.3.0 needs this on every feed.
#[pg_extern(volatile, parallel_unsafe)]
fn syntra_fold_capture(source: PgRelation) -> i64 {
    admission(&source);
    let _snapshot = Snapshot::latest();
    let snapshot = crate::source::source_snapshot();
    let config = current_configuration(&source).unwrap_or_else(|e| fail(e));
    let h = head(&source, snapshot).unwrap_or_else(|e| fail(e));
    if h.epoch != config.epoch {
        fail("capture_reseeded");
    }
    let old = checkpoint(&source, snapshot).unwrap_or_else(|e| fail(e));
    with_layout!(
        config.key_width,
        config.value_width,
        fail("capture_unavailable"),
        apply_loaded(&source, snapshot, &config, &h, old)
    );
    // Read back what the apply stored, as prune does, before folding it.
    let stored = |expected_generation: Option<u64>| {
        let _published_snapshot = Snapshot::latest();
        let fresh = crate::source::source_snapshot();
        let c = checkpoint(&source, fresh)
            .unwrap_or_else(|e| fail(e))
            .unwrap_or_else(|| fail("checkpoint_unavailable"));
        if c.epoch != h.epoch
            || c.applied != h.emitted
            || c.rows != h.rows
            || c.manifest.binding != binding(&config)
            || expected_generation.is_some_and(|g| c.manifest.generation != g)
        {
            fail("invalid_checkpoint");
        }
        validate(&source, fresh, &config, &c.manifest);
        c
    };
    let c = stored(None);
    let fold = (c.manifest.chunks.iter().enumerate())
        .filter(|(_, chunk)| chunk.delta.is_some())
        .fold(0_u64, |mask, (i, _)| mask | 1_u64 << i);
    if fold == 0 {
        return 0;
    }
    let generation = c.manifest.generation + 1;
    // The snapshot guard must outlive the advance that reads through it.
    let _fold_snapshot = Snapshot::latest();
    let fresh = crate::source::source_snapshot();
    with_layout!(
        config.key_width,
        config.value_width,
        Err("capture_unavailable"),
        chunks::advance(&source, fresh, &h, c.manifest, &[], fold)
    )
    .unwrap_or_else(|e| fail(e));
    let folded = stored(Some(generation));
    if folded.manifest.chunks.iter().any(|c| c.delta.is_some()) {
        fail("invalid_checkpoint");
    }
    i64::from(fold.count_ones())
}

#[pg_extern(volatile, parallel_unsafe)]
fn syntra_prune_capture(source: PgRelation) -> i64 {
    admission(&source);
    let _snapshot = Snapshot::latest();
    let snapshot = crate::source::source_snapshot();
    let config = current_configuration(&source).unwrap_or_else(|e| fail(e));
    let h = head(&source, snapshot).unwrap_or_else(|e| fail(e));
    let c = checkpoint(&source, snapshot)
        .unwrap_or_else(|e| fail(e))
        .unwrap_or_else(|| fail("checkpoint_unavailable"));
    if h.epoch != config.epoch
        || c.epoch != h.epoch
        || c.applied < h.pruned
        || c.applied > h.emitted
    {
        fail("checkpoint_history_gap");
    }
    if c.manifest.binding != binding(&config) {
        fail("invalid_checkpoint");
    }
    validate(&source, snapshot, &config, &c.manifest);
    prune_verified(&source, &h, c.applied)
}

// Caller holds maintenance admission and has validated the durable applied state.
// Use the initial cut even if producers commit a newer head while this call runs.
// Both deletes start above the old cut: rows below it are already deleted, and an
// unbounded range would step over every one a held snapshot keeps.
fn prune_verified(source: &PgRelation, h: &Head, applied: i64) -> i64 {
    if applied == h.pruned {
        return 0;
    }
    Spi::get_one_with_args::<i64>(r#"WITH advanced AS (
        UPDATE syntra.capture_heads AS h SET pruned=$3,emitted=greatest(h.emitted,$3),live_rows=coalesce(m.live_rows,h.live_rows)
        FROM (SELECT NULL) AS anchor LEFT JOIN syntra.capture_marks AS m ON m.epoch=$2 AND m.ordinal=$3 AND m.source_relation=$1
        WHERE h.source_relation=$1 AND h.epoch=$2 AND h.pruned=$4 AND (h.emitted>=$3 OR m.ordinal IS NOT NULL) RETURNING h.epoch
    ), folded AS (
        DELETE FROM syntra.capture_marks AS m WHERE m.source_relation=$1 AND m.epoch=$2 AND m.ordinal>$4 AND m.ordinal<=$3
        AND EXISTS(SELECT epoch FROM advanced WHERE epoch=$2)
    ), removed AS (
        DELETE FROM syntra.capture_events AS e WHERE e.source_relation=$1 AND e.epoch=$2 AND e.ordinal>$4 AND e.ordinal<=$3
        AND EXISTS(SELECT epoch FROM advanced WHERE epoch=$2) RETURNING e.ordinal
    ) SELECT count(*) FROM removed"#,&[source.oid().into(),h.epoch.into(),applied.into(),h.pruned.into()])
        .unwrap_or_else(|e|pgrx::error!("{}",e)).unwrap_or_else(||fail("checkpoint prune failed"))
}

// The SQL entry point validates source access and own-XID safety before this.
pub(super) fn snapshot_summary(
    source: &PgRelation,
    snapshot: pg_sys::Snapshot,
    attribute: i16,
    value_width: i16,
    replay_tail: bool,
) -> Checked<(WideSummary, bool)> {
    let config = current_configuration(source)?;
    let h = head(source, snapshot)?;
    // The numeric reader passes width 0; a numeric feed is stored at 8 bytes.
    let numeric = value_width == 0;
    if h.epoch != config.epoch
        || h.value != attribute
        || config.value_scale.is_some() != numeric
        || (!numeric && crate::stored_width(config.value_width) != value_width)
    {
        return Err("capture_binding_changed");
    }
    with_layout!(
        config.key_width,
        config.value_width,
        Err("capture_unavailable"),
        summary_at(source, snapshot, &config, &h, replay_tail)
    )
}

fn summary_at<K: Column, V: Column>(
    source: &PgRelation,
    snapshot: pg_sys::Snapshot,
    config: &Configuration,
    h: &Head,
    replay_tail: bool,
) -> Checked<(WideSummary, bool)> {
    let c = checkpoint(source, snapshot)?.ok_or("checkpoint_unavailable")?;
    if c.epoch != h.epoch || c.applied > h.emitted || c.applied < h.pruned {
        return Err("checkpoint_history_gap");
    }
    if c.manifest.binding != binding(config) {
        return Err("invalid_checkpoint");
    }
    if c.applied == h.emitted {
        let s = chunks::summarize::<K, V>(source, snapshot, &c.manifest)?;
        if c.rows != h.rows || s.0 != c.rows {
            return Err("invalid_checkpoint");
        }
        return Ok((s, false));
    }
    if !replay_tail {
        return Err("checkpoint_lagging");
    }
    if h.emitted - c.applied > 256 {
        return Err("checkpoint_tail_limit");
    }
    let delta = changes_from::<K, V>(source, snapshot, config, h, c.applied, c.applied)?;
    let s = chunks::replay(source, snapshot, &c.manifest, &delta, h.rows)?;
    Ok((s, true))
}

/// A summary with its route and reason. `value_width` is 4 for the int4 readers,
/// which also read int2 columns, and 8 for the int8 readers.
fn read(
    source: &PgRelation,
    column: &str,
    value_width: i16,
    replay_tail: bool,
) -> (WideSummary, &'static str, &'static str) {
    let attribute = if value_width == 4 {
        crate::source::validate_column(
            source,
            column,
            &[pg_sys::INT2OID, pg_sys::INT4OID],
            "source capture requires a stored, non-generated int2 or int4 column",
        )
        .0
    } else {
        crate::source::validate_column(
            source,
            column,
            &[pg_sys::INT8OID],
            "source capture requires a stored, non-generated int8 column",
        )
        .0
    };
    let snapshot = crate::source::source_snapshot();
    // SAFETY: backend-local lookup does not assign an XID. Own-command changes
    // may be visible before capture callbacks, so their readers use the heap.
    let checked = if unsafe { pg_sys::GetCurrentFullTransactionIdIfAny().value != 0 } {
        Err("transaction_has_xid")
    } else {
        snapshot_summary(source, snapshot, attribute, value_width, replay_tail)
    };
    match checked {
        Ok((s, false)) => (s, "checkpoint", "eligible"),
        Ok((s, true)) => (s, "replay", "bounded_tail"),
        Err(reason) if value_width == 4 => (
            crate::source::widen(crate::source::heap_summary_checked(
                source,
                attribute,
                snapshot,
                syntra_exec::Int4Predicate::All,
            )),
            "heap",
            reason,
        ),
        Err(reason) => (
            crate::source::heap_int8_summary_checked(source, attribute, snapshot),
            "heap",
            reason,
        ),
    }
}

/// An int4 feed's values, and so its sum and bounds, fit the int4 outputs.
fn narrow_int4(s: WideSummary) -> crate::source::SummaryRow {
    let bound =
        |v: Option<i64>| v.map(|v| i32::try_from(v).unwrap_or_else(|_| fail("invalid_checkpoint")));
    (
        s.0,
        s.1,
        s.2.map(|v| i64::try_from(v).unwrap_or_else(|_| fail("invalid_checkpoint"))),
        bound(s.3),
        bound(s.4),
    )
}

/// Per-backend observation of the latest checkpoint chunk pass: chunks passed on,
/// stored bytes fetched and the largest amount held at once. Callers reach a pass
/// only after source or superuser checks; sizes follow from visible row counts.
#[pg_extern(volatile, parallel_unsafe)]
fn syntra_checkpoint_input_bytes() -> TableIterator<
    'static,
    (
        name!(chunks, i64),
        name!(bytes_read, i64),
        name!(peak_bytes, i64),
    ),
> {
    let (chunks, bytes, peak) = chunks::last_pass();
    TableIterator::new(std::iter::once((chunks as i64, bytes as i64, peak as i64)))
}

/// Transactions started since the oldest snapshot, slot or prepared transaction
/// that stops vacuum from removing dead versions of the capture catalogs. Retained
/// snapshots grow the head and chunk version chains until they end, so operators
/// alert on this number. It reveals only an age, never another session's data.
#[pg_extern(volatile, parallel_unsafe)]
fn syntra_cleanup_horizon_age() -> i64 {
    let (horizon, next) = unsafe {
        let namespace = pg_sys::get_namespace_oid(c"syntra".as_ptr(), true);
        let catalog = pg_sys::get_relname_relid(c"capture_heads".as_ptr(), namespace);
        let rel = pg_sys::relation_open(catalog, pg_sys::AccessShareLock as pg_sys::LOCKMODE);
        let horizon = pg_sys::GetOldestNonRemovableTransactionId(rel);
        pg_sys::relation_close(rel, pg_sys::AccessShareLock as pg_sys::LOCKMODE);
        (horizon, pg_sys::ReadNextTransactionId())
    };
    if horizon == pg_sys::TransactionId::INVALID {
        return 0;
    }
    i64::from(next.into_inner().wrapping_sub(horizon.into_inner()))
}

/// How far a source's checkpoint trails its committed feed, for alerting. Before
/// the first apply there is no checkpoint, and every retained event is pending.
/// Open to any role that can read or write the source, like the headroom.
#[pg_extern(volatile, parallel_unsafe)]
fn syntra_capture_lag(
    source: PgRelation,
) -> TableIterator<
    'static,
    (
        name!(emitted, i64),
        name!(applied, Option<i64>),
        name!(pending_events, i64),
        name!(generation, Option<i64>),
    ),
> {
    crate::capture::require_table_access(
        &source,
        "capture lag requires schema USAGE and a table privilege",
    );
    let _snapshot_guard = Snapshot::latest();
    // SAFETY: the guard above pins this MVCC snapshot through the scans.
    let snapshot = unsafe { pg_sys::GetActiveSnapshot() };
    let h = head(&source, snapshot).unwrap_or_else(|e| fail(e));
    let c = checkpoint(&source, snapshot).unwrap_or_else(|e| fail(e));
    if let Some(c) = &c {
        if c.epoch != h.epoch || c.applied < h.pruned || c.applied > h.emitted {
            fail("checkpoint_history_gap");
        }
    }
    let applied = c.as_ref().map(|c| c.applied);
    TableIterator::new(std::iter::once((
        h.emitted,
        applied,
        h.emitted - applied.unwrap_or(h.pruned),
        c.map(|c| c.manifest.generation as i64),
    )))
}

#[allow(clippy::type_complexity)]
#[pg_extern(stable, parallel_unsafe)]
fn syntra_checkpoint_int4_summary(
    source: PgRelation,
    column: &str,
) -> TableIterator<
    'static,
    (
        name!(rows, i64),
        name!(non_nulls, i64),
        name!(sum, Option<i64>),
        name!(min, Option<i32>),
        name!(max, Option<i32>),
        name!(route, &'static str),
        name!(reason, &'static str),
    ),
> {
    let (s, route, reason) = read(&source, column, 4, false);
    let s = narrow_int4(s);
    TableIterator::new(std::iter::once((s.0, s.1, s.2, s.3, s.4, route, reason)))
}

#[allow(clippy::type_complexity)]
#[pg_extern(name = "syntra_checkpoint_int4_summary", stable, parallel_unsafe)]
fn syntra_checkpoint_int4_summary_replay(
    source: PgRelation,
    column: &str,
    replay_tail: bool,
) -> TableIterator<
    'static,
    (
        name!(rows, i64),
        name!(non_nulls, i64),
        name!(sum, Option<i64>),
        name!(min, Option<i32>),
        name!(max, Option<i32>),
        name!(route, &'static str),
        name!(reason, &'static str),
    ),
> {
    let (s, route, reason) = read(&source, column, 4, replay_tail);
    let s = narrow_int4(s);
    TableIterator::new(std::iter::once((s.0, s.1, s.2, s.3, s.4, route, reason)))
}

/// The numeric counterpart of `syntra_checkpoint_int8_summary`, for a numeric(p,s)
/// column with p at most 18. Sum, min and max are numeric at the column's scale.
/// The heap route returns NaN as PostgreSQL does: a NaN makes the sum NaN, and
/// NaN sorts above every other value.
#[allow(clippy::type_complexity)]
#[pg_extern(stable, parallel_unsafe)]
fn syntra_checkpoint_numeric_summary(
    source: PgRelation,
    column: &str,
    replay_tail: default!(bool, false),
) -> TableIterator<
    'static,
    (
        name!(rows, i64),
        name!(non_nulls, i64),
        name!(sum, Option<AnyNumeric>),
        name!(min, Option<AnyNumeric>),
        name!(max, Option<AnyNumeric>),
        name!(route, &'static str),
        name!(reason, &'static str),
    ),
> {
    let attribute = crate::source::validate_column(
        &source,
        column,
        &[pg_sys::NUMERICOID],
        "source capture requires a stored, non-generated numeric column",
    )
    .0;
    let scale = crate::capture::numeric_scale(&source, attribute).unwrap_or_else(|| {
        crate::source::unsupported(
            "numeric summary requires numeric(p,s) with p from 1 to 18 and s from 0 to p",
        )
    });
    let snapshot = crate::source::source_snapshot();
    // SAFETY: as in `read`.
    let checked = if unsafe { pg_sys::GetCurrentFullTransactionIdIfAny().value != 0 } {
        Err("transaction_has_xid")
    } else {
        snapshot_summary(&source, snapshot, attribute, 0, replay_tail)
    };
    let (s, nans, route, reason) = match checked {
        Ok((s, false)) => (s, 0, "checkpoint", "eligible"),
        Ok((s, true)) => (s, 0, "replay", "bounded_tail"),
        Err(reason) => {
            let (s, nans) =
                crate::source::heap_numeric_summary_checked(&source, attribute, snapshot, scale);
            (s, nans, "heap", reason)
        }
    };
    let nan = || AnyNumeric::try_from("NaN").unwrap_or_else(|_| fail("invalid_checkpoint"));
    let value = |v: Option<i64>| v.map(|v| crate::numeric::to_numeric(i128::from(v), scale));
    let ordered = s.1 > nans;
    TableIterator::new(std::iter::once((
        s.0,
        s.1,
        if nans > 0 {
            Some(nan())
        } else {
            s.2.map(|v| crate::numeric::to_numeric(v, scale))
        },
        if ordered {
            value(s.3)
        } else {
            (nans > 0).then(nan)
        },
        if nans > 0 { Some(nan()) } else { value(s.4) },
        route,
        reason,
    )))
}

/// The int8 counterpart of `syntra_checkpoint_int4_summary`. As with PostgreSQL's
/// `sum(bigint)`, the sum is numeric because it can exceed the bigint range.
#[allow(clippy::type_complexity)]
#[pg_extern(stable, parallel_unsafe)]
fn syntra_checkpoint_int8_summary(
    source: PgRelation,
    column: &str,
    replay_tail: default!(bool, false),
) -> TableIterator<
    'static,
    (
        name!(rows, i64),
        name!(non_nulls, i64),
        name!(sum, Option<AnyNumeric>),
        name!(min, Option<i64>),
        name!(max, Option<i64>),
        name!(route, &'static str),
        name!(reason, &'static str),
    ),
> {
    let (s, route, reason) = read(&source, column, 8, replay_tail);
    TableIterator::new(std::iter::once((
        s.0,
        s.1,
        s.2.map(AnyNumeric::from),
        s.3,
        s.4,
        route,
        reason,
    )))
}
