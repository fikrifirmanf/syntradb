//! Private, bounded transactional changes. No SQL reader routes through this log.
use crate::native_catalog::{
    function_oid, guards_current, int8_key, oid_key, CatalogScan, Int8Comparison,
};
use pgrx::prelude::*;
use pgrx::spi::OwnedPreparedStatement;
use pgrx::trigger_support::{PgTriggerLevel, PgTriggerWhen};
use pgrx::{AllocatedByPostgres, PgRelation};
use std::{ffi::CStr, num::NonZeroUsize};

pub(super) fn stale() -> ! {
    pgrx::ereport!(
        ERROR,
        pgrx::PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
        "capture configuration is missing or changed; reseed before writing"
    );
}

pub(super) fn full() -> ! {
    pgrx::ereport!(
        ERROR,
        pgrx::PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
        "capture log is full or registration changed; apply and prune or reseed before writing"
    );
}

/// A deferred trigger is a constraint trigger that is deferrable and initially
/// deferred; any other trigger must be a plain, immediate one.
fn trigger_valid(
    source: &PgRelation,
    oid: pg_sys::Oid,
    procedure: pg_sys::Oid,
    kind: i16,
    deferred: bool,
) -> bool {
    // SAFETY: source is locked for the statement; all descriptor fields are
    // inspected synchronously and no catalog pointer escapes.
    unsafe {
        if source.trigdesc.is_null() {
            return false;
        }
        let desc = &*source.trigdesc;
        (0..desc.numtriggers).any(|index| {
            let t = &*desc.triggers.add(index as usize);
            t.tgoid == oid
                && t.tgfoid == procedure
                && t.tgtype == kind
                && crate::catalog_char(t.tgenabled) == b'A'
                && !t.tgisinternal
                && !t.tgisclone
                && t.tgnargs == 0
                && t.tgnattr == 0
                && t.tgqual.is_null()
                && (t.tgconstraint != pg_sys::InvalidOid) == deferred
                && t.tgdeferrable == deferred
                && t.tginitdeferred == deferred
                && t.tgoldtable.is_null()
                && t.tgnewtable.is_null()
        })
    }
}

/// The row trigger is deferred from 0.6.0. The immediate trigger of 0.5.0 also
/// captures every change, firing per statement and holding the head lock longer,
/// and stays valid until ALTER EXTENSION UPDATE replaces it.
fn row_trigger_valid(source: &PgRelation, oid: pg_sys::Oid, procedure: pg_sys::Oid) -> bool {
    trigger_valid(source, oid, procedure, 29, true)
        || trigger_valid(source, oid, procedure, 29, false)
}

/// Deletes a source's appended checkpoint deltas. Before `ALTER EXTENSION
/// UPDATE` creates their table there are none.
fn delete_appended_deltas(source: pg_sys::Oid) {
    if crate::native_catalog::relation_exists(c"syntra", c"capture_chunk_deltas") {
        Spi::run_with_args(
            "DELETE FROM syntra.capture_chunk_deltas WHERE source_relation=$1",
            &[source.into()],
        )
        .unwrap_or_else(|e| pgrx::error!("{}", e));
    }
}
fn install(
    source: &PgRelation,
    name: &CStr,
    function: &'static CStr,
    events: i16,
    row: bool,
    before: bool,
    deferred: bool,
) -> pg_sys::Oid {
    let procedure =
        function_oid(function).unwrap_or_else(|| pgrx::error!("capture function is missing"));
    let kind = events
        | i16::from(row)
        | if before {
            pg_sys::TRIGGER_TYPE_BEFORE as i16
        } else {
            0
        };
    // SAFETY: caller holds source AccessExclusiveLock. Identifiers are static,
    // source is nonpartitioned, and PostgreSQL owns all statement allocations.
    unsafe {
        let existing = pg_sys::get_trigger_oid(source.oid(), name.as_ptr(), true);
        if existing != pg_sys::InvalidOid {
            // A deferred trigger may also be the immediate one 0.5.0 installed,
            // until ALTER EXTENSION UPDATE replaces it; it captures the same rows.
            if !trigger_valid(source, existing, procedure, kind, deferred)
                && !(deferred && trigger_valid(source, existing, procedure, kind, false))
            {
                pgrx::error!("existing capture trigger is incompatible; repair it before seeding");
            }
            return existing;
        }
        let mut statement =
            PgBox::<pg_sys::CreateTrigStmt>::alloc_node(pg_sys::NodeTag::T_CreateTrigStmt);
        statement.trigname = pg_sys::pstrdup(name.as_ptr());
        statement.events = events;
        statement.row = row;
        statement.timing = if before {
            pg_sys::TRIGGER_TYPE_BEFORE as i16
        } else {
            0
        };
        statement.isconstraint = deferred;
        statement.deferrable = deferred;
        statement.initdeferred = deferred;
        pg_sys::CreateTriggerFiringOn(
            statement.as_ptr(),
            c"SyntraDB bounded capture".as_ptr(),
            source.oid(),
            pg_sys::InvalidOid,
            pg_sys::InvalidOid,
            pg_sys::InvalidOid,
            procedure,
            pg_sys::InvalidOid,
            std::ptr::null_mut(),
            false,
            false,
            pg_sys::TRIGGER_FIRES_ALWAYS as std::ffi::c_char,
        )
        .objectId
    }
}

pub(super) struct Snapshot;
impl Snapshot {
    pub(super) fn latest() -> Self {
        // SAFETY: backend main thread; the caller holds its source relation lock.
        // Push copies/pins the snapshot until this guard is dropped.
        unsafe {
            pg_sys::PushActiveSnapshot(pg_sys::GetLatestSnapshot());
        }
        Self
    }
    fn text(&self) -> String {
        // SAFETY: this guard holds the active native snapshot. These builtins
        // serialize it without SPI and the text is copied before any pop.
        unsafe {
            let datum = pgrx::direct_function_call_as_datum(pg_sys::pg_current_snapshot, &[])
                .unwrap_or_else(|| pgrx::error!("cannot obtain capture snapshot"));
            pgrx::direct_function_call::<&CStr>(pg_sys::pg_snapshot_out, &[Some(datum)])
                .unwrap_or_else(|| pgrx::error!("cannot describe capture snapshot"))
                .to_string_lossy()
                .into_owned()
        }
    }
}
impl Drop for Snapshot {
    fn drop(&mut self) {
        // SAFETY: exactly one matching push; no callback retains its pointer.
        unsafe {
            pg_sys::PopActiveSnapshot();
        }
    }
}

#[pg_extern(volatile, parallel_unsafe)]
fn syntra_seed_capture(
    source: PgRelation,
    key_column: &str,
    value_column: &str,
    max_events: i32,
) -> i64 {
    // SAFETY: backend-local privilege and isolation settings.
    unsafe {
        if !pg_sys::superuser() {
            pgrx::ereport!(
                ERROR,
                pgrx::PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
                "capture seeding currently requires a superuser"
            );
        }
        if pg_sys::XactReadOnly || pg_sys::XactIsoLevel >= pg_sys::XACT_REPEATABLE_READ as i32 {
            pgrx::ereport!(
                ERROR,
                pgrx::PgSqlErrorCode::ERRCODE_FEATURE_NOT_SUPPORTED,
                "capture seeding requires a writable read committed transaction"
            );
        }
        // Retain the exclusive lock through commit. Reject an active source
        // executor: installing a trigger mid-statement could miss remaining rows.
        pg_sys::LockRelationOid(source.oid(), pg_sys::AccessExclusiveLock as i32);
        pg_sys::CheckTableNotInUse(source.as_ptr(), c"seed capture".as_ptr());
    }
    if !(1..=4096).contains(&max_events) {
        pgrx::ereport!(
            ERROR,
            pgrx::PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "capture event limit must be between 1 and 4096"
        );
    }
    let (key, key_type) = crate::source::validate_column(
        &source,
        key_column,
        &[pg_sys::INT2OID, pg_sys::INT4OID, pg_sys::INT8OID],
        "capture requires stored, non-generated int2, int4 or int8 key and value columns",
    );
    let (value, value_type) = crate::source::validate_column(
        &source,
        value_column,
        &[
            pg_sys::INT2OID,
            pg_sys::INT4OID,
            pg_sys::INT8OID,
            pg_sys::NUMERICOID,
        ],
        "capture requires a stored, non-generated int2, int4, int8 or numeric(p,s) value column",
    );
    let value_scale = (value_type == pg_sys::NUMERICOID).then(|| {
        numeric_scale(&source, value).unwrap_or_else(|| {
            crate::source::unsupported(
                "capture requires numeric(p,s) with p from 1 to 18 and s from 0 to p",
            )
        })
    });
    let (key_width, value_width) = (
        crate::source::width(key_type),
        crate::source::width(value_type),
    );
    // SAFETY: source exclusive lock protects all relation flags.
    if unsafe {
        crate::catalog_char((*source.rd_rel).relpersistence) != pg_sys::RELPERSISTENCE_PERMANENT
            || (*source.rd_rel).relhasrules
            || pg_sys::has_superclass(source.oid())
    } || matches!(source.namespace(), "syntra" | "pg_catalog" | "pg_toast")
    {
        pgrx::ereport!(
            ERROR,
            pgrx::PgSqlErrorCode::ERRCODE_FEATURE_NOT_SUPPORTED,
            "capture requires a permanent heap without inheritance or rewrite rules"
        );
    }
    let primary = Spi::get_one_with_args::<bool>(
        r#"SELECT count(*)=1 FROM pg_catalog.pg_index AS i
        JOIN pg_catalog.pg_constraint AS c ON c.conindid=i.indexrelid AND c.contype='p'
        WHERE i.indrelid=$1 AND i.indisprimary AND i.indisvalid AND i.indisready
        AND i.indisunique AND i.indimmediate AND i.indnkeyatts=1 AND i.indkey[0]=$2
        AND i.indpred IS NULL AND i.indexprs IS NULL AND NOT c.condeferrable"#,
        &[source.oid().into(), key.into()],
    )
    .unwrap_or_else(|e| pgrx::error!("{}", e));
    if primary != Some(true) {
        pgrx::ereport!(
            ERROR,
            pgrx::PgSqlErrorCode::ERRCODE_FEATURE_NOT_SUPPORTED,
            "capture requires a single nondeferrable int4 or int8 primary key"
        );
    }
    // Nested writes from earlier AFTER ROW or RI triggers can reverse callback
    // order relative to actual tuple changes. Reject that scope until capture
    // has a tested ordering protocol for arbitrary trigger interactions.
    let no_other_triggers=Spi::get_one_with_args::<bool>("SELECT count(*)=0 FROM pg_catalog.pg_trigger WHERE tgrelid=$1 AND tgname NOT IN ('_syntra_source_changed','_syntra_capture_row','_syntra_capture_no_truncate')",&[source.oid().into()])
        .unwrap_or_else(|e|pgrx::error!("{}",e));
    if no_other_triggers != Some(true) {
        pgrx::ereport!(
            ERROR,
            pgrx::PgSqlErrorCode::ERRCODE_FEATURE_NOT_SUPPORTED,
            "capture does not support other source triggers or foreign-key triggers"
        );
    }
    // Nonqueueing, transaction-level admission bounds the global live feed count;
    // it retries briefly so a vacuum of the catalog does not fail setup.
    // This experimental setup lock can conflict with writers on other feeds.
    unsafe {
        let namespace = pg_sys::get_namespace_oid(c"syntra".as_ptr(), false);
        let catalog = pg_sys::get_relname_relid(c"capture_heads".as_ptr(), namespace);
        if !crate::native_catalog::try_admission_lock(catalog, pg_sys::ShareRowExclusiveLock as i32)
        {
            pgrx::ereport!(
                ERROR,
                pgrx::PgSqlErrorCode::ERRCODE_LOCK_NOT_AVAILABLE,
                "capture setup is busy; retry after active capture transactions end"
            );
        }
    }
    let _tracking_epoch = Spi::get_one_with_args::<i64>(
        "SELECT public.syntra_track_source($1::regclass,$2)",
        &[source.oid().into(), value_column.into()],
    )
    .unwrap_or_else(|e| pgrx::error!("{}", e))
    .unwrap_or_else(|| pgrx::error!("capture source registration failed"));
    // Refresh this backend's relcache after native trigger installation.
    // SAFETY: this backend already holds the source exclusive lock.
    let fresh = unsafe { PgRelation::with_lock(source.oid(), pg_sys::AccessShareLock as i32) };
    // The row trigger fires at commit, so the head-row lock it takes is held only
    // through commit instead of from the first captured change.
    let row_trigger = install(
        &fresh,
        c"_syntra_capture_row",
        c"public.syntra_capture_row()",
        28,
        true,
        false,
        true,
    );
    // SAFETY: source exclusive lock is still held; reopen reloads trigger metadata.
    let fresh = unsafe { PgRelation::with_lock(source.oid(), pg_sys::AccessShareLock as i32) };
    let truncate_trigger = install(
        &fresh,
        c"_syntra_capture_no_truncate",
        c"public.syntra_capture_no_truncate()",
        32,
        false,
        true,
        false,
    );
    let snapshot = Snapshot::latest();
    let snapshot_text = snapshot.text();
    // SAFETY: the seed guard pins the exact snapshot used for the source scan.
    let base_command = unsafe { i64::from((*pg_sys::GetActiveSnapshot()).curcid) };
    let rows = crate::source::keyed_seed(
        &source,
        (key, key_width),
        (value, value_width),
        value_scale,
        crate::source::source_snapshot(),
    );
    let row_count = rows.len() as i64;
    let identity = crate::live::identity(&source, value);
    delete_appended_deltas(source.oid());
    Spi::run_with_args("DELETE FROM syntra.capture_chunks WHERE source_relation=$1; DELETE FROM syntra.capture_checkpoints WHERE source_relation=$1; DELETE FROM syntra.capture_events WHERE source_relation=$1; DELETE FROM syntra.capture_marks WHERE source_relation=$1; DELETE FROM syntra.capture_base_rows WHERE source_relation=$1; DELETE FROM syntra.capture_heads WHERE source_relation=$1",&[source.oid().into()])
        .unwrap_or_else(|e|pgrx::error!("{}",e));
    let epoch=Spi::get_one_with_args::<i64>(r#"WITH seeded AS (INSERT INTO syntra.capture_heads
        (source_relation,tracking_epoch,key_attribute,value_attribute,row_trigger,truncate_trigger,max_events,source_identity,base_snapshot,base_rows,base_command,live_rows)
        SELECT $1,s.epoch,$2,$3,$4,$5,$6,$7,$8,$9,$10,$9 FROM syntra.tracked_sources AS s
        WHERE s.source_relation=$1 AND (SELECT count(*) FROM syntra.capture_heads)<16 RETURNING epoch)
        SELECT max(epoch) FROM seeded"#,
        &[source.oid().into(),key.into(),value.into(),row_trigger.into(),truncate_trigger.into(),max_events.into(),
            identity.as_str().into(),snapshot_text.as_str().into(),row_count.into(),base_command.into()])
        .unwrap_or_else(|e|pgrx::error!("{}",e)).unwrap_or_else(||{
            pgrx::ereport!(ERROR,pgrx::PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,"capture limit of 16 sources reached");
        });
    Spi::run_with_args("INSERT INTO syntra.capture_bindings(source_relation,epoch,tracking_epoch,key_attribute,value_attribute,row_trigger,truncate_trigger,source_identity,key_width,value_width) SELECT source_relation,epoch,tracking_epoch,key_attribute,value_attribute,row_trigger,truncate_trigger,source_identity,$3,$4 FROM syntra.capture_heads WHERE source_relation=$1 AND epoch=$2",&[source.oid().into(),epoch.into(),crate::stored_width(key_width).into(),crate::stored_width(value_width).into()])
        .unwrap_or_else(|e|pgrx::error!("{}",e));
    let mut keys = Vec::new();
    let mut values = Vec::new();
    keys.try_reserve_exact(rows.len()).unwrap_or_else(|_| {
        pgrx::ereport!(
            ERROR,
            pgrx::PgSqlErrorCode::ERRCODE_OUT_OF_MEMORY,
            "cannot allocate capture keys"
        );
    });
    values.try_reserve_exact(rows.len()).unwrap_or_else(|_| {
        pgrx::ereport!(
            ERROR,
            pgrx::PgSqlErrorCode::ERRCODE_OUT_OF_MEMORY,
            "cannot allocate capture values"
        );
    });
    for row in &rows {
        keys.push(row.0);
        values.push(row.1);
    }
    Spi::run_with_args("INSERT INTO syntra.capture_base_rows(source_relation,epoch,key,value) SELECT $1,$2,r.key,r.value FROM unnest($3::int8[],$4::int8[]) AS r(key,value)",
        &[source.oid().into(),epoch.into(),keys.into(),values.into()]).unwrap_or_else(|e|pgrx::error!("{}",e));
    drop(snapshot);
    epoch
}

pub(super) struct Configuration {
    pub(super) key: i16,
    pub(super) value: i16,
    /// Column type widths in bytes, 2, 4 or 8. The catalog stores 4 for an int2
    /// column; `stored_width` gives the width that chooses the encoding layout.
    pub(super) key_width: i16,
    pub(super) value_width: i16,
    /// Precision and scale of a numeric value column, stored at 8 bytes as the
    /// value times 10^scale; None for integer columns. From the live column type.
    pub(super) value_scale: Option<crate::numeric::Scale>,
    pub(super) epoch: i64,
    pub(super) tracking_epoch: i64,
    pub(super) row: pg_sys::Oid,
    pub(super) truncate: pg_sys::Oid,
    pub(super) identity: String,
}

#[pg_extern(volatile, parallel_unsafe)]
fn syntra_stop_capture(source: PgRelation) -> bool {
    // SAFETY: backend-local settings and relation lock APIs. Native dependency
    // deletion below is authorized only for explicit admin capture retirement.
    unsafe {
        if !pg_sys::superuser() {
            pgrx::ereport!(
                ERROR,
                pgrx::PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
                "capture retirement currently requires a superuser"
            );
        }
        if pg_sys::XactReadOnly || pg_sys::XactIsoLevel >= pg_sys::XACT_REPEATABLE_READ as i32 {
            pgrx::ereport!(
                ERROR,
                pgrx::PgSqlErrorCode::ERRCODE_FEATURE_NOT_SUPPORTED,
                "capture retirement requires a writable read committed transaction"
            );
        }
        pg_sys::LockRelationOid(source.oid(), pg_sys::AccessExclusiveLock as i32);
        pg_sys::CheckTableNotInUse(source.as_ptr(), c"stop capture".as_ptr());
    }
    let row_function = function_oid(c"public.syntra_capture_row()").unwrap_or_else(|| stale());
    let truncate_function =
        function_oid(c"public.syntra_capture_no_truncate()").unwrap_or_else(|| stale());
    let mut triggers = Vec::new();
    // SAFETY: inspect and copy all OIDs before deletion invalidates the relcache.
    // Only this source's triggers using our two capture functions are selected.
    unsafe {
        if !source.trigdesc.is_null() {
            let desc = &*source.trigdesc;
            for index in 0..desc.numtriggers {
                let trigger = &*desc.triggers.add(index as usize);
                if trigger.tgfoid == row_function || trigger.tgfoid == truncate_function {
                    triggers.push(trigger.tgoid);
                }
            }
        }
        for oid in &triggers {
            let address = pg_sys::ObjectAddress {
                classId: pg_sys::TriggerRelationId,
                objectId: *oid,
                objectSubId: 0,
            };
            pg_sys::performDeletion(&address, pg_sys::DropBehavior::DROP_RESTRICT, 0);
        }
    }
    delete_appended_deltas(source.oid());
    Spi::run_with_args("DELETE FROM syntra.capture_chunks WHERE source_relation=$1; DELETE FROM syntra.capture_checkpoints WHERE source_relation=$1; DELETE FROM syntra.capture_events WHERE source_relation=$1; DELETE FROM syntra.capture_marks WHERE source_relation=$1; DELETE FROM syntra.capture_base_rows WHERE source_relation=$1",&[source.oid().into()])
        .unwrap_or_else(|e|pgrx::error!("{}",e));
    let removed=Spi::get_one_with_args::<i64>("WITH removed AS (DELETE FROM syntra.capture_heads WHERE source_relation=$1 RETURNING epoch) SELECT count(*) FROM removed",&[source.oid().into()])
        .unwrap_or_else(|e|pgrx::error!("{}",e));
    let changed = removed == Some(1) || !triggers.is_empty();
    if changed {
        Spi::run_with_args(
            "UPDATE syntra.tracked_sources SET epoch=DEFAULT WHERE source_relation=$1",
            &[source.oid().into()],
        )
        .unwrap_or_else(|e| pgrx::error!("{}", e));
    }
    changed
}
/// Monitoring reads are open to any role that can read or write the source.
pub(super) fn require_table_access(source: &PgRelation, denied: &'static str) {
    if !has_table_access(source.oid(), source.namespace_oid()) {
        pgrx::ereport!(
            ERROR,
            pgrx::PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
            denied
        );
    }
}

/// Schema USAGE plus any of SELECT, INSERT, UPDATE or DELETE on the relation.
fn has_table_access(relation: pg_sys::Oid, namespace: pg_sys::Oid) -> bool {
    // SAFETY: backend-local ACL lookups; callers pass an existing relation and its schema.
    unsafe {
        let role = pg_sys::GetUserId();
        let any = u64::from(
            pg_sys::ACL_SELECT | pg_sys::ACL_INSERT | pg_sys::ACL_UPDATE | pg_sys::ACL_DELETE,
        );
        pg_sys::object_aclcheck(
            pg_sys::NamespaceRelationId,
            namespace,
            role,
            pg_sys::ACL_USAGE.into(),
        ) == pg_sys::AclResult::ACLCHECK_OK
            && pg_sys::pg_class_aclmask(relation, role, any, pg_sys::AclMaskHow::ACLMASK_ANY) != 0
    }
}

/// Captured sources the caller could pass to `syntra_capture_lag` or
/// `syntra_capture_headroom`. Sources the caller has no privilege on are left out,
/// so the list reveals nothing those functions would not.
#[pg_extern(volatile, parallel_unsafe)]
fn syntra_captured_sources() -> SetOfIterator<'static, pg_sys::Oid> {
    let _snapshot_guard = Snapshot::latest();
    // SAFETY: the guard above pins this MVCC snapshot through the scan.
    let snapshot = unsafe { pg_sys::GetActiveSnapshot() };
    let mut scan = CatalogScan::open(
        c"syntra",
        c"capture_heads",
        c"capture_heads_pkey",
        snapshot,
        &[],
    )
    .unwrap_or_else(|| stale());
    let mut sources = Vec::new();
    while let Some(h) = scan.next() {
        let source = h
            .get::<pg_sys::Oid>(1, "source_relation")
            .unwrap_or_else(|| stale());
        // SAFETY: catalog lookup by OID; a dropped relation returns InvalidOid.
        let namespace = unsafe { pg_sys::get_rel_namespace(source) };
        if namespace != pg_sys::InvalidOid && has_table_access(source, namespace) {
            sources.push(source);
        }
    }
    SetOfIterator::new(sources)
}

/// Advisory room left in a captured source's event feed, for producers that want
/// to slow down before the hard "capture log is full" error. Counts committed
/// events only: concurrent open transactions can still use the free events, so it
/// is a signal, not a reservation. Any role that can read or write the source may
/// ask; the private head row is read natively because roles have no catalog access.
#[pg_extern(volatile, parallel_unsafe)]
fn syntra_capture_headroom(
    source: PgRelation,
) -> TableIterator<
    'static,
    (
        name!(retained_events, i64),
        name!(max_events, i32),
        name!(free_events, i64),
        name!(live_rows, i64),
    ),
> {
    require_table_access(
        &source,
        "capture headroom requires schema USAGE and a table privilege",
    );
    let _snapshot_guard = Snapshot::latest();
    // SAFETY: the guard above pins this MVCC snapshot through the scan.
    let snapshot = unsafe { pg_sys::GetActiveSnapshot() };
    let mut scan = CatalogScan::open(
        c"syntra",
        c"capture_heads",
        c"capture_heads_pkey",
        snapshot,
        &[oid_key(1, "source_relation", source.oid())],
    )
    .unwrap_or_else(|| stale());
    let h = scan.next_back().unwrap_or_else(|| stale());
    let read = |attribute, name| h.get::<i64>(attribute, name).unwrap_or_else(|| stale());
    let max_events = h.get::<i32>(8, "max_events").unwrap_or_else(|| stale());
    let (epoch, pruned) = (read(2, "epoch"), read(15, "pruned"));
    let (emitted, live_rows) = position(
        &source,
        snapshot,
        epoch,
        read(9, "emitted"),
        read(16, "live_rows"),
    )
    .unwrap_or_else(|| stale());
    let retained = emitted - pruned;
    if pruned < 0 || retained < 0 || retained > i64::from(max_events) {
        stale();
    }
    TableIterator::new(std::iter::once((
        retained,
        max_events,
        i64::from(max_events) - retained,
        live_rows,
    )))
}

pub(super) fn current_configuration(
    source: &PgRelation,
) -> std::result::Result<Configuration, &'static str> {
    // Configuration must be current even when the writing transaction has an old
    // repeatable-read snapshot. Never use its stale tracked epoch as DDL proof.
    let _snapshot_guard = Snapshot::latest();
    // SAFETY: the guard above pins this MVCC snapshot through both catalog scans.
    let snapshot = unsafe { pg_sys::GetActiveSnapshot() };
    let mut scan = CatalogScan::open(
        c"syntra",
        c"capture_bindings",
        c"capture_bindings_pkey",
        snapshot,
        &[oid_key(1, "source_relation", source.oid())],
    )
    .ok_or("capture_unavailable")?;
    let h = scan.next_back().ok_or("capture_unavailable")?;
    let config = Configuration {
        key: h.get(4, "key_attribute").ok_or("capture_unavailable")?,
        value: h.get(5, "value_attribute").ok_or("capture_unavailable")?,
        epoch: h.get(2, "epoch").ok_or("capture_unavailable")?,
        tracking_epoch: h.get(3, "tracking_epoch").ok_or("capture_unavailable")?,
        row: h.get(6, "row_trigger").ok_or("capture_unavailable")?,
        truncate: h.get(7, "truncate_trigger").ok_or("capture_unavailable")?,
        identity: h.get(8, "source_identity").ok_or("capture_unavailable")?,
        key_width: h.get(9, "key_width").ok_or("capture_unavailable")?,
        value_width: h.get(10, "value_width").ok_or("capture_unavailable")?,
        value_scale: None,
    };
    drop(scan);
    let mut tracked = CatalogScan::open(
        c"syntra",
        c"tracked_sources",
        c"tracked_sources_pkey",
        snapshot,
        &[oid_key(1, "source_relation", source.oid())],
    )
    .ok_or("capture_unavailable")?;
    let h = tracked.next().ok_or("capture_unavailable")?;
    let statement_guard = h
        .get::<pg_sys::Oid>(2, "trigger_oid")
        .ok_or("capture_unavailable")?;
    if h.get::<i64>(3, "epoch") != Some(config.tracking_epoch)
        || !guards_current(source, statement_guard)
        || unsafe { source.trigdesc.is_null() || (*source.trigdesc).numtriggers != 3 }
        || config.identity != crate::live::identity(source, config.value)
        || !row_trigger_valid(
            source,
            config.row,
            function_oid(c"public.syntra_capture_row()").ok_or("capture_unavailable")?,
        )
        || !trigger_valid(
            source,
            config.truncate,
            function_oid(c"public.syntra_capture_no_truncate()").ok_or("capture_unavailable")?,
            34,
            false,
        )
    {
        return Err("capture_unavailable");
    }
    // The catalog records stored widths, 4 or 8. An int2 column is stored at 4
    // bytes, so its own width, 2, comes from the live column type; changing that
    // type rewrites the table, which the identity check above already refuses.
    // A numeric value column takes its precision and scale from the same type.
    let descriptor = source.tuple_desc();
    let column_width = |number: i16, stored: i16| {
        descriptor
            .iter()
            .find(|a| a.attnum == number && !a.attisdropped)
            .and_then(|a| match (stored, a.atttypid) {
                (4, pg_sys::INT2OID) => Some(2),
                (4, pg_sys::INT4OID) => Some(4),
                (8, pg_sys::INT8OID | pg_sys::NUMERICOID) => Some(8),
                _ => None,
            })
    };
    let key_is_integer = descriptor
        .iter()
        .any(|a| a.attnum == config.key && a.atttypid != pg_sys::NUMERICOID);
    let mut config = config;
    match (
        column_width(config.key, config.key_width).filter(|_| key_is_integer),
        column_width(config.value, config.value_width),
    ) {
        (Some(key), Some(value)) => (config.key_width, config.value_width) = (key, value),
        _ => return Err("capture_unavailable"),
    }
    if descriptor
        .iter()
        .any(|a| a.attnum == config.value && a.atttypid == pg_sys::NUMERICOID)
    {
        config.value_scale =
            Some(numeric_scale(source, config.value).ok_or("capture_unavailable")?);
    }
    if unsafe {
        // A callback can arrive after source DDL changed its supported scope.
        // Check current relation flags before extracting tuple attributes.
        (*source.rd_rel).relrowsecurity
            || (*source.rd_rel).relhasrules
            || (*source.rd_rel).relhassubclass
            || (*source.rd_rel).relispartition
            || crate::catalog_char((*source.rd_rel).relpersistence)
                != pg_sys::RELPERSISTENCE_PERMANENT
            || pg_sys::has_superclass(source.oid())
    } {
        return Err("capture_unavailable");
    }
    Ok(config)
}

fn configuration(source: &PgRelation, trigger: pg_sys::Oid) -> Configuration {
    let config = current_configuration(source).unwrap_or_else(|_| stale());
    if trigger != config.row {
        stale();
    }
    config
}

/// Precision and scale of numeric attribute `attribute`, when capture supports them.
pub(super) fn numeric_scale(source: &PgRelation, attribute: i16) -> Option<crate::numeric::Scale> {
    source
        .tuple_desc()
        .iter()
        .find(|a| a.attnum == attribute && a.atttypid == pg_sys::NUMERICOID)
        .and_then(|a| crate::numeric::from_typmod(a.atttypmod))
}

/// One integer attribute of a captured tuple at the feed's width, as int8.
fn column(tuple: &PgHeapTuple<'_, AllocatedByPostgres>, attribute: i16, width: i16) -> Option<i64> {
    let index = NonZeroUsize::new(attribute as usize).unwrap_or_else(|| stale());
    let value = match width {
        2 => tuple.get_by_index::<i16>(index).map(|v| v.map(i64::from)),
        4 => tuple.get_by_index::<i32>(index).map(|v| v.map(i64::from)),
        8 => tuple.get_by_index::<i64>(index),
        _ => stale(),
    };
    value.unwrap_or_else(|e| pgrx::error!("{}", e))
}

fn pair(
    tuple: Option<PgHeapTuple<'_, AllocatedByPostgres>>,
    config: &Configuration,
) -> Option<(i64, Option<i64>)> {
    tuple.map(|tuple| {
        let key = column(&tuple, config.key, config.key_width).unwrap_or_else(|| stale());
        let value = match config.value_scale {
            None => column(&tuple, config.value, config.value_width),
            Some(scale) => {
                let index = NonZeroUsize::new(config.value as usize).unwrap_or_else(|| stale());
                tuple
                    .get_by_index::<pgrx::AnyNumeric>(index)
                    .unwrap_or_else(|e| pgrx::error!("{}", e))
                    .map(|v| {
                        crate::numeric::to_stored(&v, scale)
                            .unwrap_or_else(|e| crate::source::unsupported(e))
                    })
            }
        };
        (key, value)
    })
}

/// The feed's position under `snapshot`: the newest mark above the head's folded
/// floor, or the floor itself when every mark has been pruned. Returns the last
/// reserved ordinal and the live row count after it; None on a foreign mark.
pub(super) fn position(
    source: &PgRelation,
    snapshot: pg_sys::Snapshot,
    epoch: i64,
    floor: i64,
    floor_rows: i64,
) -> Option<(i64, i64)> {
    let mut scan = CatalogScan::open(
        c"syntra",
        c"capture_marks",
        c"capture_marks_pkey",
        snapshot,
        &[
            int8_key(2, "epoch", epoch, Int8Comparison::Equal),
            int8_key(3, "ordinal", floor, Int8Comparison::After),
        ],
    )?;
    let Some(row) = scan.next_back() else {
        return Some((floor, floor_rows));
    };
    if row.get::<pg_sys::Oid>(1, "source_relation") != Some(source.oid())
        || row.get::<i64>(2, "epoch") != Some(epoch)
    {
        return None;
    }
    Some((row.get(3, "ordinal")?, row.get(4, "live_rows")?))
}

/// The feed position seen at `snapshot`, when the source is captured in tracking
/// epoch `tracking_epoch`; None when it is not. Seeding and retirement each start
/// a new tracking epoch, so within one epoch every committed row change appends
/// a mark. Ordinals follow commit order, so a snapshot sees a prefix of them and
/// the position changes with every committed change it can see.
pub(super) fn feed_position(
    source: &PgRelation,
    snapshot: pg_sys::Snapshot,
    tracking_epoch: i64,
) -> Result<Option<i64>, &'static str> {
    let mut scan = CatalogScan::open(
        c"syntra",
        c"capture_heads",
        c"capture_heads_pkey",
        snapshot,
        &[oid_key(1, "source_relation", source.oid())],
    )
    .ok_or("metadata_unavailable")?;
    let Some(row) = scan.next() else {
        return Ok(None);
    };
    if row.get::<i64>(3, "tracking_epoch") != Some(tracking_epoch) {
        return Ok(None);
    }
    let (Some(epoch), Some(emitted), Some(rows)) = (
        row.get::<i64>(2, "epoch"),
        row.get::<i64>(9, "emitted"),
        row.get::<i64>(16, "live_rows"),
    ) else {
        return Err("metadata_invalid");
    };
    if scan.next().is_some() {
        return Err("metadata_invalid");
    }
    drop(scan);
    position(source, snapshot, epoch, emitted, rows)
        .map(|(position, _)| Some(position))
        .ok_or("metadata_invalid")
}

/// Parsed and planned once per backend. The trigger fires for every changed row,
/// and parsing plus planning its two statements each time cost about 0.3 ms of
/// roughly 0.7 ms per firing (benchmarks/hold-control-linux-2026-11-01.md). The
/// kept plans are revalidated by the plan cache when the catalogs change. The
/// statements are leaked on purpose: freeing a plan during backend exit would
/// touch memory that is already gone.
fn kept_plan(
    slot: &'static std::thread::LocalKey<std::cell::Cell<Option<&'static OwnedPreparedStatement>>>,
    query: &str,
    types: &[PgOid],
) -> Result<&'static OwnedPreparedStatement, pgrx::spi::Error> {
    if let Some(plan) = slot.with(|cell| cell.get()) {
        return Ok(plan);
    }
    let plan: &'static OwnedPreparedStatement = Spi::connect_mut(|client| {
        client
            .prepare_mut(query, types)
            .map(|prepared| &*Box::leak(Box::new(prepared.keep())))
    })?;
    slot.with(|cell| cell.set(Some(plan)));
    Ok(plan)
}
thread_local! {
    static HEAD_PLAN: std::cell::Cell<Option<&'static OwnedPreparedStatement>> = const { std::cell::Cell::new(None) };
    static APPEND_PLAN: std::cell::Cell<Option<&'static OwnedPreparedStatement>> = const { std::cell::Cell::new(None) };
}
fn head_plan() -> Result<&'static OwnedPreparedStatement, pgrx::spi::Error> {
    kept_plan(
        &HEAD_PLAN,
        r#"SELECT h.emitted,h.pruned+h.max_events,h.live_rows FROM syntra.capture_heads AS h
        JOIN syntra.tracked_sources AS s ON s.source_relation=h.source_relation AND s.epoch=h.tracking_epoch
        WHERE h.source_relation=$1 AND h.epoch=$2 AND h.row_trigger=$3 FOR NO KEY UPDATE OF h"#,
        &[
            PgOid::BuiltIn(PgBuiltInOids::OIDOID),
            PgOid::BuiltIn(PgBuiltInOids::INT8OID),
            PgOid::BuiltIn(PgBuiltInOids::OIDOID),
        ],
    )
}
pub(super) fn append_plan() -> Result<&'static OwnedPreparedStatement, pgrx::spi::Error> {
    kept_plan(
        &APPEND_PLAN,
        r#"WITH inserted AS (
        INSERT INTO syntra.capture_events(source_relation,epoch,ordinal,origin_xid,command_id,key,before_present,before_value,after_present,after_value)
        SELECT $1,$2,$3+e.position,pg_catalog.pg_current_xact_id(),e.command,e.key,e.bp,e.bv,e.ap,e.av
        FROM unnest($4::int8[],$5::boolean[],$6::int8[],$7::boolean[],$8::int8[],$9::int8[])
        WITH ORDINALITY AS e(key,bp,bv,ap,av,command,position) RETURNING ordinal
    ), marked AS (
        INSERT INTO syntra.capture_marks(source_relation,epoch,ordinal,live_rows) VALUES($1,$2,$3+$10,$11)
    ) SELECT count(*) FROM inserted"#,
        &[
            PgOid::BuiltIn(PgBuiltInOids::OIDOID),
            PgOid::BuiltIn(PgBuiltInOids::INT8OID),
            PgOid::BuiltIn(PgBuiltInOids::INT8OID),
            PgOid::BuiltIn(PgBuiltInOids::INT8ARRAYOID),
            PgOid::BuiltIn(PgBuiltInOids::BOOLARRAYOID),
            PgOid::BuiltIn(PgBuiltInOids::INT8ARRAYOID),
            PgOid::BuiltIn(PgBuiltInOids::BOOLARRAYOID),
            PgOid::BuiltIn(PgBuiltInOids::INT8ARRAYOID),
            PgOid::BuiltIn(PgBuiltInOids::INT8ARRAYOID),
            PgOid::BuiltIn(PgBuiltInOids::INT8OID),
            PgOid::BuiltIn(PgBuiltInOids::INT8OID),
        ],
    )
}

#[pg_trigger]
fn syntra_capture_row<'a>(
    trigger: &'a PgTrigger<'a>,
) -> Result<Option<PgHeapTuple<'a, AllocatedByPostgres>>, pgrx::spi::Error> {
    if !matches!(trigger.level(), PgTriggerLevel::Row)
        || !matches!(trigger.when(), Ok(PgTriggerWhen::After))
    {
        stale();
    }
    let oid = trigger.relid().unwrap_or_else(|_| stale());
    let source = unsafe { PgRelation::with_lock(oid, pg_sys::AccessShareLock as i32) };
    let config = configuration(&source, trigger.trigger().tgoid);
    let old = pair(trigger.old(), &config);
    let new = pair(trigger.new(), &config);
    let mut changes = Vec::with_capacity(2);
    if let (Some((key, before)), Some((next, after))) = (old, new) {
        if key == next {
            changes.push((key, Some(before), Some(after)));
        } else {
            changes.push((key, Some(before), None));
            changes.push((next, None, Some(after)));
        }
    } else {
        if let Some((key, value)) = old {
            changes.push((key, Some(value), None));
        }
        if let Some((key, value)) = new {
            changes.push((key, None, Some(value)));
        }
    }
    if changes.is_empty() {
        stale();
    }
    let row_delta = i64::from(new.is_some()) - i64::from(old.is_some());
    let count = changes.len() as i64;
    let command = unsafe { i64::from(pg_sys::GetCurrentCommandId(false)) };
    // Lock, do not update, the head row. The lock is held to commit, so ordinals
    // stay gap-free and follow commit order as they did with the counter update,
    // but the row gains no version per write. Only pruning still updates it.
    // A deferred firing at commit locks on its first call and buffers its changes;
    // a pre-commit callback then writes them all with one insert and one mark
    // (capture_batch.rs). Any other firing, such as SET CONSTRAINTS IMMEDIATE
    // inside a statement, writes its own events and mark at once.
    let batched = crate::capture_batch::at_commit(oid);
    let held = if batched {
        crate::capture_batch::state(oid, config.epoch)
    } else {
        None
    };
    let (emitted, live, limit) = match held {
        Some(state) => state,
        None => {
            let (floor, limit, floor_rows) = Spi::connect_mut(|client| {
                client
                    .update(
                        head_plan()?,
                        Some(1),
                        &[
                            oid.into(),
                            config.epoch.into(),
                            trigger.trigger().tgoid.into(),
                        ],
                    )?
                    .first()
                    .get_three::<i64, i64, i64>()
            })?;
            let (Some(floor), Some(limit), Some(floor_rows)) = (floor, limit, floor_rows) else {
                full()
            };
            // A latest snapshot sees every mark committed before this lock was
            // granted, including under repeatable read, and this transaction's
            // own earlier marks.
            let (emitted, live) = {
                let _latest = Snapshot::latest();
                // SAFETY: the guard above pins this MVCC snapshot through the scan.
                let snapshot = unsafe { pg_sys::GetActiveSnapshot() };
                position(&source, snapshot, config.epoch, floor, floor_rows)
                    .unwrap_or_else(|| stale())
            };
            if batched {
                crate::capture_batch::begin(oid, config.epoch, emitted, live, limit);
            }
            (emitted, live, limit)
        }
    };
    let live = live + row_delta;
    if emitted > limit - count || emitted > i64::MAX - count || !(0..=1_000_000).contains(&live) {
        full();
    }
    if batched {
        crate::capture_batch::record(oid, &changes, command, row_delta);
        return Ok(None);
    }
    let keys: Vec<i64> = changes.iter().map(|c| c.0).collect();
    let before: Vec<bool> = changes.iter().map(|c| c.1.is_some()).collect();
    let old: Vec<Option<i64>> = changes.iter().map(|c| c.1.flatten()).collect();
    let after: Vec<bool> = changes.iter().map(|c| c.2.is_some()).collect();
    let new: Vec<Option<i64>> = changes.iter().map(|c| c.2.flatten()).collect();
    let commands = vec![command; changes.len()];
    let inserted = Spi::connect_mut(|client| {
        client
            .update(
                append_plan()?,
                Some(1),
                &[
                    oid.into(),
                    config.epoch.into(),
                    emitted.into(),
                    keys.into(),
                    before.into(),
                    old.into(),
                    after.into(),
                    new.into(),
                    commands.into(),
                    count.into(),
                    live.into(),
                ],
            )?
            .first()
            .get_one::<i64>()
    })?;
    if inserted != Some(count) {
        full();
    }
    Ok(None)
}

#[pg_trigger]
fn syntra_capture_no_truncate<'a>(
    _trigger: &'a PgTrigger<'a>,
) -> Result<Option<PgHeapTuple<'a, AllocatedByPostgres>>, pgrx::spi::Error> {
    pgrx::ereport!(
        ERROR,
        pgrx::PgSqlErrorCode::ERRCODE_FEATURE_NOT_SUPPORTED,
        "TRUNCATE is not supported while capture is installed"
    );
}
