//! Backend-local buffer for captured row changes, flushed once per transaction.
//!
//! The capture trigger used to run two SQL statements for every changed row. A
//! deferred firing at top-level commit now records its change here, and a
//! pre-commit callback writes them all with one insert and one mark per source
//! (docs/capture-batching-design.md). The head row lock taken by the first firing
//! is held to commit as before, so ordinals stay gap-free and follow commit order.
//!
//! Only firings that happen at commit, outside any statement, are buffered. Any
//! firing inside a statement or subtransaction (immediate triggers, `SET
//! CONSTRAINTS ... IMMEDIATE`) keeps the per-firing path, so its events are
//! visible to the writing session as soon as the statement ends.
use pgrx::prelude::*;
use std::{cell::Cell, cell::RefCell, ffi::c_void};

pub(super) type Side = Option<Option<i64>>;

struct Change {
    key: i64,
    before: Side,
    after: Side,
    command: i64,
}

struct Batch {
    source: pg_sys::Oid,
    epoch: i64,
    /// Last reserved ordinal when the head row was locked.
    base: i64,
    limit: i64,
    live: i64,
    changes: Vec<Change>,
}

thread_local! {
    static BATCHES: RefCell<Vec<Batch>> = const { RefCell::new(Vec::new()) };
    static REGISTERED: Cell<bool> = const { Cell::new(false) };
}

/// True when the trigger is running as a deferred firing at top-level commit: no
/// statement is executing and no subtransaction is open. A deferred trigger that
/// `SET CONSTRAINTS IMMEDIATE` fires inside a statement, which has a portal, and
/// so does a procedure's own COMMIT; both take the per-firing path.
pub(super) fn at_commit(source: pg_sys::Oid) -> bool {
    // SAFETY: both are plain backend-local reads on the main thread.
    let at_top = unsafe { pg_sys::GetCurrentTransactionNestLevel() == 1 };
    let idle = unsafe { pg_sys::ActivePortal.is_null() };
    if at_top && idle {
        return true;
    }
    // A buffered source never mixes with another path in one transaction.
    if BATCHES.with(|b| b.borrow().iter().any(|b| b.source == source)) {
        crate::capture::stale();
    }
    false
}

/// Where the next ordinal and live count stand for `source`, when this
/// transaction already holds its head lock: (emitted, live, limit).
pub(super) fn state(source: pg_sys::Oid, epoch: i64) -> Option<(i64, i64, i64)> {
    BATCHES.with(|batches| {
        batches
            .borrow()
            .iter()
            .find(|b| b.source == source)
            .map(|b| {
                if b.epoch != epoch {
                    crate::capture::stale();
                }
                (b.base + b.changes.len() as i64, b.live, b.limit)
            })
    })
}

/// Start buffering for `source` after its head row was locked and its position read.
pub(super) fn begin(source: pg_sys::Oid, epoch: i64, base: i64, live: i64, limit: i64) {
    register();
    BATCHES.with(|batches| {
        batches.borrow_mut().push(Batch {
            source,
            epoch,
            base,
            limit,
            live,
            changes: Vec::new(),
        })
    });
}

/// Record one firing's changes. `row_delta` is the firing's live-row change.
pub(super) fn record(
    source: pg_sys::Oid,
    changes: &[(i64, Side, Side)],
    command: i64,
    row_delta: i64,
) {
    BATCHES.with(|batches| {
        let mut batches = batches.borrow_mut();
        let batch = batches
            .iter_mut()
            .find(|b| b.source == source)
            .unwrap_or_else(|| crate::capture::stale());
        batch.live += row_delta;
        for (key, before, after) in changes {
            batch.changes.push(Change {
                key: *key,
                before: *before,
                after: *after,
                command,
            });
        }
    });
}

fn register() {
    if REGISTERED.with(|r| r.replace(true)) {
        return;
    }
    // SAFETY: the callback is a plain function that lives as long as the library,
    // and registration happens on the backend main thread.
    unsafe { pg_sys::RegisterXactCallback(Some(xact_callback), std::ptr::null_mut()) };
}

#[pg_guard]
unsafe extern "C-unwind" fn xact_callback(event: pg_sys::XactEvent::Type, _arg: *mut c_void) {
    use pg_sys::XactEvent::*;
    match event {
        XACT_EVENT_PRE_COMMIT | XACT_EVENT_PRE_PREPARE => flush(),
        XACT_EVENT_COMMIT | XACT_EVENT_PREPARE | XACT_EVENT_ABORT | XACT_EVENT_PARALLEL_ABORT => {
            BATCHES.with(|batches| batches.borrow_mut().clear())
        }
        _ => {}
    }
}

/// Write every buffered change, one insert and one mark per source.
fn flush() {
    let batches = BATCHES.with(|batches| std::mem::take(&mut *batches.borrow_mut()));
    if batches.is_empty() {
        return;
    }
    // No statement is running at pre-commit, so SPI has no snapshot until one is
    // pushed. An error leaves it for transaction abort to release.
    // SAFETY: balanced with the pop below on the same backend thread.
    unsafe { pg_sys::PushActiveSnapshot(pg_sys::GetTransactionSnapshot()) };
    for batch in batches {
        let count = batch.changes.len() as i64;
        if count == 0 {
            continue;
        }
        let keys: Vec<i64> = batch.changes.iter().map(|c| c.key).collect();
        let before: Vec<bool> = batch.changes.iter().map(|c| c.before.is_some()).collect();
        let old: Vec<Option<i64>> = batch.changes.iter().map(|c| c.before.flatten()).collect();
        let after: Vec<bool> = batch.changes.iter().map(|c| c.after.is_some()).collect();
        let new: Vec<Option<i64>> = batch.changes.iter().map(|c| c.after.flatten()).collect();
        let commands: Vec<i64> = batch.changes.iter().map(|c| c.command).collect();
        if batch.base > batch.limit - count
            || batch.base > i64::MAX - count
            || !(0..=1_000_000).contains(&batch.live)
        {
            crate::capture::full();
        }
        let inserted = Spi::connect_mut(|client| {
            client
                .update(
                    crate::capture::append_plan()?,
                    Some(1),
                    &[
                        batch.source.into(),
                        batch.epoch.into(),
                        batch.base.into(),
                        keys.into(),
                        before.into(),
                        old.into(),
                        after.into(),
                        new.into(),
                        commands.into(),
                        count.into(),
                        batch.live.into(),
                    ],
                )?
                .first()
                .get_one::<i64>()
        })
        .unwrap_or_else(|e: pgrx::spi::Error| pgrx::error!("{}", e));
        if inserted != Some(count) {
            crate::capture::full();
        }
    }
    // SAFETY: pops the snapshot pushed above.
    unsafe { pg_sys::PopActiveSnapshot() };
}
