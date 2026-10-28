//! DDL epochs fence live bindings and transactional capture configurations.
use pgrx::prelude::*;
use std::ffi::CStr;

/// # Safety
/// fcinfo must be a live PostgreSQL call frame supplied to a guarded V1 wrapper.
unsafe fn writable_event(fcinfo: pg_sys::FunctionCallInfo, expected: &CStr) -> bool {
    // SAFETY: pgrx supplies fcinfo on PostgreSQL's backend main thread. Validate
    // the event context before casting; no context pointer escapes this call.
    unsafe {
        if fcinfo.is_null()
            || (*fcinfo).context.is_null()
            || (*(*fcinfo).context).type_ != pg_sys::NodeTag::T_EventTriggerData
        {
            pgrx::error!("DDL epoch guard requires an event trigger context");
        }
        let event = &*((*fcinfo).context.cast::<pg_sys::EventTriggerData>());
        if event.event.is_null() || CStr::from_ptr(event.event) != expected {
            pgrx::error!("DDL epoch guard received the wrong event");
        }
        // Read-only maintenance, such as REINDEX, can fire event triggers.
        // It cannot perform the permanent source DDL guarded by these epochs.
        // Physical storage replacement has a separate locked identity guard.
        !pg_sys::XactReadOnly
    }
}

// The event_trigger SQL signatures are declared in the final extension SQL block.
// pgrx generates the guarded V1 wrappers and their pg_finfo entry points.
#[pg_extern(sql = false)]
fn syntra_ddl_epoch_guard(fcinfo: pg_sys::FunctionCallInfo) {
    // SAFETY: this guarded wrapper receives fcinfo directly from PostgreSQL.
    if !unsafe { writable_event(fcinfo, c"ddl_command_end") } {
        return;
    }
    // Addresses come from PostgreSQL's current DDL command list. Touch only the
    // affected registered sources; unrelated temporary DDL must not serialize all
    // tracked writers. Trigger addresses map through the post-command catalog.
    Spi::run(
        r#"WITH commands AS MATERIALIZED (
        SELECT classid, objid FROM pg_catalog.pg_event_trigger_ddl_commands()
    ), affected AS (
        SELECT objid AS relation FROM commands
        WHERE classid = 'pg_catalog.pg_class'::regclass::oid
        UNION
        SELECT t.tgrelid AS relation FROM commands AS c
        JOIN pg_catalog.pg_trigger AS t ON t.oid = c.objid
        WHERE c.classid = 'pg_catalog.pg_trigger'::regclass::oid
    ) UPDATE syntra.tracked_sources AS s SET epoch = DEFAULT
      FROM affected AS a WHERE s.source_relation = a.relation"#,
    )
    .unwrap_or_else(|error| pgrx::error!("{}", error));
}

#[pg_extern(sql = false)]
fn syntra_ddl_drop_guard(fcinfo: pg_sys::FunctionCallInfo) {
    // SAFETY: this guarded wrapper receives fcinfo directly from PostgreSQL.
    if !unsafe { writable_event(fcinfo, c"sql_drop") } {
        return;
    }
    // Delete dropped sources before updating dropped-guard epochs, so a source
    // removed together with its trigger is never modified twice by one command.
    Spi::run(
        r#"DELETE FROM syntra.capture_chunks AS p
        USING pg_catalog.pg_event_trigger_dropped_objects() AS d
        WHERE d.classid = 'pg_catalog.pg_class'::regclass::oid AND d.objid = p.source_relation"#,
    )
    .unwrap_or_else(|e| pgrx::error!("{}", e));
    Spi::run(
        r#"DELETE FROM syntra.capture_checkpoints AS p
        USING pg_catalog.pg_event_trigger_dropped_objects() AS d
        WHERE d.classid = 'pg_catalog.pg_class'::regclass::oid AND d.objid = p.source_relation;
        DELETE FROM syntra.capture_events AS p
        USING pg_catalog.pg_event_trigger_dropped_objects() AS d
        WHERE d.classid = 'pg_catalog.pg_class'::regclass::oid AND d.objid = p.source_relation;
        DELETE FROM syntra.capture_marks AS p
        USING pg_catalog.pg_event_trigger_dropped_objects() AS d
        WHERE d.classid = 'pg_catalog.pg_class'::regclass::oid AND d.objid = p.source_relation;
        DELETE FROM syntra.capture_base_rows AS p
        USING pg_catalog.pg_event_trigger_dropped_objects() AS d
        WHERE d.classid = 'pg_catalog.pg_class'::regclass::oid AND d.objid = p.source_relation;
        DELETE FROM syntra.capture_heads AS p
        USING pg_catalog.pg_event_trigger_dropped_objects() AS d
        WHERE d.classid = 'pg_catalog.pg_class'::regclass::oid AND d.objid = p.source_relation;
        DELETE FROM syntra.trusted_int4_projections AS p
        USING pg_catalog.pg_event_trigger_dropped_objects() AS d
        WHERE d.classid = 'pg_catalog.pg_class'::regclass::oid AND d.objid = p.source_relation;
        DELETE FROM syntra.source_generation_shards AS g
        USING pg_catalog.pg_event_trigger_dropped_objects() AS d
        WHERE d.classid = 'pg_catalog.pg_class'::regclass::oid AND d.objid = g.source_relation;
        DELETE FROM syntra.tracked_sources AS s
        USING pg_catalog.pg_event_trigger_dropped_objects() AS d
        WHERE d.classid = 'pg_catalog.pg_class'::regclass::oid AND d.objid = s.source_relation;
        UPDATE syntra.tracked_sources AS s SET epoch = DEFAULT
        FROM pg_catalog.pg_event_trigger_dropped_objects() AS d
        WHERE d.classid = 'pg_catalog.pg_trigger'::regclass::oid AND d.objid = s.trigger_oid"#,
    )
    .unwrap_or_else(|error| pgrx::error!("{}", error));
}
