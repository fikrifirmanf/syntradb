-- SyntraDB 0.4.0 to 0.3.0. Run through ALTER EXTENSION syntradb UPDATE, never directly,
-- while the 0.4.0 library is still installed, then install 0.3.0 and restart.
\echo Use "ALTER EXTENSION syntradb UPDATE TO '0.3.0'" to load this file. \quit

-- 0.3.0 cannot read partition deltas or version 2 manifests. syntra_fold_capture
-- on every captured source writes the checkpoint as 0.3.0 stores it. The locks keep
-- maintenance from writing a delta between this check and the commit; capture
-- writers never touch these tables, so they keep committing.
LOCK TABLE syntra.capture_checkpoints, syntra.capture_chunks IN ACCESS EXCLUSIVE MODE;
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM syntra.capture_chunks WHERE delta IS NOT NULL)
        OR EXISTS (SELECT 1 FROM syntra.capture_checkpoints WHERE octet_length(manifest)<>5200) THEN
        RAISE EXCEPTION 'partition deltas remain'
            USING HINT = 'Run SELECT syntra_fold_capture(source_relation::regclass) FROM syntra.capture_checkpoints, then update again.';
    END IF;
    -- 0.3.0 cannot capture numeric columns.
    IF EXISTS (SELECT 1 FROM syntra.capture_bindings AS b
        JOIN pg_catalog.pg_attribute AS a ON a.attrelid=b.source_relation AND a.attnum=b.value_attribute
        WHERE a.atttypid='pg_catalog.numeric'::pg_catalog.regtype) THEN
        RAISE EXCEPTION 'numeric capture feeds remain'
            USING HINT = 'Stop them with syntra_stop_capture, then update again.';
    END IF;
END
$$;

-- The delta column stays, NULL in every row: 0.3.0 neither reads nor writes it,
-- and 0.4.0 reads chunk rows by attribute number, so dropping it would leave a
-- later update to 0.4.0 unable to read any checkpoint.
ALTER TABLE syntra.capture_checkpoints
    DROP CONSTRAINT capture_checkpoints_manifest_check,
    ADD CONSTRAINT capture_checkpoints_manifest_check CHECK (octet_length(manifest)=5200);
DROP FUNCTION syntra_fold_capture(regclass);
DROP FUNCTION syntra_checkpoint_numeric_summary(regclass,text,boolean);
