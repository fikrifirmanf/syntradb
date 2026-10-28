-- SyntraDB 0.2.0 to 0.3.0. Run through ALTER EXTENSION syntradb UPDATE, never directly.
\echo Use "ALTER EXTENSION syntradb UPDATE TO '0.3.0'" to load this file. \quit

-- Block seeds, stops and prunes until this transaction commits. Capture writes
-- wait too: ADD COLUMN below holds ACCESS EXCLUSIVE on capture_bindings, and a
-- writer reads the bindings after commit, with the width columns in place.
LOCK TABLE syntra.capture_heads IN SHARE ROW EXCLUSIVE MODE;

-- Every existing feed is int4 key and int4 value. Its checkpoint bytes are the
-- v1 formats, which 0.3.0 still writes for that layout, so nothing is reseeded.
ALTER TABLE syntra.capture_bindings
    ADD COLUMN key_width int2 NOT NULL DEFAULT 4 CHECK (key_width IN (4,8)),
    ADD COLUMN value_width int2 NOT NULL DEFAULT 4 CHECK (value_width IN (4,8));
ALTER TABLE syntra.capture_bindings
    ALTER COLUMN key_width DROP DEFAULT,
    ALTER COLUMN value_width DROP DEFAULT;

-- Stored keys and values become bigint. Every int4 value fits. These rewrite the
-- two tables, so the update takes longer as unpruned history grows.
ALTER TABLE syntra.capture_base_rows
    ALTER COLUMN key TYPE bigint,
    ALTER COLUMN value TYPE bigint;
ALTER TABLE syntra.capture_events
    ALTER COLUMN key TYPE bigint,
    ALTER COLUMN before_value TYPE bigint,
    ALTER COLUMN after_value TYPE bigint;

CREATE FUNCTION syntra_checkpoint_int8_summary(
    "source" regclass,
    "column" TEXT,
    "replay_tail" bool DEFAULT false
) RETURNS TABLE (
    "rows" bigint,
    "non_nulls" bigint,
    "sum" NUMERIC,
    "min" bigint,
    "max" bigint,
    "route" TEXT,
    "reason" TEXT
)
STRICT STABLE PARALLEL UNSAFE
LANGUAGE c
AS 'MODULE_PATHNAME', 'syntra_checkpoint_int8_summary_wrapper';
