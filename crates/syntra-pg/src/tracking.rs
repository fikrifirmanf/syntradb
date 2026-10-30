//! Transactional counter shards used with epochs and storage identity for reuse.
use pgrx::prelude::*;
use pgrx::trigger_support::{PgTriggerLevel, PgTriggerWhen};
use pgrx::{AllocatedByPostgres, PgRelation};

const TRIGGER_NAME: &std::ffi::CStr = c"_syntra_source_changed";
const GENERATION_SHARDS: u64 = 16;

/// Statement triggers roll their counter update back with the source mutation.
#[pg_trigger]
fn syntra_source_changed<'a>(
    trigger: &'a PgTrigger<'a>,
) -> Result<Option<PgHeapTuple<'a, AllocatedByPostgres>>, pgrx::spi::Error> {
    if !matches!(trigger.level(), PgTriggerLevel::Statement)
        || !matches!(trigger.when(), Ok(PgTriggerWhen::After))
    {
        pgrx::error!("source tracking requires an AFTER STATEMENT trigger");
    }
    let relation = trigger
        .relid()
        .unwrap_or_else(|error| pgrx::error!("{}", error));
    let trigger_oid = trigger.trigger().tgoid;
    // A captured source's feed records every committed row change, and trusted
    // reuse compares its position instead. Updating a counter row here would add
    // a version that a held snapshot keeps and every later update walks.
    if captured(relation, trigger_oid) {
        return Ok(None);
    }
    // SAFETY: this runs inside a source statement on the backend main thread.
    // Counter writes need an assigned XID even for a source statement with no
    // changed rows. The bounded modulo result fits int2. No backend is shared.
    let shard = unsafe { (pg_sys::GetCurrentFullTransactionId().value % GENERATION_SHARDS) as i16 };
    let updated = Spi::get_one_with_args::<i64>(
        "WITH changed AS (UPDATE syntra.source_generation_shards AS g SET generation = g.generation + 1 FROM syntra.tracked_sources AS s WHERE s.source_relation = $1 AND s.trigger_oid = $2 AND g.source_relation = s.source_relation AND g.shard = $3 RETURNING g.generation) SELECT count(*) FROM changed",
        &[relation.into(), trigger_oid.into(), shard.into()],
    )?;
    if updated != Some(1) {
        pgrx::ereport!(
            ERROR,
            pgrx::PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            "source tracking registration is missing; stop writes and repair the registration"
        );
    }
    Ok(None)
}

/// True when this trigger is the source's current registration and the source is
/// captured in its current tracking epoch, both read at a latest snapshot as the
/// capture row trigger reads them. Any doubt counts as not captured, which keeps
/// the counter update.
fn captured(relation: pg_sys::Oid, trigger: pg_sys::Oid) -> bool {
    // SAFETY: the firing statement already holds a lock on its source.
    let source = unsafe { PgRelation::with_lock(relation, pg_sys::AccessShareLock as i32) };
    let _latest = crate::capture::Snapshot::latest();
    // SAFETY: the guard above pins this MVCC snapshot through both scans.
    let snapshot = unsafe { pg_sys::GetActiveSnapshot() };
    let Some(mut registry) = crate::native_catalog::CatalogScan::open(
        c"syntra",
        c"tracked_sources",
        c"tracked_sources_pkey",
        snapshot,
        &[crate::native_catalog::oid_key(
            1,
            "source_relation",
            relation,
        )],
    ) else {
        return false;
    };
    let Some(row) = registry.next() else {
        return false;
    };
    let (Some(registered), Some(epoch)) = (
        row.get::<pg_sys::Oid>(2, "trigger_oid"),
        row.get::<i64>(3, "epoch"),
    ) else {
        return false;
    };
    drop(registry);
    registered == trigger
        && matches!(
            crate::capture::feed_position(&source, snapshot, epoch),
            Ok(Some(_))
        )
}

/// Admin-only setup acquires an exclusive source lock before installing a typed
/// trigger statement and its private counter row in the same transaction.
#[pg_extern(volatile, parallel_unsafe)]
fn syntra_track_source(source: PgRelation, column: &str) -> i64 {
    // SAFETY: PostgreSQL backend main thread; no pointers escape this call.
    // The SQL ACL and this check both restrict installation to a superuser.
    unsafe {
        if !pg_sys::superuser() {
            pgrx::ereport!(
                ERROR,
                pgrx::PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
                "source tracking setup currently requires a superuser"
            );
        }
    }
    // SAFETY: source's regclass adapter holds AccessShareLock. Upgrade before
    // validating or changing the relation. Both locks follow transaction lifetime.
    let locked = unsafe { PgRelation::with_lock(source.oid(), pg_sys::AccessExclusiveLock as i32) };
    // Tracking counts mutations and never reads the column's values, so it also
    // accepts the int2, int8 and numeric columns that capture feeds can use.
    crate::source::validate_column(
        &locked,
        column,
        &[
            pg_sys::INT2OID,
            pg_sys::INT4OID,
            pg_sys::INT8OID,
            pg_sys::NUMERICOID,
        ],
        "source tracking requires a stored, non-generated int2, int4, int8 or numeric column",
    );
    // SAFETY: locked relation descriptor is live; inheritance lookup is catalog-only.
    unsafe {
        if crate::catalog_char((*locked.rd_rel).relpersistence) != pg_sys::RELPERSISTENCE_PERMANENT
            || pg_sys::has_superclass(locked.oid())
        {
            pgrx::ereport!(
                ERROR,
                pgrx::PgSqlErrorCode::ERRCODE_FEATURE_NOT_SUPPORTED,
                "source tracking requires a permanent heap table without inheritance"
            );
        }
    }
    let function_oid =
        Spi::get_one::<pg_sys::Oid>("SELECT 'public.syntra_source_changed()'::regprocedure::oid")
            .unwrap_or_else(|error| pgrx::error!("{}", error))
            .unwrap_or_else(|| pgrx::error!("source tracking function is missing"));
    // SAFETY: relation remains locked; lookup copies an OID, not a tuple pointer.
    let existing = unsafe { pg_sys::get_trigger_oid(locked.oid(), TRIGGER_NAME.as_ptr(), true) };
    if existing != pg_sys::InvalidOid {
        let valid = Spi::get_one_with_args::<bool>(
            "SELECT count(*) = 1 FROM pg_catalog.pg_trigger WHERE oid = $1 AND tgrelid = $2 AND tgfoid = $3 AND tgenabled = 'A' AND tgtype = 60 AND NOT tgisinternal AND tgnargs = 0 AND tgqual IS NULL AND tgconstraint = 0 AND tgparentid = 0 AND tgattr = ''::int2vector",
            &[existing.into(), locked.oid().into(), function_oid.into()],
        ).unwrap_or_else(|error| pgrx::error!("{}", error));
        if valid != Some(true) {
            pgrx::error!(
                "existing source tracking trigger is incompatible; repair it before registration"
            );
        }
    }
    // SAFETY: palloc node fields start zeroed. This is an AFTER statement trigger
    // for all four mutation events. Explicit relation/function OIDs avoid parsing
    // identifiers or evaluating dynamic SQL. Source validation excludes partitions,
    // so PostgreSQL will not recurse with the unused RangeVar field.
    let trigger_oid = if existing != pg_sys::InvalidOid {
        existing
    } else {
        unsafe {
            let mut statement =
                PgBox::<pg_sys::CreateTrigStmt>::alloc_node(pg_sys::NodeTag::T_CreateTrigStmt);
            statement.trigname = pg_sys::pstrdup(TRIGGER_NAME.as_ptr());
            statement.events = (pg_sys::TRIGGER_TYPE_INSERT
                | pg_sys::TRIGGER_TYPE_UPDATE
                | pg_sys::TRIGGER_TYPE_DELETE
                | pg_sys::TRIGGER_TYPE_TRUNCATE) as i16;
            pg_sys::CreateTriggerFiringOn(
                statement.as_ptr(),
                c"SyntraDB source tracking".as_ptr(),
                locked.oid(),
                pg_sys::InvalidOid,
                pg_sys::InvalidOid,
                pg_sys::InvalidOid,
                function_oid,
                pg_sys::InvalidOid,
                std::ptr::null_mut(),
                false,
                false,
                pg_sys::TRIGGER_FIRES_ALWAYS as std::ffi::c_char,
            )
            .objectId
        }
    };
    let epoch = Spi::get_one_with_args::<i64>(
        "INSERT INTO syntra.tracked_sources (source_relation, trigger_oid) VALUES ($1, $2) ON CONFLICT (source_relation) DO UPDATE SET epoch = DEFAULT, trigger_oid = EXCLUDED.trigger_oid RETURNING epoch",
        &[locked.oid().into(), trigger_oid.into()],
    ).unwrap_or_else(|error| pgrx::error!("{}", error))
        .unwrap_or_else(|| pgrx::error!("source tracking registration failed"));
    Spi::run_with_args(
        "INSERT INTO syntra.source_generation_shards (source_relation, shard, generation) SELECT $1, n::int2, 0 FROM generate_series(0, $2::int4 - 1) AS n ON CONFLICT (source_relation, shard) DO UPDATE SET generation = 0",
        &[locked.oid().into(), (GENERATION_SHARDS as i32).into()],
    ).unwrap_or_else(|error| pgrx::error!("{}", error));
    let valid = Spi::get_one_with_args::<bool>(
        "SELECT guard_config_valid FROM public.syntra_source_tracking_status($1::regclass) LIMIT 1",
        &[locked.oid().into()],
    )
    .unwrap_or_else(|error| pgrx::error!("{}", error));
    if valid != Some(true) {
        pgrx::error!("source tracking requires active DDL guards");
    }
    epoch
}
