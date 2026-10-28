-- SyntraDB 0.3.0 to 0.4.0. Run through ALTER EXTENSION syntradb UPDATE, never directly.
\echo Use "ALTER EXTENSION syntradb UPDATE TO '0.4.0'" to load this file. \quit

-- Partition deltas. NULL means a partition has none, which is true of every
-- existing partition, so no checkpoint is rewritten. ADD COLUMN without a default
-- changes only the catalog. Capture writers never touch these two tables, so they
-- neither wait for this update nor fail; maintenance waits for its locks. A
-- database downgraded from 0.4.0 already has the column, NULL in every row.
ALTER TABLE syntra.capture_chunks
    ADD COLUMN IF NOT EXISTS delta bytea CHECK (octet_length(delta) BETWEEN 85 AND 65536);

-- Manifest version 2 (8,272 bytes) is written while some partition has a delta.
ALTER TABLE syntra.capture_checkpoints
    DROP CONSTRAINT capture_checkpoints_manifest_check,
    ADD CONSTRAINT capture_checkpoints_manifest_check CHECK (octet_length(manifest) IN (5200,8272));

-- Folds every partition delta, so a downgrade to 0.3.0 can run.
CREATE FUNCTION syntra_fold_capture(
    "source" regclass
) RETURNS bigint
STRICT VOLATILE PARALLEL UNSAFE
LANGUAGE c
AS 'MODULE_PATHNAME', 'syntra_fold_capture_wrapper';
REVOKE ALL ON FUNCTION public.syntra_fold_capture(regclass) FROM PUBLIC;

-- numeric(p,s) value columns with p up to 18, stored as scaled int8 values.
CREATE FUNCTION syntra_checkpoint_numeric_summary(
    "source" regclass,
    "column" TEXT,
    "replay_tail" bool DEFAULT false
) RETURNS TABLE (
    "rows" bigint,
    "non_nulls" bigint,
    "sum" NUMERIC,
    "min" NUMERIC,
    "max" NUMERIC,
    "route" TEXT,
    "reason" TEXT
)
STRICT STABLE PARALLEL UNSAFE
LANGUAGE c
AS 'MODULE_PATHNAME', 'syntra_checkpoint_numeric_summary_wrapper';
