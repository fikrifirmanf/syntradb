//! The only manual FFI in the spike. Observe path planning without adding paths.
use pgrx::{pg_guard, pg_sys};
use std::sync::atomic::{AtomicI64, Ordering};

static CALLS: AtomicI64 = AtomicI64::new(0);
static mut PREVIOUS: pg_sys::set_rel_pathlist_hook_type = None;
static mut INSTALLED: bool = false;

/// # Safety
/// Must run exactly on the owning PostgreSQL backend's main thread.
pub unsafe fn install() {
    // SAFETY: PostgreSQL backend initialization and planning are single-threaded.
    // The guard avoids chaining this callback to itself on repeated initialization.
    unsafe {
        if !INSTALLED {
            PREVIOUS = pg_sys::set_rel_pathlist_hook;
            pg_sys::set_rel_pathlist_hook = Some(observe_paths);
            INSTALLED = true;
        }
    }
}

pub fn calls() -> i64 {
    CALLS.load(Ordering::Relaxed)
}

#[pg_guard]
unsafe extern "C-unwind" fn observe_paths(
    root: *mut pg_sys::PlannerInfo,
    relation: *mut pg_sys::RelOptInfo,
    index: pg_sys::Index,
    entry: *mut pg_sys::RangeTblEntry,
) {
    // Saturate diagnostic state rather than introducing a planner error on wrap.
    let _ = CALLS.try_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
        Some(n.saturating_add(1))
    });
    // SAFETY: PostgreSQL supplies these pointers for this hook invocation. We do
    // not dereference or retain them; the previous hook receives them unchanged.
    unsafe {
        if let Some(previous) = PREVIOUS {
            previous(root, relation, index, entry);
        }
    }
}
