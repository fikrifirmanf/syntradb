//! Source capture uses the caller's active MVCC snapshot and a locked relation.
//! All new manual PostgreSQL FFI for source reads is confined to this module.
use pgrx::prelude::*;
use pgrx::PgRelation;
use std::ffi::CStr;
use std::marker::PhantomData;
use syntra_storage::SegmentError;

pub(super) fn unsupported(message: &'static str) -> ! {
    pgrx::ereport!(
        ERROR,
        pgrx::PgSqlErrorCode::ERRCODE_FEATURE_NOT_SUPPORTED,
        message
    );
}

/// The relation guard, active snapshot, scan and slot all stay on one backend.
/// The lifetime prevents the scan from outliving its AccessShare-locked source.
/// `V` is the column's Rust type; callers validate that it matches the column.
struct SourceScan<'a, V = i32> {
    scan: pg_sys::TableScanDesc,
    slot: *mut pg_sys::TupleTableSlot,
    attribute: i16,
    _relation: PhantomData<&'a PgRelation>,
    _value: PhantomData<V>,
}

impl<'a, V> SourceScan<'a, V> {
    fn with_time_range(
        relation: &'a PgRelation,
        attribute: i16,
        snapshot: pg_sys::Snapshot,
        time_attribute: i16,
        range: TimeRange,
    ) -> Self {
        let (ge, lt) = match range.column_type {
            pg_sys::DATEOID => (pg_sys::F_DATE_GE, pg_sys::F_DATE_LT),
            pg_sys::TIMESTAMPOID => (pg_sys::F_TIMESTAMP_GE, pg_sys::F_TIMESTAMP_LT),
            pg_sys::TIMESTAMPTZOID => (pg_sys::F_TIMESTAMPTZ_GE, pg_sys::F_TIMESTAMPTZ_LT),
            _ => unsupported("unsupported time filter type"),
        };
        // SAFETY: the caller validates both stored attributes under the relation
        // lock. Typed SQL adapters supply by-value date/timestamp Datums matching
        // column_type. Native built-in comparisons enforce [lower, upper),
        // including infinity and NULL semantics. heap_beginscan copies the keys;
        // the snapshot and relation remain owned by this backend for the scan.
        unsafe {
            let mut keys = [pg_sys::ScanKeyData::default(); 2];
            pg_sys::ScanKeyInit(
                &mut keys[0],
                time_attribute,
                pg_sys::BTGreaterEqualStrategyNumber as u16,
                pg_sys::Oid::from(ge),
                range.lower,
            );
            pg_sys::ScanKeyInit(
                &mut keys[1],
                time_attribute,
                pg_sys::BTLessStrategyNumber as u16,
                pg_sys::Oid::from(lt),
                range.upper,
            );
            let scan = pg_sys::table_beginscan(relation.as_ptr(), snapshot, 2, keys.as_mut_ptr());
            let slot = pg_sys::table_slot_create(relation.as_ptr(), std::ptr::null_mut());
            Self {
                scan,
                slot,
                attribute,
                _relation: PhantomData,
                _value: PhantomData,
            }
        }
    }

    fn new(relation: &'a PgRelation, attribute: i16, snapshot: pg_sys::Snapshot) -> Self {
        // SAFETY: as in `with_predicate`, without scan keys.
        unsafe {
            let scan =
                pg_sys::table_beginscan(relation.as_ptr(), snapshot, 0, std::ptr::null_mut());
            let slot = pg_sys::table_slot_create(relation.as_ptr(), std::ptr::null_mut());
            Self {
                scan,
                slot,
                attribute,
                _relation: PhantomData,
                _value: PhantomData,
            }
        }
    }
}

impl<'a> SourceScan<'a, i32> {
    fn with_predicate(
        relation: &'a PgRelation,
        attribute: i16,
        snapshot: pg_sys::Snapshot,
        predicate: syntra_exec::Int4Predicate,
    ) -> Self {
        // SAFETY: the caller validates a heap relation, positive int4 attribute,
        // and active MVCC snapshot. PgRelation holds AccessShareLock for 'a.
        // Scan state never escapes the backend; PostgreSQL owns its allocations.
        unsafe {
            let mut keys = [pg_sys::ScanKeyData::default(); 2];
            let nkeys = if let syntra_exec::Int4Predicate::Range { lower, upper } = predicate {
                // Native heap keys apply the same built-in int4 comparisons as
                // BETWEEN, excluding NULLs. heap_beginscan copies these keys into
                // scan-owned memory; int4 arguments are by-value Datums.
                pg_sys::ScanKeyInit(
                    &mut keys[0],
                    attribute,
                    pg_sys::BTGreaterEqualStrategyNumber as u16,
                    pg_sys::Oid::from(pg_sys::F_INT4GE),
                    pg_sys::Datum::from(lower),
                );
                pg_sys::ScanKeyInit(
                    &mut keys[1],
                    attribute,
                    pg_sys::BTLessEqualStrategyNumber as u16,
                    pg_sys::Oid::from(pg_sys::F_INT4LE),
                    pg_sys::Datum::from(upper),
                );
                2
            } else {
                0
            };
            let scan = pg_sys::table_beginscan(
                relation.as_ptr(),
                snapshot,
                nkeys,
                if nkeys == 0 {
                    std::ptr::null_mut()
                } else {
                    keys.as_mut_ptr()
                },
            );
            let slot = pg_sys::table_slot_create(relation.as_ptr(), std::ptr::null_mut());
            Self {
                scan,
                slot,
                attribute,
                _relation: PhantomData,
                _value: PhantomData,
            }
        }
    }
}

impl<V: FromDatum> Iterator for SourceScan<'_, V> {
    type Item = Option<V>;

    fn next(&mut self) -> Option<Self::Item> {
        // SAFETY: scan/slot belong to the same locked table and are live until
        // Drop. The checked attribute has type V in that table's descriptor.
        // FromDatum copies the by-value integer; no tuple pointer is retained.
        unsafe {
            if !pg_sys::table_scan_getnextslot(
                self.scan,
                pg_sys::ScanDirection::ForwardScanDirection,
                self.slot,
            ) {
                return None;
            }
            let mut is_null = false;
            let value = pg_sys::slot_getattr(self.slot, i32::from(self.attribute), &mut is_null);
            Some(V::from_datum(value, is_null))
        }
    }
}

impl<V> Drop for SourceScan<'_, V> {
    fn drop(&mut self) {
        // SAFETY: one owner, same backend thread, and no outstanding tuple borrows.
        // Releasing the slot first also releases its buffer pins before scan end.
        unsafe {
            pg_sys::ExecDropSingleTupleTableSlot(self.slot);
            pg_sys::table_endscan(self.scan);
        }
    }
}

/// Read both fields from the same MVCC-visible tuple. The value scan owns the
/// slot and scan; copying the integer key retains no PostgreSQL tuple pointer.
struct SourcePairs<'a, K, V> {
    values: SourceScan<'a, V>,
    key: i16,
    _key: PhantomData<K>,
}

impl<K: FromDatum, V: FromDatum> Iterator for SourcePairs<'_, K, V> {
    type Item = (Option<K>, Option<V>);

    fn next(&mut self) -> Option<Self::Item> {
        let value = self.values.next()?;
        // SAFETY: validation checked both integer attribute types under the
        // relation lock. SourceScan just filled this live slot, and FromDatum
        // copies the by-value key before the next tuple can replace the slot.
        let key = unsafe {
            let mut is_null = false;
            let datum = pg_sys::slot_getattr(self.values.slot, i32::from(self.key), &mut is_null);
            K::from_datum(datum, is_null)
        };
        Some((key, value))
    }
}

pub(super) fn group_integer(
    source: &PgRelation,
    key: &str,
    value: &str,
    max_groups: usize,
) -> Result<Vec<syntra_exec::GroupedInt8Summary>, syntra_exec::GroupingError> {
    let types = &[pg_sys::INT2OID, pg_sys::INT4OID, pg_sys::INT8OID];
    let message = "heap grouping requires stored, non-generated int2, int4 or int8 columns";
    let (key, key_type) = validate_column(source, key, types, message);
    let (value, value_type) = validate_column(source, value, types, message);
    let snapshot = heap_snapshot();
    crate::with_layout!(
        width(key_type),
        width(value_type),
        unsupported(message),
        group_integer_checked(source, key, value, snapshot, max_groups)
    )
}

fn group_integer_checked<K: crate::Column, V: crate::Column>(
    source: &PgRelation,
    key: i16,
    value: i16,
    snapshot: pg_sys::Snapshot,
    max_groups: usize,
) -> Result<Vec<syntra_exec::GroupedInt8Summary>, syntra_exec::GroupingError> {
    let rows = SourcePairs::<K, V> {
        values: SourceScan::new(source, value, snapshot),
        key,
        _key: PhantomData,
    };
    syntra_exec::group_int8(
        rows.map(|(key, value)| (key.map(Into::into), value.map(Into::into))),
        max_groups,
        || {
            pgrx::check_for_interrupts!();
            true
        },
    )
}

struct Provenance {
    database: pg_sys::Oid,
    snapshot: String,
    transaction: Option<String>,
    command: i64,
    captured_at: TimestampWithTimeZone,
    active_snapshot: pg_sys::Snapshot,
}

// Fresh scans use table_beginscan's SO_TYPE_SEQSCAN contract: heap_beginscan
// acquires the relation predicate lock, and the native visibility scan checks
// outgoing SSI conflicts. Cached/capture paths must use source_snapshot below.
fn heap_snapshot() -> pg_sys::Snapshot {
    // SAFETY: backend main thread, called while the SQL function is executing.
    // The active snapshot stays owned by PostgreSQL throughout the native scan.
    unsafe {
        let snapshot = pg_sys::GetActiveSnapshot();
        if snapshot.is_null() || (*snapshot).snapshot_type != pg_sys::SnapshotType::SNAPSHOT_MVCC {
            unsupported("source capture requires an active MVCC snapshot");
        }
        snapshot
    }
}

pub(super) fn source_snapshot() -> pg_sys::Snapshot {
    let snapshot = heap_snapshot();
    // SAFETY: backend-thread transaction state. Reused state has no separate
    // proof that it supplies the source relation's SSI read dependencies.
    if unsafe { pg_sys::XactIsoLevel == pg_sys::XACT_SERIALIZABLE as i32 } {
        unsupported("serializable source capture is not supported yet");
    }
    snapshot
}

/// Bounded keyed seed scan. Caller owns the source's exclusive transaction lock
/// and an active snapshot obtained after that lock was granted. Keys and values
/// are returned as `int8` whatever the column widths; widths are 2, 4 or 8 bytes.
pub(super) fn keyed_seed(
    source: &PgRelation,
    key: (i16, i16),
    value: (i16, i16),
    value_scale: Option<crate::numeric::Scale>,
    snapshot: pg_sys::Snapshot,
) -> Vec<(i64, Option<i64>)> {
    if let Some(scale) = value_scale {
        // A numeric column is read as numeric and stored as a scaled int8.
        let stored = |v: pgrx::AnyNumeric| {
            crate::numeric::to_stored(&v, scale).unwrap_or_else(|e| unsupported(e))
        };
        return match crate::stored_width(key.1) {
            4 => keyed_seed_typed::<i32, _>(source, key.0, value.0, snapshot, stored),
            8 => keyed_seed_typed::<i64, _>(source, key.0, value.0, snapshot, stored),
            _ => pgrx::error!("capture column width is invalid"),
        };
    }
    crate::with_layout!(
        key.1,
        value.1,
        pgrx::error!("capture column width is invalid"),
        keyed_seed_integers(source, key.0, value.0, snapshot)
    )
}

fn keyed_seed_integers<K: crate::Column, V: crate::Column>(
    source: &PgRelation,
    key: i16,
    value: i16,
    snapshot: pg_sys::Snapshot,
) -> Vec<(i64, Option<i64>)> {
    keyed_seed_typed::<K, V>(source, key, value, snapshot, Into::into)
}

/// Seed rows of key type `K`, reading values as `V` and storing them as `stored`
/// returns them.
fn keyed_seed_typed<K: crate::Column, V: FromDatum>(
    source: &PgRelation,
    key: i16,
    value: i16,
    snapshot: pg_sys::Snapshot,
    stored: impl Fn(V) -> i64,
) -> Vec<(i64, Option<i64>)> {
    let mut scan = SourceScan::<V>::new(source, value, snapshot);
    let mut rows = Vec::new();
    rows.try_reserve_exact(100_000).unwrap_or_else(|_| {
        pgrx::ereport!(
            ERROR,
            pgrx::PgSqlErrorCode::ERRCODE_OUT_OF_MEMORY,
            "cannot allocate capture seed"
        );
    });
    while let Some(value) = scan.next() {
        if rows.len() == 100_000 {
            pgrx::ereport!(
                ERROR,
                pgrx::PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                "capture seed exceeds 100000 rows"
            );
        }
        // SAFETY: the live scan slot still holds the tuple just yielded. The
        // caller validated a non-NULL primary key of type K under an exclusive lock.
        let key = unsafe {
            let mut null = false;
            let datum = pg_sys::slot_getattr(scan.slot, i32::from(key), &mut null);
            if null {
                pgrx::error!("capture primary key is NULL");
            }
            K::from_datum(datum, false)
                .unwrap_or_else(|| pgrx::error!("capture primary key is invalid"))
        };
        rows.push((key.into(), value.map(&stored)));
    }
    rows.sort_unstable_by_key(|row| row.0);
    pgrx::check_for_interrupts!();
    rows
}

fn provenance() -> Provenance {
    // SAFETY: invoked only by a parallel-unsafe SQL function on the backend main
    // thread. Direct calls use PostgreSQL's typed builtins and do not run SPI or
    // acquire a different snapshot. Snapshot storage stays active until SQL return.
    unsafe {
        let snapshot = source_snapshot();
        let datum = pgrx::direct_function_call_as_datum(pg_sys::pg_current_snapshot, &[])
            .unwrap_or_else(|| pgrx::error!("cannot obtain source snapshot"));
        let text = pgrx::direct_function_call::<&CStr>(pg_sys::pg_snapshot_out, &[Some(datum)])
            .unwrap_or_else(|| pgrx::error!("cannot describe source snapshot"));
        let captured_at =
            pgrx::direct_function_call::<TimestampWithTimeZone>(pg_sys::statement_timestamp, &[])
                .unwrap_or_else(|| pgrx::error!("cannot obtain capture statement timestamp"));
        let transaction = pg_sys::GetCurrentFullTransactionIdIfAny().value;
        Provenance {
            database: pg_sys::MyDatabaseId,
            snapshot: text.to_string_lossy().into_owned(),
            transaction: (transaction != 0).then(|| transaction.to_string()),
            command: i64::from((*snapshot).curcid),
            captured_at,
            active_snapshot: snapshot,
        }
    }
}

pub(super) fn validate_source(relation: &PgRelation, column: &str) -> i16 {
    validate_column(
        relation,
        column,
        &[pg_sys::INT4OID],
        "source capture requires a stored, non-generated int4 column",
    )
    .0
}

/// Byte width of a capture column type: 2 for int2, 4 for int4, 8 for int8 and
/// for numeric, which is stored as a scaled int8.
pub(super) fn width(column_type: pg_sys::Oid) -> i16 {
    match column_type {
        pg_sys::INT2OID => 2,
        pg_sys::INT4OID => 4,
        pg_sys::INT8OID | pg_sys::NUMERICOID => 8,
        _ => unsupported("capture columns must be int2, int4, int8 or numeric"),
    }
}

/// Like `validate_source`, accepting any of `types`; returns the attribute and type.
pub(super) fn validate_column(
    relation: &PgRelation,
    column: &str,
    types: &[pg_sys::Oid],
    wrong_type: &'static str,
) -> (i16, pg_sys::Oid) {
    // SAFETY: PgRelation was opened by pgrx's REGCLASS adapter with AccessShareLock.
    // Catalog and ACL calls remain in its owning backend. Check permissions before
    // inspecting user column metadata; raw table scans do not enforce SQL ACL/RLS.
    unsafe {
        let role = pg_sys::GetUserId();
        if pg_sys::object_aclcheck(
            pg_sys::NamespaceRelationId,
            relation.namespace_oid(),
            role,
            pg_sys::ACL_USAGE.into(),
        ) != pg_sys::AclResult::ACLCHECK_OK
            || pg_sys::pg_class_aclcheck(relation.oid(), role, pg_sys::ACL_SELECT.into())
                != pg_sys::AclResult::ACLCHECK_OK
        {
            pgrx::ereport!(
                ERROR,
                pgrx::PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
                "source capture requires schema USAGE and table SELECT privileges"
            );
        }
        let catalog = &*relation.rd_rel;
        if crate::catalog_char(catalog.relkind) != pg_sys::RELKIND_RELATION
            || catalog.relam != pg_sys::HEAP_TABLE_AM_OID
            || catalog.relispartition
            || catalog.relhassubclass
        {
            unsupported("source capture supports only ordinary heap tables without partitions or descendants");
        }
        if catalog.relrowsecurity || catalog.relforcerowsecurity {
            unsupported("source capture does not support row-level security");
        }
    }
    let descriptor = relation.tuple_desc();
    let attribute = descriptor
        .iter()
        .find(|attribute| {
            !attribute.attisdropped && pgrx::name_data_to_str(&attribute.attname) == column
        })
        .unwrap_or_else(|| {
            pgrx::ereport!(
                ERROR,
                pgrx::PgSqlErrorCode::ERRCODE_UNDEFINED_COLUMN,
                "source column does not exist"
            );
        });
    if !types.contains(&attribute.atttypid) || attribute.attgenerated != 0 || attribute.attnum <= 0
    {
        unsupported(wrong_type);
    }
    // SAFETY: the argument adapter already holds AccessShareLock. Retain an
    // additional reference until transaction end: PgRelation drops its own lock
    // at function return, but repeated reads must stay protected from DDL just
    // like a native table reference. PostgreSQL releases this reference on abort
    // or commit; no Rust-owned pointer or manual unlock escapes this function.
    unsafe { pg_sys::LockRelationOid(relation.oid(), pg_sys::AccessShareLock as i32) };
    (attribute.attnum, attribute.atttypid)
}

pub(super) type SummaryRow = (i64, i64, Option<i64>, Option<i32>, Option<i32>);
/// Rows, non-NULLs, sum, minimum and maximum for either width. `int8` sums can
/// exceed `bigint`, as PostgreSQL's `sum(bigint)` returning `numeric` allows.
pub(super) type WideSummary = (i64, i64, Option<i128>, Option<i64>, Option<i64>);

/// Only typed SQL adapters construct these by-value Datums. The source column
/// must have exactly column_type; implicit timestamp/time-zone casts are refused.
pub(super) struct TimeRange {
    pub(super) column_type: pg_sys::Oid,
    pub(super) lower: pg_sys::Datum,
    pub(super) upper: pg_sys::Datum,
}

pub(super) fn time_summary(
    source: &PgRelation,
    value: &str,
    time: &str,
    range: TimeRange,
) -> (WideSummary, i64, crate::numeric::Scale) {
    let (value, value_type) = validate_column(
        source,
        value,
        &[
            pg_sys::INT2OID,
            pg_sys::INT4OID,
            pg_sys::INT8OID,
            pg_sys::NUMERICOID,
        ],
        "time summaries require a stored integer or fixed-scale numeric value column",
    );
    let (time, _) = validate_column(
        source,
        time,
        &[range.column_type],
        "time filter column must be stored and match the bounds' date or timestamp type",
    );
    let snapshot = heap_snapshot();
    let scale = if value_type == pg_sys::NUMERICOID {
        let typmod = source
            .tuple_desc()
            .iter()
            .find(|a| a.attnum == value)
            .map(|a| a.atttypmod)
            .unwrap_or(-1);
        crate::numeric::from_typmod(typmod).unwrap_or_else(|| {
            unsupported("time summaries require numeric(p,s) with 1 <= p <= 18 and 0 <= s <= p")
        })
    } else {
        crate::numeric::Scale {
            precision: 18,
            scale: 0,
        }
    };
    let (summary, nans) = match value_type {
        pg_sys::INT2OID => (
            integer_summary(
                SourceScan::<i16>::with_time_range(source, value, snapshot, time, range)
                    .map(|v| v.map(i64::from)),
            ),
            0,
        ),
        pg_sys::INT4OID => (
            integer_summary(
                SourceScan::<i32>::with_time_range(source, value, snapshot, time, range)
                    .map(|v| v.map(i64::from)),
            ),
            0,
        ),
        pg_sys::INT8OID => (
            integer_summary(SourceScan::<i64>::with_time_range(
                source, value, snapshot, time, range,
            )),
            0,
        ),
        pg_sys::NUMERICOID => numeric_summary(
            SourceScan::<pgrx::AnyNumeric>::with_time_range(source, value, snapshot, time, range),
            scale,
        ),
        _ => unsupported("unsupported time summary value type"),
    };
    (summary, nans, scale)
}

pub(super) fn widen(s: SummaryRow) -> WideSummary {
    (
        s.0,
        s.1,
        s.2.map(i128::from),
        s.3.map(i64::from),
        s.4.map(i64::from),
    )
}

/// Fresh `int8` aggregate over `snapshot`; the caller validated an int8 column.
pub(super) fn heap_int8_summary_checked(
    source: &PgRelation,
    attribute: i16,
    snapshot: pg_sys::Snapshot,
) -> WideSummary {
    integer_summary(SourceScan::<i64>::new(source, attribute, snapshot))
}

fn integer_summary(rows: impl Iterator<Item = Option<i64>>) -> WideSummary {
    let mut summary = syntra_core::Int8Summary::default();
    for (index, value) in rows.enumerate() {
        if index % 1024 == 0 {
            pgrx::check_for_interrupts!();
        }
        summary.push(value).unwrap_or_else(|error| {
            pgrx::ereport!(
                ERROR,
                pgrx::PgSqlErrorCode::ERRCODE_NUMERIC_VALUE_OUT_OF_RANGE,
                error.to_string()
            );
        });
    }
    (
        summary.rows(),
        summary.non_nulls(),
        summary.sum(),
        summary.min(),
        summary.max(),
    )
}

/// Fresh aggregate of a numeric(p,s) column over `snapshot`, in stored integers
/// (value times 10^s), and the number of NaN values, which the summary counts as
/// non-NULL rows but leaves out of the sum and bounds. The caller validated the
/// column and its scale.
pub(super) fn heap_numeric_summary_checked(
    source: &PgRelation,
    attribute: i16,
    snapshot: pg_sys::Snapshot,
    scale: crate::numeric::Scale,
) -> (WideSummary, i64) {
    numeric_summary(
        SourceScan::<pgrx::AnyNumeric>::new(source, attribute, snapshot),
        scale,
    )
}

fn numeric_summary(
    rows: impl Iterator<Item = Option<pgrx::AnyNumeric>>,
    scale: crate::numeric::Scale,
) -> (WideSummary, i64) {
    let mut summary = syntra_core::Int8Summary::default();
    let mut nans = 0_i64;
    for (index, value) in rows.enumerate() {
        if index % 1024 == 0 {
            pgrx::check_for_interrupts!();
        }
        let stored = match value {
            Some(v) if v.is_nan() => {
                nans += 1;
                None
            }
            Some(v) => {
                Some(crate::numeric::to_stored(&v, scale).unwrap_or_else(|e| unsupported(e)))
            }
            None => None,
        };
        summary.push(stored).unwrap_or_else(|error| {
            pgrx::ereport!(
                ERROR,
                pgrx::PgSqlErrorCode::ERRCODE_NUMERIC_VALUE_OUT_OF_RANGE,
                error.to_string()
            );
        });
    }
    (
        (
            summary.rows(),
            summary.non_nulls() + nans,
            summary.sum(),
            summary.min(),
            summary.max(),
        ),
        nans,
    )
}

fn heap_summary(
    source: &PgRelation,
    column: &str,
    predicate: syntra_exec::Int4Predicate,
) -> SummaryRow {
    let attribute = validate_source(source, column);
    heap_summary_checked(source, attribute, heap_snapshot(), predicate)
}

pub(super) fn heap_summary_checked(
    source: &PgRelation,
    attribute: i16,
    snapshot: pg_sys::Snapshot,
    predicate: syntra_exec::Int4Predicate,
) -> SummaryRow {
    let rows = SourceScan::with_predicate(source, attribute, snapshot, predicate);
    let summary = syntra_exec::summarize_int4_where(rows, predicate, || {
        pgrx::check_for_interrupts!();
        true
    })
    .unwrap_or_else(|error| {
        let code = match error {
            syntra_exec::ExecutionError::Aggregate(_) => {
                pgrx::PgSqlErrorCode::ERRCODE_NUMERIC_VALUE_OUT_OF_RANGE
            }
            syntra_exec::ExecutionError::Cancelled => pgrx::PgSqlErrorCode::ERRCODE_QUERY_CANCELED,
            syntra_exec::ExecutionError::Segment(_) => pgrx::PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
        };
        pgrx::ereport!(ERROR, code, error.to_string());
    });
    (
        summary.rows(),
        summary.non_nulls(),
        summary.sum(),
        summary.min(),
        summary.max(),
    )
}

pub(super) fn encode_source(
    source: &PgRelation,
    attribute: i16,
    snapshot: pg_sys::Snapshot,
) -> Result<(Vec<u8>, u32), SegmentError> {
    syntra_storage::encode_int4_with_row_count(SourceScan::new(source, attribute, snapshot), || {
        pgrx::check_for_interrupts!();
        true
    })
}

/// Fresh aggregate over the caller's snapshot. No segment or cache is created.
#[allow(clippy::type_complexity)]
#[pg_extern(stable, parallel_unsafe)]
fn syntra_heap_int4_summary(
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
    ),
> {
    TableIterator::new(std::iter::once(heap_summary(
        &source,
        column,
        syntra_exec::Int4Predicate::All,
    )))
}

/// Inclusive BETWEEN semantics, with interrupt checks on all examined rows.
#[allow(clippy::type_complexity)]
#[pg_extern(stable, parallel_unsafe)]
fn syntra_heap_int4_range_summary(
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
    ),
> {
    TableIterator::new(std::iter::once(heap_summary(
        &source,
        column,
        syntra_exec::Int4Predicate::Range { lower, upper },
    )))
}

/// Current locked storage identity, not an MVCC catalog row or a freshness proof.
/// Relcache invalidation makes storage replacement observable to old snapshots.
#[allow(clippy::type_complexity)]
#[pg_extern(volatile, parallel_unsafe)]
fn syntra_source_identity(
    source: PgRelation,
    column: &str,
) -> TableIterator<
    'static,
    (
        name!(cluster_identity, String),
        name!(source_database, pg_sys::Oid),
        name!(source_relation, pg_sys::Oid),
        name!(storage_tablespace, pg_sys::Oid),
        name!(storage_database, Option<pg_sys::Oid>),
        name!(storage_file, pg_sys::Oid),
        name!(source_attribute, i16),
        name!(source_type, pg_sys::Oid),
        name!(type_modifier, i32),
        name!(source_collation, Option<pg_sys::Oid>),
        name!(access_method, pg_sys::Oid),
    ),
> {
    let attribute = validate_source(&source, column);
    let descriptor = source.tuple_desc();
    let field = descriptor
        .iter()
        .find(|field| field.attnum == attribute)
        .unwrap_or_else(|| pgrx::error!("locked source attribute disappeared"));
    // SAFETY: PgRelation holds AccessShareLock, preventing a concurrent rewrite
    // until this transaction releases it. Copy only scalar descriptor/locator
    // fields. The cluster control identifier is initialized before backend use.
    let (cluster, database, locator, method) = unsafe {
        (
            pg_sys::GetSystemIdentifier().to_string(),
            pg_sys::MyDatabaseId,
            source.rd_locator,
            (*source.rd_rel).relam,
        )
    };
    TableIterator::new(std::iter::once((
        cluster,
        database,
        source.oid(),
        locator.spcOid,
        (locator.dbOid != pg_sys::InvalidOid).then_some(locator.dbOid),
        locator.relNumber,
        attribute,
        field.atttypid,
        field.atttypmod,
        (field.attcollation != pg_sys::InvalidOid).then_some(field.attcollation),
        method,
    )))
}

/// Capture directly from a checked source. This fixed artifact is never eligible
/// for automatic live-table substitution, regardless of matching snapshot text.
// Concrete named columns are required by pgrx schema generation.
#[allow(clippy::type_complexity)]
#[pg_extern(stable, parallel_unsafe)]
fn syntra_capture_int4(
    source: PgRelation,
    column: &str,
) -> TableIterator<
    'static,
    (
        name!(segment, Vec<u8>),
        name!(source_database, pg_sys::Oid),
        name!(source_relation, pg_sys::Oid),
        name!(source_schema, String),
        name!(source_table, String),
        name!(source_column, String),
        name!(source_attribute, i16),
        name!(source_type, pg_sys::Oid),
        name!(source_snapshot, String),
        name!(source_transaction, Option<String>),
        name!(source_command, i64),
        name!(captured_at, TimestampWithTimeZone),
        name!(source_rows, i64),
    ),
> {
    let attribute = validate_source(&source, column);
    let provenance = provenance();
    let rows = SourceScan::new(&source, attribute, provenance.active_snapshot);
    let (segment, count) = syntra_storage::encode_int4_with_row_count(rows, || {
        pgrx::check_for_interrupts!();
        true
    })
    .unwrap_or_else(|error| {
        let code = match error {
            SegmentError::LimitExceeded => pgrx::PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            SegmentError::AllocationFailed => pgrx::PgSqlErrorCode::ERRCODE_OUT_OF_MEMORY,
            _ => pgrx::PgSqlErrorCode::ERRCODE_QUERY_CANCELED,
        };
        pgrx::ereport!(ERROR, code, error.to_string());
    });
    TableIterator::new(std::iter::once((
        segment,
        provenance.database,
        source.oid(),
        source.namespace().to_owned(),
        source.name().to_owned(),
        column.to_owned(),
        attribute,
        pg_sys::INT4OID,
        provenance.snapshot,
        provenance.transaction,
        provenance.command,
        provenance.captured_at,
        i64::from(count),
    )))
}
