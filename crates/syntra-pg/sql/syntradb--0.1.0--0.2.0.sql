-- SyntraDB 0.1.0 to 0.2.0. Run through ALTER EXTENSION syntradb UPDATE, never directly.
\echo Use "ALTER EXTENSION syntradb UPDATE TO '0.2.0'" to load this file. \quit

-- Block seeds, stops and prunes until this transaction commits. Capture writes
-- do not wait: until commit they find no bindings and fail closed.
LOCK TABLE syntra.capture_heads IN SHARE ROW EXCLUSIVE MODE;

CREATE TABLE syntra.capture_bindings (
    source_relation oid PRIMARY KEY,
    epoch bigint NOT NULL UNIQUE,
    tracking_epoch bigint NOT NULL,
    key_attribute int2 NOT NULL CHECK (key_attribute>0),
    value_attribute int2 NOT NULL CHECK (value_attribute>0),
    row_trigger oid NOT NULL,
    truncate_trigger oid NOT NULL,
    source_identity text NOT NULL CHECK (octet_length(source_identity)<=256),
    UNIQUE(source_relation,epoch),
    FOREIGN KEY(source_relation,epoch) REFERENCES syntra.capture_heads(source_relation,epoch) ON DELETE CASCADE
);
INSERT INTO syntra.capture_bindings
    (source_relation,epoch,tracking_epoch,key_attribute,value_attribute,row_trigger,truncate_trigger,source_identity)
SELECT source_relation,epoch,tracking_epoch,key_attribute,value_attribute,row_trigger,truncate_trigger,source_identity
FROM syntra.capture_heads;

-- With no marks, a feed's position is its head row, which 0.1.0 kept current.
-- Create it before any DROP below: the 0.2.0 sql_drop guard already deletes
-- from this table while the upgrade runs.
CREATE TABLE syntra.capture_marks (
    source_relation oid NOT NULL,
    epoch bigint NOT NULL,
    ordinal bigint NOT NULL CHECK (ordinal>0),
    live_rows bigint NOT NULL CHECK (live_rows BETWEEN 0 AND 1000000),
    PRIMARY KEY(epoch,ordinal),
    FOREIGN KEY(source_relation,epoch) REFERENCES syntra.capture_bindings(source_relation,epoch) ON DELETE CASCADE
);
CREATE INDEX capture_marks_source_index ON syntra.capture_marks(source_relation);

ALTER TABLE syntra.capture_base_rows
    DROP CONSTRAINT capture_base_rows_source_relation_epoch_fkey,
    ADD CONSTRAINT capture_base_rows_source_relation_epoch_fkey FOREIGN KEY(source_relation,epoch)
        REFERENCES syntra.capture_bindings(source_relation,epoch) ON DELETE CASCADE;
ALTER TABLE syntra.capture_events
    DROP CONSTRAINT capture_events_source_relation_epoch_fkey,
    ADD CONSTRAINT capture_events_source_relation_epoch_fkey FOREIGN KEY(source_relation,epoch)
        REFERENCES syntra.capture_bindings(source_relation,epoch) ON DELETE CASCADE;
ALTER TABLE syntra.capture_checkpoints
    DROP CONSTRAINT capture_checkpoints_source_relation_epoch_fkey,
    ADD CONSTRAINT capture_checkpoints_source_relation_epoch_fkey FOREIGN KEY(source_relation,epoch)
        REFERENCES syntra.capture_bindings(source_relation,epoch) ON DELETE CASCADE;
ALTER TABLE syntra.capture_chunks
    DROP CONSTRAINT capture_chunks_source_relation_epoch_fkey,
    ADD CONSTRAINT capture_chunks_source_relation_epoch_fkey FOREIGN KEY(source_relation,epoch)
        REFERENCES syntra.capture_bindings(source_relation,epoch) ON DELETE CASCADE;

REVOKE ALL ON syntra.capture_bindings,syntra.capture_marks FROM PUBLIC;

CREATE FUNCTION syntra_capture_lag(
    "source" regclass
) RETURNS TABLE (
    "emitted" bigint,
    "applied" bigint,
    "pending_events" bigint,
    "generation" bigint
)
STRICT VOLATILE PARALLEL UNSAFE
LANGUAGE c
AS 'MODULE_PATHNAME', 'syntra_capture_lag_wrapper';

CREATE FUNCTION syntra_captured_sources() RETURNS SETOF oid
STRICT VOLATILE PARALLEL UNSAFE
LANGUAGE c
AS 'MODULE_PATHNAME', 'syntra_captured_sources_wrapper';

-- One row per captured source the caller may inspect. A source whose lag or
-- headroom read fails still gets a row, with the reason in "error", so one broken
-- feed does not hide the others. Lag and headroom are read one after the other.
CREATE FUNCTION public.syntra_capture_status()
RETURNS TABLE (
    "source" regclass,
    "emitted" bigint,
    "applied" bigint,
    "pending_events" bigint,
    "generation" bigint,
    "retained_events" bigint,
    "max_events" integer,
    "free_events" bigint,
    "error" text
)
LANGUAGE plpgsql VOLATILE SECURITY INVOKER
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    captured oid;
BEGIN
    FOR captured IN SELECT s FROM public.syntra_captured_sources() AS s ORDER BY s LOOP
        "source" := captured::regclass;
        BEGIN
            SELECT l.emitted, l.applied, l.pending_events, l.generation
                INTO "emitted", "applied", "pending_events", "generation"
                FROM public.syntra_capture_lag(captured::regclass) AS l;
            SELECT h.retained_events, h.max_events, h.free_events
                INTO "retained_events", "max_events", "free_events"
                FROM public.syntra_capture_headroom(captured::regclass) AS h;
            "error" := NULL;
        EXCEPTION WHEN OTHERS THEN
            "emitted" := NULL; "applied" := NULL; "pending_events" := NULL; "generation" := NULL;
            "retained_events" := NULL; "max_events" := NULL; "free_events" := NULL;
            "error" := SQLERRM;
        END;
        RETURN NEXT;
    END LOOP;
END
$$;
