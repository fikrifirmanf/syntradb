//! Explicit trusted publication and read-only reuse. No automatic planner routing.
use crate::native_catalog::{guards_current, oid_key, CatalogScan, Key};
use pgrx::prelude::*;
use pgrx::PgRelation;
use syntra_exec::Int4Predicate;

struct Version {
    epoch: i64,
    vector: Vec<i64>,
}

fn trackable(source: &PgRelation) -> bool {
    // SAFETY: caller holds the validated source's transaction-level read lock.
    unsafe {
        crate::catalog_char((*source.rd_rel).relpersistence) == pg_sys::RELPERSISTENCE_PERMANENT
            && !(*source.rd_rel).relhasrules
            && !pg_sys::has_superclass(source.oid())
            && !matches!(source.namespace(), "syntra" | "pg_catalog" | "pg_toast")
    }
}

pub(super) fn identity(source: &PgRelation, attribute: i16) -> String {
    let descriptor = source.tuple_desc();
    let field = descriptor
        .iter()
        .find(|a| a.attnum == attribute)
        .unwrap_or_else(|| pgrx::error!("locked source attribute disappeared"));
    // SAFETY: source read lock excludes rewrites. Copy only scalar current
    // relcache metadata; no snapshot or descriptor pointer is retained.
    unsafe {
        let database = pg_sys::MyDatabaseId;
        format!(
            "{}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}",
            pg_sys::GetSystemIdentifier(),
            database,
            source.oid(),
            source.namespace_oid(),
            source.rd_locator.spcOid,
            source.rd_locator.dbOid,
            source.rd_locator.relNumber,
            attribute,
            field.atttypid,
            field.atttypmod,
            field.attcollation,
            (*source.rd_rel).relam
        )
    }
}

fn version(source: &PgRelation, snapshot: pg_sys::Snapshot) -> Result<Version, &'static str> {
    if !trackable(source) {
        return Err("source_not_trackable");
    }
    let mut registry = CatalogScan::open(
        c"syntra",
        c"tracked_sources",
        c"tracked_sources_pkey",
        snapshot,
        &[oid_key(1, "source_relation", source.oid())],
    )
    .ok_or("metadata_unavailable")?;
    let row = registry.next().ok_or("untracked")?;
    let trigger = row
        .get::<pg_sys::Oid>(2, "trigger_oid")
        .ok_or("metadata_invalid")?;
    let epoch = row.get::<i64>(3, "epoch").ok_or("metadata_invalid")?;
    if epoch <= 0 || registry.next().is_some() {
        return Err("metadata_invalid");
    }
    if !guards_current(source, trigger) {
        return Err("guard_invalid");
    }
    let mut scan = CatalogScan::open(
        c"syntra",
        c"source_generation_shards",
        c"source_generation_shards_pkey",
        snapshot,
        &[oid_key(1, "source_relation", source.oid())],
    )
    .ok_or("metadata_unavailable")?;
    let mut vector = vec![-1; 16];
    let mut count = 0;
    while let Some(row) = scan.next() {
        count += 1;
        if count > 16 {
            return Err("metadata_invalid");
        }
        let shard = row
            .get::<i16>(2, "shard")
            .and_then(|n| usize::try_from(n).ok())
            .ok_or("metadata_invalid")?;
        let value = row.get::<i64>(3, "generation").ok_or("metadata_invalid")?;
        if shard >= 16 || !(0..=576460752303423487).contains(&value) || vector[shard] != -1 {
            return Err("metadata_invalid");
        }
        vector[shard] = value;
    }
    if count != 16 {
        return Err("metadata_invalid");
    }
    Ok(Version { epoch, vector })
}

fn keys(source: &PgRelation, attribute: i16) -> [Key; 2] {
    [
        oid_key(1, "source_relation", source.oid()),
        Key {
            attribute: 2,
            name: "source_attribute",
            kind: pg_sys::INT2OID,
            procedure: pg_sys::F_INT2EQ,
            strategy: pg_sys::BTEqualStrategyNumber as u16,
            value: attribute.into(),
        },
    ]
}

fn cached(
    source: &PgRelation,
    attribute: i16,
    snapshot: pg_sys::Snapshot,
    version: &Version,
    predicate: Int4Predicate,
) -> Result<crate::source::SummaryRow, &'static str> {
    let mut scan = CatalogScan::open(
        c"syntra",
        c"trusted_int4_projections",
        c"trusted_int4_projections_pkey",
        snapshot,
        &keys(source, attribute),
    )
    .ok_or("metadata_unavailable")?;
    let row = scan.next().ok_or("no_projection")?;
    // Explicit identity also rejects restored/cloned database bindings. Metadata
    // comparisons happen before allocating or validating the segment payload.
    if row.get::<String>(4, "source_identity").as_deref()
        != Some(identity(source, attribute).as_str())
    {
        return Err("storage_or_schema_changed");
    }
    if row.get::<i64>(5, "epoch") != Some(version.epoch) {
        return Err("epoch_changed");
    }
    let vector = row
        .get::<Vec<Option<i64>>>(6, "generation_vector")
        .and_then(|v| v.into_iter().collect::<Option<Vec<_>>>())
        .ok_or("metadata_invalid")?;
    if vector != version.vector {
        return Err("version_changed");
    }
    // SAFETY: backend initialized database identity, copied by value.
    if row.get::<pg_sys::Oid>(3, "source_database") != Some(unsafe { pg_sys::MyDatabaseId }) {
        return Err("database_changed");
    }
    let bytes = row.get::<Vec<u8>>(7, "segment").ok_or("invalid_segment")?;
    if scan.next().is_some() {
        return Err("metadata_invalid");
    }
    let result = syntra_exec::summarize_segment(&bytes, predicate, || {
        pgrx::check_for_interrupts!();
        true
    })
    .map_err(|_| "invalid_segment")?;
    let s = result.summary;
    Ok((s.rows(), s.non_nulls(), s.sum(), s.min(), s.max()))
}

type LiveRow = (
    i64,
    i64,
    Option<i64>,
    Option<i32>,
    Option<i32>,
    &'static str,
    &'static str,
);

fn summarize(source: &PgRelation, column: &str, predicate: Int4Predicate) -> LiveRow {
    let attribute = crate::source::validate_source(source, column);
    let snapshot = crate::source::source_snapshot();
    // AFTER STATEMENT counters have not advanced inside a row trigger or a
    // modifying CTE. Never reuse a projection after this transaction acquired
    // an XID, even when its writes were to an unrelated relation.
    // SAFETY: backend main thread; this lookup does not assign an XID.
    let result = if unsafe { pg_sys::GetCurrentFullTransactionIdIfAny().value != 0 } {
        Err("transaction_has_xid")
    } else {
        version(source, snapshot).and_then(|v| cached(source, attribute, snapshot, &v, predicate))
    };
    let (s, route, reason) = match result {
        Ok(s) => (s, "projection", "eligible"),
        Err(reason) => (
            crate::source::heap_summary_checked(source, attribute, snapshot, predicate),
            "heap",
            reason,
        ),
    };
    (s.0, s.1, s.2, s.3, s.4, route, reason)
}

#[allow(clippy::type_complexity)]
#[pg_extern(stable, parallel_unsafe)]
fn syntra_live_int4_summary(
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
    TableIterator::new(std::iter::once(summarize(
        &source,
        column,
        Int4Predicate::All,
    )))
}

#[allow(clippy::type_complexity)]
#[pg_extern(stable, parallel_unsafe)]
fn syntra_live_int4_range_summary(
    source: PgRelation,
    column: &str,
    lower: i32,
    upper: i32,
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
    TableIterator::new(std::iter::once(summarize(
        &source,
        column,
        Int4Predicate::Range { lower, upper },
    )))
}

#[pg_extern(volatile, parallel_unsafe)]
fn syntra_publish_trusted_int4(source: PgRelation, column: &str) -> i64 {
    // SAFETY: backend main thread; the SQL ACL and this check independently
    // restrict publication of authoritative bytes to an explicit admin operation.
    if !unsafe { pg_sys::superuser() } {
        pgrx::ereport!(
            ERROR,
            pgrx::PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
            "trusted publication currently requires a superuser"
        );
    }
    // Snapshot isolation could count an obsolete catalog after the admission
    // lock becomes available. Only a fresh SPI command snapshot can enforce
    // the publication capacity bound across committed publishers.
    // SAFETY: backend-local transaction settings, read without mutation.
    if unsafe {
        pg_sys::XactReadOnly || pg_sys::XactIsoLevel >= pg_sys::XACT_REPEATABLE_READ as i32
    } {
        pgrx::ereport!(
            ERROR,
            pgrx::PgSqlErrorCode::ERRCODE_FEATURE_NOT_SUPPORTED,
            "trusted publication requires a writable read committed transaction"
        );
    }
    let attribute = crate::source::validate_source(&source, column);
    let snapshot = crate::source::source_snapshot();
    let version = version(&source, snapshot).unwrap_or_else(|_| {
        pgrx::ereport!(
            ERROR,
            pgrx::PgSqlErrorCode::ERRCODE_FEATURE_NOT_SUPPORTED,
            "trusted publication requires a complete active source registration"
        );
    });
    // SAFETY: schema-qualified internal relation; transaction lock is retained
    // until commit/abort. Nonqueueing admission, retried briefly past a vacuum of
    // the catalog, serializes publishers for the catalog capacity bound while
    // ordinary MVCC readers remain unblocked.
    unsafe {
        let namespace = pg_sys::get_namespace_oid(c"syntra".as_ptr(), false);
        let relation = pg_sys::get_relname_relid(c"trusted_int4_projections".as_ptr(), namespace);
        if relation == pg_sys::InvalidOid {
            pgrx::error!("trusted catalog is missing");
        }
        if !crate::native_catalog::try_admission_lock(
            relation,
            pg_sys::ShareRowExclusiveLock as i32,
        ) {
            pgrx::ereport!(
                ERROR,
                pgrx::PgSqlErrorCode::ERRCODE_LOCK_NOT_AVAILABLE,
                "another trusted publisher is active; retry after its transaction ends"
            );
        }
    }
    // A valid publication at this exact snapshot needs no replacement. Assigned
    // XIDs are excluded: nested row triggers can see source changes before the
    // AFTER STATEMENT counter has advanced. Still validate the payload so a
    // corrupt cached segment is rebuilt rather than preserved by this shortcut.
    // SAFETY: backend-local lookup, which never assigns a transaction ID.
    if unsafe { pg_sys::GetCurrentFullTransactionIdIfAny().value == 0 } {
        if let Ok(summary) = cached(&source, attribute, snapshot, &version, Int4Predicate::All) {
            return summary.0;
        }
    }
    let (bytes, rows) =
        crate::source::encode_source(&source, attribute, snapshot).unwrap_or_else(|e| {
            let code = match e {
                syntra_storage::SegmentError::AllocationFailed => {
                    pgrx::PgSqlErrorCode::ERRCODE_OUT_OF_MEMORY
                }
                syntra_storage::SegmentError::Cancelled => {
                    pgrx::PgSqlErrorCode::ERRCODE_QUERY_CANCELED
                }
                _ => pgrx::PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            };
            pgrx::ereport!(ERROR, code, e.to_string());
        });
    let identity = identity(&source, attribute);
    // SAFETY: copy the current backend database OID; no pointer escapes.
    let database = unsafe { pg_sys::MyDatabaseId };
    let inserted = Spi::get_one_with_args::<i64>(r#"WITH published AS (
        INSERT INTO syntra.trusted_int4_projections
        (source_relation,source_attribute,source_database,source_identity,epoch,generation_vector,segment)
        SELECT $1,$2,$3,$4,$5,$6,$7
        WHERE EXISTS (SELECT source_relation FROM syntra.trusted_int4_projections
            WHERE source_relation=$1 AND source_attribute=$2)
            OR (SELECT count(*) FROM syntra.trusted_int4_projections) < 64
        ON CONFLICT (source_relation,source_attribute) DO UPDATE SET
            source_database=EXCLUDED.source_database,source_identity=EXCLUDED.source_identity,
            epoch=EXCLUDED.epoch,generation_vector=EXCLUDED.generation_vector,segment=EXCLUDED.segment
        RETURNING source_relation) SELECT count(*) FROM published"#,
        &[source.oid().into(),attribute.into(),database.into(),identity.as_str().into(),
            version.epoch.into(),version.vector.into(),bytes.into()])
        .unwrap_or_else(|e| pgrx::error!("{}",e));
    if inserted != Some(1) {
        pgrx::ereport!(
            ERROR,
            pgrx::PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            "trusted catalog capacity of 64 columns reached"
        );
    }
    i64::from(rows)
}
