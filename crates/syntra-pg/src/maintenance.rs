//! Optional background maintenance: apply and prune every captured source.
//!
//! Off unless the library is in `shared_preload_libraries` and
//! `syntradb.maintenance_database` names one or more databases, one worker each.
//! Each source is maintained in its own transaction, so one source's locks never
//! wait on another's work.
use pgrx::bgworkers::{
    BackgroundWorker, BackgroundWorkerBuilder, BgWorkerStartTime, SignalWakeFlags,
};
use pgrx::guc::{GucContext, GucFlags, GucRegistry, GucSetting};
use pgrx::prelude::*;
use std::ffi::CString;
use std::time::Duration;

static DATABASE: GucSetting<Option<CString>> = GucSetting::<Option<CString>>::new(None);
static INTERVAL_MS: GucSetting<i32> = GucSetting::<i32>::new(1000);

/// Called from `_PG_init`. PostgreSQL accepts postmaster settings only during
/// preload, so a library loaded later by `CREATE EXTENSION` defines none.
pub(crate) fn install() {
    // SAFETY: read-only check of a postmaster global during module initialization.
    if !unsafe { pg_sys::process_shared_preload_libraries_in_progress } {
        return;
    }
    GucRegistry::define_string_guc(
        c"syntradb.maintenance_database",
        c"Comma-separated databases whose captured sources SyntraDB workers apply and prune.",
        c"One worker per database, at most 8. Empty disables them. Takes effect at server start; requires syntradb in shared_preload_libraries.",
        &DATABASE,
        GucContext::Postmaster,
        GucFlags::default(),
    );
    GucRegistry::define_int_guc(
        c"syntradb.maintenance_interval_ms",
        c"Pause between SyntraDB maintenance passes.",
        c"Each pass applies and prunes every captured source with pending events.",
        &INTERVAL_MS,
        100,
        3_600_000,
        GucContext::Sighup,
        GucFlags::UNIT_MS,
    );
    let setting = DATABASE
        .get()
        .map(|d| d.to_string_lossy().into_owned())
        .unwrap_or_default();
    // A malformed list stops the server from starting rather than running some
    // databases unmaintained.
    let databases = parse_databases(&setting)
        .unwrap_or_else(|e| pgrx::error!("invalid syntradb.maintenance_database: {}", e));
    for database in databases {
        BackgroundWorkerBuilder::new(&format!("syntradb maintenance {database}"))
            .set_type("syntradb maintenance")
            .set_function("syntra_maintenance_main")
            .set_library("syntradb")
            .set_extra(database)
            .enable_spi_access()
            .set_start_time(BgWorkerStartTime::RecoveryFinished)
            .set_restart_time(Some(Duration::from_secs(10)))
            .load();
    }
}

/// Splits the setting on commas and trims spaces. Names are taken literally, so a
/// database whose name contains a comma or edge spaces cannot be listed.
pub(crate) fn parse_databases(setting: &str) -> Result<Vec<&str>, &'static str> {
    if setting.trim().is_empty() {
        return Ok(Vec::new());
    }
    let mut databases = Vec::new();
    for name in setting.split(',').map(str::trim) {
        if name.is_empty() {
            return Err("empty database name");
        }
        if name.len() > 63 {
            return Err("database name longer than 63 bytes");
        }
        if databases.contains(&name) {
            return Err("database listed twice");
        }
        databases.push(name);
    }
    if databases.len() > 8 {
        return Err("more than 8 databases");
    }
    Ok(databases)
}

/// One source per transaction. A busy maintenance lock means another caller is
/// already maintaining; any other error is logged and the next source still runs.
fn maintain_sql(source: pg_sys::Oid) -> String {
    let oid = u32::from(source);
    format!(
        "DO $syntra$ BEGIN
            IF (SELECT applied IS NULL OR pending_events > 0 FROM public.syntra_capture_lag({oid}::oid::regclass)) THEN
                PERFORM public.syntra_apply_prune_capture({oid}::oid::regclass);
            END IF;
        EXCEPTION
            WHEN lock_not_available THEN NULL;
            WHEN OTHERS THEN RAISE WARNING 'syntradb maintenance skipped relation {oid}: %', SQLERRM;
        END $syntra$"
    )
}

#[pg_guard]
#[no_mangle]
pub extern "C-unwind" fn syntra_maintenance_main(_arg: pg_sys::Datum) {
    BackgroundWorker::attach_signal_handlers(SignalWakeFlags::SIGHUP | SignalWakeFlags::SIGTERM);
    let database = BackgroundWorker::get_extra().to_owned();
    BackgroundWorker::connect_worker_to_spi(Some(&database), None);
    log!("syntradb maintenance started in database \"{}\"", database);
    'worker: while BackgroundWorker::wait_latch(Some(Duration::from_millis(
        INTERVAL_MS.get() as u64
    ))) {
        if BackgroundWorker::sighup_received() {
            // SAFETY: the worker's main loop, outside any transaction.
            unsafe { pg_sys::ProcessConfigFile(pg_sys::GucContext::PGC_SIGHUP) };
        }
        let sources = BackgroundWorker::transaction(|| {
            let installed =
                Spi::get_one::<bool>("SELECT to_regclass('syntra.capture_heads') IS NOT NULL")
                    .unwrap_or_else(|e| pgrx::error!("{}", e))
                    .unwrap_or(false);
            if !installed {
                return Vec::new();
            }
            Spi::connect(|client| {
                client
                    .select(
                        "SELECT source_relation FROM syntra.capture_heads ORDER BY source_relation",
                        None,
                        &[],
                    )
                    .unwrap_or_else(|e| pgrx::error!("{}", e))
                    .map(|row| {
                        row.get::<pg_sys::Oid>(1)
                            .ok()
                            .flatten()
                            .unwrap_or_else(|| pgrx::error!("invalid capture head"))
                    })
                    .collect::<Vec<_>>()
            })
        });
        for source in sources {
            if BackgroundWorker::sigterm_received() {
                // This accessor consumes the signal flag. Exit the outer loop
                // as well, so wait_latch cannot miss the shutdown request.
                break 'worker;
            }
            BackgroundWorker::transaction(|| {
                // The PgRelation argument adapter can wait on a source's DDL
                // lock before maintenance admission runs. Bound every lock wait
                // in this transaction so one source cannot starve the others.
                // PostgreSQL reports a lock timeout as lock_not_available;
                // maintain_sql catches it and rolls back that source's work.
                Spi::run("SET LOCAL lock_timeout = '1s'").unwrap_or_else(|e| pgrx::error!("{}", e));
                // unslop-ignore NO_SQL_INTERPOLATION -- the only value is a catalog OID formatted as an integer; DO takes no bind parameters.
                Spi::run(&maintain_sql(source)).unwrap_or_else(|e| pgrx::error!("{}", e));
            });
        }
    }
    log!("syntradb maintenance stopped");
}
