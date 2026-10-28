//! Internal indexed reads with an explicit snapshot. No SPI or caller ACL bypass
//! is exposed through SQL; live readers validate source access before using this.
use pgrx::prelude::*;
use pgrx::PgRelation;
use std::ffi::CStr;

pub(super) struct Key {
    pub attribute: i16,
    pub name: &'static str,
    pub kind: pg_sys::Oid,
    pub procedure: u32,
    pub strategy: u16,
    pub value: pg_sys::Datum,
}

pub(super) struct CatalogScan {
    relation: PgRelation,
    scan: pg_sys::SysScanDesc,
}

impl CatalogScan {
    /// NULL snapshots are used only for current PostgreSQL catalog metadata.
    /// Live reuse uses the caller's exact active snapshot. Capture configuration
    /// checks explicitly use a latest snapshot to reject stale DDL registrations.
    /// Non-NULL snapshots must stay active or registered until the scan is dropped.
    pub fn open(
        schema: &CStr,
        table: &CStr,
        index: &CStr,
        snapshot: pg_sys::Snapshot,
        keys: &[Key],
    ) -> Option<Self> {
        // SAFETY: backend thread; all identifiers are internal static C strings.
        // relation_open supplies the lock and descriptor until the scan is ended.
        unsafe {
            let namespace = pg_sys::get_namespace_oid(schema.as_ptr(), true);
            if namespace == pg_sys::InvalidOid {
                return None;
            }
            let oid = pg_sys::get_relname_relid(table.as_ptr(), namespace);
            if oid == pg_sys::InvalidOid {
                return None;
            }
            let relation = PgRelation::with_lock(oid, pg_sys::AccessShareLock as i32);
            let descriptor = relation.tuple_desc();
            let mut native_keys = Vec::with_capacity(keys.len());
            for key in keys {
                let field = descriptor.iter().find(|a| a.attnum == key.attribute)?;
                if field.attisdropped
                    || field.atttypid != key.kind
                    || pgrx::name_data_to_str(&field.attname) != key.name
                {
                    return None;
                }
                let mut native = pg_sys::ScanKeyData::default();
                pg_sys::ScanKeyInit(
                    &mut native,
                    key.attribute,
                    key.strategy,
                    pg_sys::Oid::from(key.procedure),
                    key.value,
                );
                native_keys.push(native);
            }
            drop(descriptor);
            let index = pg_sys::get_relname_relid(index.as_ptr(), namespace);
            // systable_beginscan remaps heap attributes but requires the resulting
            // btree keys to be ordered. Hold the index lock across scan creation.
            let index_relation = if index != pg_sys::InvalidOid {
                Some(PgRelation::with_lock(index, pg_sys::AccessShareLock as i32))
            } else {
                None
            };
            if let Some(index_relation) = &index_relation {
                let definition = index_relation.rd_index;
                if definition.is_null() {
                    return None;
                }
                let count = usize::try_from((*definition).indnkeyatts).ok()?;
                if count == 0 || count > 32 {
                    return None;
                }
                let attributes = (*definition).indkey.values.as_slice(count);
                if native_keys
                    .iter()
                    .any(|k| !attributes.contains(&k.sk_attno))
                {
                    return None;
                }
                native_keys.sort_by_key(|k| attributes.iter().position(|a| *a == k.sk_attno));
            }
            let scan = pg_sys::systable_beginscan(
                relation.as_ptr(),
                index,
                index != pg_sys::InvalidOid,
                snapshot,
                native_keys.len() as i32,
                if native_keys.is_empty() {
                    std::ptr::null_mut()
                } else {
                    native_keys.as_mut_ptr()
                },
            );
            Some(Self { relation, scan })
        }
    }

    /// Reuse the pinned relation/index/snapshot for bounded exact lookups.
    /// No tuple borrow may survive a rescan. Missing or non-index scans fail closed.
    pub fn rescan(&mut self, keys: &[Key]) -> Option<()> {
        pgrx::check_for_interrupts!();
        // SAFETY: this backend owns the scan. Validate table fields and remap
        // heap attributes to real index key positions as systable_beginscan does.
        // index_rescan copies key data; ExecClearTuple releases the old slot pin.
        unsafe {
            let scan = &*self.scan;
            if scan.irel.is_null()
                || scan.iscan.is_null()
                || (*scan.iscan).numberOfKeys != keys.len() as i32
            {
                return None;
            }
            let definition = (*scan.irel).rd_index;
            if definition.is_null()
                || !(*definition).indisvalid
                || !(*definition).indisready
                || !(*definition).indislive
            {
                return None;
            }
            let count = usize::try_from((*definition).indnkeyatts).ok()?;
            if count == 0 || count > 32 {
                return None;
            }
            let attributes = (*definition).indkey.values.as_slice(count);
            let descriptor = self.relation.tuple_desc();
            let mut native_keys = Vec::with_capacity(keys.len());
            for key in keys {
                let field = descriptor.iter().find(|a| a.attnum == key.attribute)?;
                if field.attisdropped
                    || field.atttypid != key.kind
                    || pgrx::name_data_to_str(&field.attname) != key.name
                {
                    return None;
                }
                let index_attribute = attributes.iter().position(|a| *a == key.attribute)? + 1;
                let mut native = pg_sys::ScanKeyData::default();
                pg_sys::ScanKeyInit(
                    &mut native,
                    index_attribute as i16,
                    key.strategy,
                    pg_sys::Oid::from(key.procedure),
                    key.value,
                );
                native_keys.push(native);
            }
            drop(descriptor);
            native_keys.sort_by_key(|key| key.sk_attno);
            pg_sys::ExecClearTuple(scan.slot);
            pg_sys::index_rescan(
                scan.iscan,
                native_keys.as_mut_ptr(),
                native_keys.len() as i32,
                std::ptr::null_mut(),
                0,
            );
        }
        Some(())
    }

    /// Reverse index traversal is a locality hint for unique-key metadata reads.
    /// MVCC, not tuple position, selects the visible version. A missing index
    /// keeps the normal heap path. Retained snapshots may scan more entries.
    pub fn next_back(&mut self) -> Option<Row<'_>> {
        pgrx::check_for_interrupts!();
        // SAFETY: systable_beginscan initializes the same index/slot fields used
        // by systable_getnext_ordered. Use it only when an index scan exists;
        // the ordinary systable_endscan still owns and releases that index.
        // Both APIs enforce the scan's original snapshot and lossy-key checks.
        // Row borrows the pinned slot until the next scan call or drop.
        unsafe {
            if (*self.scan).irel.is_null() {
                return self.next();
            }
            let tuple = pg_sys::systable_getnext_ordered(
                self.scan,
                pg_sys::ScanDirection::BackwardScanDirection,
            );
            if tuple.is_null() {
                None
            } else {
                Some(Row {
                    tuple,
                    relation: &self.relation,
                })
            }
        }
    }

    pub fn next(&mut self) -> Option<Row<'_>> {
        pgrx::check_for_interrupts!();
        // SAFETY: one backend owns this live scan. Tuple storage stays pinned
        // until the next call or Drop; Row's borrow cannot outlive the scan.
        let tuple = unsafe { pg_sys::systable_getnext(self.scan) };
        if tuple.is_null() {
            None
        } else {
            Some(Row {
                tuple,
                relation: &self.relation,
            })
        }
    }
}

impl Drop for CatalogScan {
    fn drop(&mut self) {
        // SAFETY: stop the scan before dropping its locked relation guard.
        unsafe { pg_sys::systable_endscan(self.scan) };
    }
}

pub(super) struct Row<'a> {
    tuple: pg_sys::HeapTuple,
    relation: &'a PgRelation,
}

// Only owned output types may leave a tuple borrow. In particular a borrowed
// Array or str must never survive systable_getnext or systable_endscan.
pub(super) trait OwnedField: FromDatum + IntoDatum {}
impl OwnedField for pg_sys::Oid {}
impl OwnedField for i64 {}
impl OwnedField for i32 {}
impl OwnedField for bool {}
impl OwnedField for i16 {}
impl OwnedField for i8 {}
impl OwnedField for String {}
impl OwnedField for Vec<u8> {}
impl OwnedField for Vec<Option<i64>> {}

impl Row<'_> {
    pub fn name(&self, attribute: i16, name: &str) -> Option<String> {
        let descriptor = self.relation.tuple_desc();
        let field = descriptor.iter().find(|a| a.attnum == attribute)?;
        if field.attisdropped
            || field.atttypid != pg_sys::NAMEOID
            || pgrx::name_data_to_str(&field.attname) != name
        {
            return None;
        }
        // SAFETY: fixed-size NameData belongs to this pinned tuple. Copy it
        // before advancing the scan; name_data_to_str respects NAMEDATALEN.
        unsafe {
            let mut null = false;
            let datum = pg_sys::heap_getattr(
                self.tuple,
                i32::from(attribute),
                descriptor.as_ptr(),
                &mut null,
            );
            if null {
                None
            } else {
                Some(pgrx::name_data_to_str(&*datum.cast_mut_ptr::<pg_sys::NameData>()).to_owned())
            }
        }
    }

    pub fn is_null(&self, attribute: i16, name: &str, kind: pg_sys::Oid) -> bool {
        let descriptor = self.relation.tuple_desc();
        let Some(field) = descriptor.iter().find(|a| a.attnum == attribute) else {
            return false;
        };
        if field.attisdropped
            || field.atttypid != kind
            || pgrx::name_data_to_str(&field.attname) != name
        {
            return false;
        }
        // SAFETY: attribute and SQL type checked against this live descriptor.
        unsafe {
            let mut null = false;
            pg_sys::heap_getattr(
                self.tuple,
                i32::from(attribute),
                descriptor.as_ptr(),
                &mut null,
            );
            null
        }
    }

    /// Reads a bytea only when its stored logical length equals `len`. The size
    /// comes from the varlena or TOAST pointer header, so a mismatch is rejected
    /// before decompression, external fetch or copying into Rust memory.
    pub fn bytes_exact(&self, attribute: i16, name: &str, len: usize) -> Option<Vec<u8>> {
        let descriptor = self.relation.tuple_desc();
        let field = descriptor.iter().find(|a| a.attnum == attribute)?;
        if field.attisdropped
            || field.atttypid != pg_sys::BYTEAOID
            || pgrx::name_data_to_str(&field.attname) != name
        {
            return None;
        }
        // SAFETY: tuple belongs to this live descriptor and scan; the SQL type is
        // checked above and toast_raw_datum_size only inspects the varlena header.
        unsafe {
            let mut null = false;
            let datum = pg_sys::heap_getattr(
                self.tuple,
                i32::from(attribute),
                descriptor.as_ptr(),
                &mut null,
            );
            if null || pg_sys::toast_raw_datum_size(datum) != len.checked_add(pg_sys::VARHDRSZ)? {
                return None;
            }
            Vec::<u8>::try_from_datum(datum, false, field.atttypid)
                .ok()
                .flatten()
                .filter(|bytes| bytes.len() == len)
        }
    }

    pub fn get<T: OwnedField>(&self, attribute: i16, name: &str) -> Option<T> {
        let descriptor = self.relation.tuple_desc();
        let field = descriptor.iter().find(|a| a.attnum == attribute)?;
        if field.attisdropped || pgrx::name_data_to_str(&field.attname) != name {
            return None;
        }
        // SAFETY: tuple belongs to this live descriptor and scan. Check the SQL
        // type before conversion. Variable-sized internal fields have bounds
        // checked before detoasting/copying into Rust memory.
        unsafe {
            let mut null = false;
            let datum = pg_sys::heap_getattr(
                self.tuple,
                i32::from(attribute),
                descriptor.as_ptr(),
                &mut null,
            );
            if null {
                return None;
            }
            let limit = if field.atttypid == pg_sys::BYTEAOID {
                8 * 1024 * 1024 + 4
            } else if field.atttypid == pg_sys::TEXTOID {
                516
            } else if field.atttypid == pg_sys::INT8ARRAYOID {
                256
            } else {
                usize::MAX
            };
            if limit != usize::MAX && pg_sys::toast_raw_datum_size(datum) > limit {
                return None;
            }
            T::try_from_datum(datum, false, field.atttypid)
                .ok()
                .flatten()
        }
    }
}

pub(super) fn oid_key(attribute: i16, name: &'static str, value: pg_sys::Oid) -> Key {
    Key {
        attribute,
        name,
        kind: pg_sys::OIDOID,
        procedure: pg_sys::F_OIDEQ,
        strategy: pg_sys::BTEqualStrategyNumber as u16,
        value: value.into(),
    }
}

pub(super) fn int2_key(attribute: i16, name: &'static str, value: i16) -> Key {
    Key {
        attribute,
        name,
        kind: pg_sys::INT2OID,
        procedure: pg_sys::F_INT2EQ,
        strategy: pg_sys::BTEqualStrategyNumber as u16,
        value: value.into(),
    }
}

pub(super) enum Int8Comparison {
    Equal,
    After,
    Through,
}
pub(super) fn int8_key(
    attribute: i16,
    name: &'static str,
    value: i64,
    comparison: Int8Comparison,
) -> Key {
    let (strategy, procedure) = match comparison {
        Int8Comparison::Equal => (pg_sys::BTEqualStrategyNumber as u16, pg_sys::F_INT8EQ),
        Int8Comparison::After => (pg_sys::BTGreaterStrategyNumber as u16, pg_sys::F_INT8GT),
        Int8Comparison::Through => (pg_sys::BTLessEqualStrategyNumber as u16, pg_sys::F_INT8LE),
    };
    Key {
        attribute,
        name,
        kind: pg_sys::INT8OID,
        procedure,
        strategy,
        value: value.into(),
    }
}

pub(super) fn function_oid(name: &'static CStr) -> Option<pg_sys::Oid> {
    // SAFETY: static qualified function name, interpreted by PostgreSQL's native
    // regprocedure input routine. It neither executes SQL nor changes snapshots.
    unsafe {
        pgrx::direct_function_call(pg_sys::to_regprocedure, &[name.to_str().ok()?.into_datum()])
    }
}

pub(super) fn guards_current(source: &PgRelation, trigger: pg_sys::Oid) -> bool {
    let Some(procedure) = function_oid(c"public.syntra_source_changed()") else {
        return false;
    };
    // SAFETY: source AccessShareLock protects its descriptor. Relcache metadata
    // reflects current catalog invalidations, even under an older MVCC snapshot.
    let valid = unsafe {
        if !pg_sys::event_triggers || source.trigdesc.is_null() {
            return false;
        }
        let descriptor = &*source.trigdesc;
        (0..descriptor.numtriggers).any(|index| {
            let t = &*descriptor.triggers.add(index as usize);
            t.tgoid == trigger
                && t.tgfoid == procedure
                && t.tgtype == 60
                && t.tgenabled as u8 == b'A'
                && !t.tgisinternal
                && !t.tgisclone
                && t.tgnargs == 0
                && t.tgnattr == 0
                && t.tgqual.is_null()
                && t.tgconstraint == pg_sys::InvalidOid
                && !t.tgdeferrable
                && !t.tginitdeferred
                && t.tgoldtable.is_null()
                && t.tgnewtable.is_null()
        })
    };
    if !valid {
        return false;
    }
    for (name, function, event) in [
        (
            c"syntra_ddl_epoch_guard",
            c"public.syntra_ddl_epoch_guard()",
            "ddl_command_end",
        ),
        (
            c"syntra_ddl_drop_guard",
            c"public.syntra_ddl_drop_guard()",
            "sql_drop",
        ),
    ] {
        // SAFETY: current native catalog lookup with a constant identifier.
        let oid = unsafe { pg_sys::get_event_trigger_oid(name.as_ptr(), true) };
        let Some(function) = function_oid(function) else {
            return false;
        };
        let Some(mut scan) = CatalogScan::open(
            c"pg_catalog",
            c"pg_event_trigger",
            c"pg_event_trigger_oid_index",
            std::ptr::null_mut(),
            &[oid_key(1, "oid", oid)],
        ) else {
            return false;
        };
        let Some(row) = scan.next() else {
            return false;
        };
        if row.name(3, "evtevent").as_deref() != Some(event)
            || row.get::<pg_sys::Oid>(5, "evtfoid") != Some(function)
            || row.get::<i8>(6, "evtenabled") != Some(b'A' as i8)
            || !row.is_null(7, "evttags", pg_sys::TEXTARRAYOID)
        {
            return false;
        }
    }
    true
}

/// Takes `mode` on an internal catalog without queueing behind other holders.
/// VACUUM and ANALYZE hold SHARE UPDATE EXCLUSIVE, which conflicts with the SHARE
/// ROW EXCLUSIVE admission locks, and a conditional request never cancels
/// autovacuum. These catalogs are small, so a visit ends in milliseconds: retry
/// every 10 ms for up to one second before the caller reports itself busy.
///
/// Only vacuum is waited for. A ROW EXCLUSIVE probe conflicts with another
/// admission and with DDL but not with SHARE UPDATE EXCLUSIVE, so when the probe
/// fails another caller holds the catalog and this one is busy at once, as before
/// the retry existed. Waiting on each other made concurrent appliers commit
/// several times as often, and with an old snapshot held every commit leaves row
/// versions that vacuum cannot remove.
pub(super) fn try_admission_lock(relation: pg_sys::Oid, mode: pg_sys::LOCKMODE) -> bool {
    let probe = pg_sys::RowExclusiveLock as pg_sys::LOCKMODE;
    for attempt in 0..100 {
        // SAFETY: backend thread; a granted lock is held until transaction end.
        if unsafe { pg_sys::ConditionalLockRelationOid(relation, mode) } {
            return true;
        }
        // SAFETY: backend thread. The probe is released at once; every caller
        // raises an error when admission fails, which releases any other lock.
        unsafe {
            if !pg_sys::ConditionalLockRelationOid(relation, probe) {
                return false;
            }
            pg_sys::UnlockRelationOid(relation, probe);
        }
        if attempt < 99 {
            // SAFETY: backend thread; plain sleep, no locks or memory involved.
            unsafe { pg_sys::pg_usleep(10_000) };
            pgrx::check_for_interrupts!();
        }
    }
    false
}
