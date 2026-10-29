-- SyntraDB 0.6.0 to 0.7.0. Run through ALTER EXTENSION syntradb UPDATE, never directly.
\echo Use "ALTER EXTENSION syntradb UPDATE TO '0.7.0'" to load this file. \quit

-- A partition's delta, one row per revision. Apply appends a row and deletes
-- the one it replaces instead of updating the chunk row, so a long-held
-- snapshot does not leave versions of the chunk row for every lookup to walk.
-- The chunk row then holds the base at its base revision.
CREATE TABLE syntra.capture_chunk_deltas (
    source_relation oid NOT NULL,
    epoch bigint NOT NULL,
    partition smallint NOT NULL CHECK (partition BETWEEN 0 AND 63),
    revision bigint NOT NULL CHECK (revision>0),
    row_count bigint NOT NULL CHECK (row_count BETWEEN 0 AND 1000000),
    delta bytea NOT NULL CHECK (octet_length(delta) BETWEEN 85 AND 65536),
    PRIMARY KEY(source_relation,epoch,partition,revision),
    FOREIGN KEY(source_relation,epoch) REFERENCES syntra.capture_bindings(source_relation,epoch) ON DELETE CASCADE
);
REVOKE ALL ON syntra.capture_chunk_deltas FROM PUBLIC;
-- Existing chunk rows keep their inline deltas. The 0.7.0 library reads them
-- as 0.6.0 wrote them and moves each delta to its own row the next time an
-- apply changes that partition.
