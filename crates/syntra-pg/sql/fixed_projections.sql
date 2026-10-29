-- Caller-owned fixed artifacts. These rows never prove live-scan eligibility.
CREATE SCHEMA syntra;
REVOKE ALL ON SCHEMA syntra FROM PUBLIC;
GRANT USAGE ON SCHEMA syntra TO PUBLIC;

CREATE TABLE syntra.fixed_projections (
    owner_oid oid NOT NULL DEFAULT (CURRENT_USER::regrole::oid),
    name text NOT NULL CHECK (octet_length(name) BETWEEN 1 AND 128 AND btrim(name) <> ''),
    generation bigint NOT NULL CHECK (generation > 0),
    segment bytea NOT NULL CHECK (octet_length(segment) BETWEEN 40 AND 8388608),
    source_database oid NOT NULL,
    source_relation oid NOT NULL,
    source_schema text NOT NULL,
    source_table text NOT NULL,
    source_column text NOT NULL,
    source_attribute int2 NOT NULL CHECK (source_attribute > 0),
    source_type oid NOT NULL,
    source_snapshot text NOT NULL,
    source_transaction text,
    source_command bigint NOT NULL,
    captured_at timestamptz NOT NULL,
    source_rows bigint NOT NULL CHECK (source_rows BETWEEN 0 AND 1000000),
    PRIMARY KEY (owner_oid, name)
);
ALTER TABLE syntra.fixed_projections ENABLE ROW LEVEL SECURITY;
ALTER TABLE syntra.fixed_projections FORCE ROW LEVEL SECURITY;
CREATE POLICY fixed_projection_owner ON syntra.fixed_projections
    USING (owner_oid = CURRENT_USER::regrole::oid)
    WITH CHECK (owner_oid = CURRENT_USER::regrole::oid);
GRANT SELECT, INSERT, UPDATE, DELETE ON syntra.fixed_projections TO PUBLIC;
SELECT pg_catalog.pg_extension_config_dump('syntra.fixed_projections', '');

CREATE FUNCTION public.syntra_publish_fixed_projection(text, regclass, text)
RETURNS bigint LANGUAGE SQL VOLATILE STRICT SECURITY INVOKER
SET search_path = pg_catalog, pg_temp
AS $$
    INSERT INTO syntra.fixed_projections AS existing (
        owner_oid, name, generation, segment, source_database, source_relation,
        source_schema, source_table, source_column, source_attribute, source_type,
        source_snapshot, source_transaction, source_command, captured_at, source_rows
    )
    SELECT CURRENT_USER::regrole::oid, $1, 1, c.segment, c.source_database,
        c.source_relation, c.source_schema, c.source_table, c.source_column,
        c.source_attribute, c.source_type, c.source_snapshot, c.source_transaction,
        c.source_command, c.captured_at, c.source_rows
    FROM public.syntra_capture_int4($2, $3) AS c
    ON CONFLICT (owner_oid, name) DO UPDATE SET
        generation = existing.generation + 1,
        segment = EXCLUDED.segment,
        source_database = EXCLUDED.source_database,
        source_relation = EXCLUDED.source_relation,
        source_schema = EXCLUDED.source_schema,
        source_table = EXCLUDED.source_table,
        source_column = EXCLUDED.source_column,
        source_attribute = EXCLUDED.source_attribute,
        source_type = EXCLUDED.source_type,
        source_snapshot = EXCLUDED.source_snapshot,
        source_transaction = EXCLUDED.source_transaction,
        source_command = EXCLUDED.source_command,
        captured_at = EXCLUDED.captured_at,
        source_rows = EXCLUDED.source_rows
    RETURNING generation
$$;

CREATE FUNCTION public.syntra_fixed_projection_summary(text)
RETURNS TABLE(generation bigint, rows bigint, non_nulls bigint, sum bigint, min int4, max int4)
LANGUAGE SQL STABLE STRICT SECURITY INVOKER
SET search_path = pg_catalog, pg_temp
AS $$
    SELECT p.generation, s.rows, s.non_nulls, s.sum, s.min, s.max
    FROM syntra.fixed_projections AS p,
        LATERAL public.syntra_segment_summary(p.segment) AS s
    WHERE p.owner_oid = CURRENT_USER::regrole::oid AND p.name = $1
    LIMIT 1
$$;

CREATE FUNCTION public.syntra_delete_fixed_projection(text)
RETURNS boolean LANGUAGE SQL VOLATILE STRICT SECURITY INVOKER
SET search_path = pg_catalog, pg_temp
AS $$
    WITH deleted AS (
        DELETE FROM syntra.fixed_projections
        WHERE owner_oid = CURRENT_USER::regrole::oid AND name = $1
        RETURNING generation
    ) SELECT EXISTS (SELECT generation FROM deleted)
$$;

-- Private, logged state for source tracking and live binding validation.
CREATE TABLE syntra.tracked_sources (
    source_relation oid PRIMARY KEY,
    trigger_oid oid NOT NULL UNIQUE,
    epoch bigint GENERATED ALWAYS AS IDENTITY UNIQUE
);
REVOKE ALL ON syntra.tracked_sources FROM PUBLIC;
CREATE TABLE syntra.source_generation_shards (
    source_relation oid NOT NULL REFERENCES syntra.tracked_sources(source_relation) ON DELETE CASCADE,
    shard int2 NOT NULL CHECK (shard BETWEEN 0 AND 15),
    -- Sixteen bounded counters have a sum that always fits int8.
    generation bigint NOT NULL DEFAULT 0 CHECK (generation BETWEEN 0 AND 576460752303423487),
    PRIMARY KEY (source_relation, shard)
);
REVOKE ALL ON syntra.source_generation_shards FROM PUBLIC;
-- Engine-owned bytes and snapshot version bindings. Never restore historical
-- eligibility through logical configuration dumps; an admin must republish.
CREATE TABLE syntra.trusted_int4_projections (
    source_relation oid NOT NULL REFERENCES syntra.tracked_sources(source_relation) ON DELETE CASCADE,
    source_attribute int2 NOT NULL CHECK (source_attribute > 0),
    source_database oid NOT NULL,
    source_identity text NOT NULL CHECK (octet_length(source_identity) <= 256),
    epoch bigint NOT NULL CHECK (epoch > 0),
    generation_vector bigint[] NOT NULL CHECK (array_ndims(generation_vector)=1
        AND cardinality(generation_vector)=16 AND array_position(generation_vector,NULL) IS NULL),
    segment bytea NOT NULL CHECK (octet_length(segment) BETWEEN 40 AND 8388608),
    PRIMARY KEY (source_relation,source_attribute),
    -- 0.5.0: the feed position of a captured source at publication; NULL for a
    -- source that is only tracked.
    capture_position bigint CHECK (capture_position >= 0)
);
REVOKE ALL ON syntra.trusted_int4_projections FROM PUBLIC;
-- Opt-in transactional capture. These tables are deliberately not config-dumped.
CREATE TABLE syntra.capture_heads (
    source_relation oid PRIMARY KEY REFERENCES syntra.tracked_sources(source_relation) ON DELETE CASCADE,
    epoch bigint GENERATED ALWAYS AS IDENTITY UNIQUE,
    tracking_epoch bigint NOT NULL,
    key_attribute int2 NOT NULL CHECK (key_attribute>0),
    value_attribute int2 NOT NULL CHECK (value_attribute>0),
    row_trigger oid NOT NULL,
    truncate_trigger oid NOT NULL,
    max_events int4 NOT NULL CHECK (max_events BETWEEN 1 AND 4096),
    emitted bigint NOT NULL DEFAULT 0 CHECK (emitted>=0),
    source_identity text NOT NULL CHECK (octet_length(source_identity)<=256),
    base_snapshot text NOT NULL,
    base_rows bigint NOT NULL CHECK (base_rows BETWEEN 0 AND 100000),
    base_transaction xid8 NOT NULL DEFAULT pg_catalog.pg_current_xact_id(),
    base_command bigint NOT NULL DEFAULT 0 CHECK (base_command BETWEEN 0 AND 4294967295),
    pruned bigint NOT NULL DEFAULT 0 CHECK (pruned>=0 AND pruned<=emitted),
    live_rows bigint NOT NULL DEFAULT 0 CHECK (live_rows BETWEEN 0 AND 1000000),
    CHECK (emitted-pruned<=max_events),
    UNIQUE(source_relation,epoch)
);
-- Immutable copy of a feed's binding, written once at seed. Writers read their
-- configuration here and child rows reference it, so the per-write path looks up
-- the head row only once, to lock it. Under a long-held snapshot every lookup of
-- that row walks all of its unremovable versions.
CREATE TABLE syntra.capture_bindings (
    source_relation oid PRIMARY KEY,
    epoch bigint NOT NULL UNIQUE,
    tracking_epoch bigint NOT NULL,
    key_attribute int2 NOT NULL CHECK (key_attribute>0),
    value_attribute int2 NOT NULL CHECK (value_attribute>0),
    row_trigger oid NOT NULL,
    truncate_trigger oid NOT NULL,
    source_identity text NOT NULL CHECK (octet_length(source_identity)<=256),
    -- Byte widths of the key and value columns: 4 for int4, 8 for int8.
    key_width int2 NOT NULL CHECK (key_width IN (4,8)),
    value_width int2 NOT NULL CHECK (value_width IN (4,8)),
    UNIQUE(source_relation,epoch),
    FOREIGN KEY(source_relation,epoch) REFERENCES syntra.capture_heads(source_relation,epoch) ON DELETE CASCADE
);
CREATE TABLE syntra.capture_base_rows (
    source_relation oid NOT NULL,
    epoch bigint NOT NULL,
    key bigint NOT NULL,
    value bigint,
    PRIMARY KEY(epoch,key),
    FOREIGN KEY(source_relation,epoch) REFERENCES syntra.capture_bindings(source_relation,epoch) ON DELETE CASCADE
);
CREATE INDEX capture_base_source_index ON syntra.capture_base_rows(source_relation);
CREATE TABLE syntra.capture_events (
    source_relation oid NOT NULL,
    epoch bigint NOT NULL,
    ordinal bigint NOT NULL CHECK (ordinal>0),
    origin_xid xid8 NOT NULL,
    command_id bigint NOT NULL,
    key bigint NOT NULL,
    before_present boolean NOT NULL,
    before_value bigint,
    after_present boolean NOT NULL,
    after_value bigint,
    PRIMARY KEY(epoch,ordinal),
    CHECK (before_present OR before_value IS NULL),
    CHECK (after_present OR after_value IS NULL),
    CHECK (before_present OR after_present),
    FOREIGN KEY(source_relation,epoch) REFERENCES syntra.capture_bindings(source_relation,epoch) ON DELETE CASCADE
);
CREATE INDEX capture_events_source_index ON syntra.capture_events(source_relation);
-- One row per trigger call: the last ordinal it reserved and the live row count
-- after it. Writers lock the head row instead of updating it, so a long-held
-- snapshot no longer leaves one unremovable head version per captured write.
-- The newest mark is the feed's position; pruning folds old marks into the head.
CREATE TABLE syntra.capture_marks (
    source_relation oid NOT NULL,
    epoch bigint NOT NULL,
    ordinal bigint NOT NULL CHECK (ordinal>0),
    live_rows bigint NOT NULL CHECK (live_rows BETWEEN 0 AND 1000000),
    PRIMARY KEY(epoch,ordinal),
    FOREIGN KEY(source_relation,epoch) REFERENCES syntra.capture_bindings(source_relation,epoch) ON DELETE CASCADE
);
CREATE INDEX capture_marks_source_index ON syntra.capture_marks(source_relation);
CREATE TABLE syntra.capture_checkpoints (
    source_relation oid PRIMARY KEY,
    epoch bigint NOT NULL,
    applied bigint NOT NULL CHECK (applied>=0),
    generation bigint NOT NULL CHECK (generation>0),
    row_count bigint NOT NULL CHECK (row_count BETWEEN 0 AND 1000000),
    manifest bytea NOT NULL CHECK (octet_length(manifest) IN (5200,8272)),
    FOREIGN KEY(source_relation,epoch) REFERENCES syntra.capture_bindings(source_relation,epoch) ON DELETE CASCADE
);
CREATE TABLE syntra.capture_chunks (
    source_relation oid NOT NULL,
    epoch bigint NOT NULL,
    partition smallint NOT NULL CHECK (partition BETWEEN 0 AND 63),
    revision bigint NOT NULL CHECK (revision>=0),
    row_count bigint NOT NULL CHECK (row_count BETWEEN 0 AND 1000000),
    state bytea NOT NULL CHECK (octet_length(state) BETWEEN 112 AND 8388608),
    segment bytea NOT NULL CHECK (octet_length(segment) BETWEEN 40 AND 8388608),
    delta bytea CHECK (octet_length(delta) BETWEEN 85 AND 65536),
    PRIMARY KEY(source_relation,partition),
    FOREIGN KEY(source_relation,epoch) REFERENCES syntra.capture_bindings(source_relation,epoch) ON DELETE CASCADE
);
CREATE INDEX capture_chunks_version_index ON syntra.capture_chunks(source_relation,epoch,partition,revision);
REVOKE ALL ON syntra.capture_chunks FROM PUBLIC;
REVOKE ALL ON syntra.capture_checkpoints FROM PUBLIC;
REVOKE ALL ON FUNCTION public.syntra_apply_capture(regclass) FROM PUBLIC;
REVOKE ALL ON FUNCTION public.syntra_prune_capture(regclass) FROM PUBLIC;
REVOKE ALL ON FUNCTION public.syntra_apply_prune_capture(regclass) FROM PUBLIC;
REVOKE ALL ON FUNCTION public.syntra_fold_capture(regclass) FROM PUBLIC;
REVOKE ALL ON syntra.capture_heads,syntra.capture_bindings,syntra.capture_base_rows,syntra.capture_events,syntra.capture_marks FROM PUBLIC;
REVOKE ALL ON FUNCTION public.syntra_seed_capture(regclass,text,text,int4) FROM PUBLIC;
REVOKE ALL ON FUNCTION public.syntra_stop_capture(regclass) FROM PUBLIC;
REVOKE ALL ON FUNCTION public.syntra_capture_row() FROM PUBLIC;
REVOKE ALL ON FUNCTION public.syntra_capture_no_truncate() FROM PUBLIC;
ALTER FUNCTION public.syntra_capture_row() SECURITY DEFINER;
ALTER FUNCTION public.syntra_capture_row() SET search_path=pg_catalog,pg_temp;
ALTER FUNCTION public.syntra_capture_no_truncate() SET search_path=pg_catalog,pg_temp;
REVOKE ALL ON FUNCTION public.syntra_publish_trusted_int4(regclass,text) FROM PUBLIC;
REVOKE ALL ON FUNCTION public.syntra_track_source(regclass, text) FROM PUBLIC;
REVOKE ALL ON FUNCTION public.syntra_source_changed() FROM PUBLIC;
ALTER FUNCTION public.syntra_source_changed() SECURITY DEFINER;
ALTER FUNCTION public.syntra_source_changed() SET search_path = pg_catalog, pg_temp;

CREATE FUNCTION public.syntra_source_tracking_status(regclass)
RETURNS TABLE(epoch bigint, generation bigint, guard_config_valid boolean, generation_vector bigint[])
LANGUAGE SQL STABLE STRICT SECURITY INVOKER
SET search_path = pg_catalog, pg_temp
AS $$
    SELECT s.epoch, COALESCE(g.generation, 0), COALESCE(g.complete AND
        t.tgrelid = s.source_relation AND t.tgenabled = 'A' AND t.tgtype = 60
        AND t.tgfoid = 'public.syntra_source_changed()'::regprocedure::oid
        AND NOT t.tgisinternal AND t.tgnargs = 0 AND t.tgqual IS NULL
        AND t.tgconstraint = 0 AND t.tgparentid = 0 AND t.tgattr = ''::int2vector
        AND c.relkind = 'r' AND c.relpersistence = 'p' AND NOT c.relispartition
        AND NOT c.relrowsecurity AND NOT c.relforcerowsecurity
        AND c.relam = (SELECT oid FROM pg_catalog.pg_am WHERE amname = 'heap' LIMIT 1)
        AND pg_catalog.current_setting('event_triggers')::boolean
        AND EXISTS (SELECT oid FROM pg_catalog.pg_event_trigger
            WHERE evtname = 'syntra_ddl_epoch_guard' AND evtevent = 'ddl_command_end'
            AND evtenabled = 'A' AND evtfoid = pg_catalog.to_regprocedure('public.syntra_ddl_epoch_guard()')::oid)
        AND EXISTS (SELECT oid FROM pg_catalog.pg_event_trigger
            WHERE evtname = 'syntra_ddl_drop_guard' AND evtevent = 'sql_drop'
            AND evtenabled = 'A' AND evtfoid = pg_catalog.to_regprocedure('public.syntra_ddl_drop_guard()')::oid)
        AND NOT EXISTS (SELECT inhrelid FROM pg_catalog.pg_inherits
            WHERE inhrelid = s.source_relation OR inhparent = s.source_relation), false), g.generation_vector
    FROM syntra.tracked_sources AS s
    LEFT JOIN pg_catalog.pg_trigger AS t ON t.oid = s.trigger_oid
    LEFT JOIN pg_catalog.pg_class AS c ON c.oid = s.source_relation
    CROSS JOIN LATERAL (
        SELECT sum(generation)::bigint AS generation,
            array_agg(generation ORDER BY shard) AS generation_vector,
            count(*) = 16 AS complete
        FROM syntra.source_generation_shards WHERE source_relation = s.source_relation
    ) AS g
    WHERE s.source_relation = $1::oid LIMIT 1
$$;
REVOKE ALL ON FUNCTION public.syntra_source_tracking_status(regclass) FROM PUBLIC;

CREATE FUNCTION public.syntra_ddl_epoch_guard() RETURNS event_trigger
AS 'MODULE_PATHNAME', 'syntra_ddl_epoch_guard_wrapper'
LANGUAGE C VOLATILE SECURITY DEFINER
SET search_path = pg_catalog, pg_temp;
REVOKE ALL ON FUNCTION public.syntra_ddl_epoch_guard() FROM PUBLIC;
CREATE EVENT TRIGGER syntra_ddl_epoch_guard ON ddl_command_end
EXECUTE FUNCTION public.syntra_ddl_epoch_guard();
ALTER EVENT TRIGGER syntra_ddl_epoch_guard ENABLE ALWAYS;
CREATE FUNCTION public.syntra_ddl_drop_guard() RETURNS event_trigger
AS 'MODULE_PATHNAME', 'syntra_ddl_drop_guard_wrapper'
LANGUAGE C VOLATILE SECURITY DEFINER
SET search_path = pg_catalog, pg_temp;
REVOKE ALL ON FUNCTION public.syntra_ddl_drop_guard() FROM PUBLIC;
CREATE EVENT TRIGGER syntra_ddl_drop_guard ON sql_drop
EXECUTE FUNCTION public.syntra_ddl_drop_guard();
ALTER EVENT TRIGGER syntra_ddl_drop_guard ENABLE ALWAYS;

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
