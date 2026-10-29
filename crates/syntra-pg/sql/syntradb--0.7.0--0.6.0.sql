-- SyntraDB 0.7.0 to 0.6.0. Run through ALTER EXTENSION syntradb UPDATE, never directly.
\echo Use "ALTER EXTENSION syntradb UPDATE TO '0.6.0'" to load this file. \quit

-- 0.6.0 reads a partition's delta inline from its chunk row, at the revision
-- the manifest records. Each partition's newest appended delta moves back into
-- its chunk row; a delta row at or below the chunk row's revision was replaced
-- by a fold and is dropped with the table. The maintenance lock, taken by every
-- apply and prune, keeps the manifests and rows still while they move. The
-- 0.7.0 drop guard would read the dropped table, so event triggers are off for
-- this script only.
SET LOCAL event_triggers = off;
LOCK TABLE syntra.capture_checkpoints IN SHARE ROW EXCLUSIVE MODE;
UPDATE syntra.capture_chunks AS c
SET revision = d.revision, row_count = d.row_count, delta = d.delta
FROM (
    SELECT DISTINCT ON (source_relation, epoch, partition)
        source_relation, epoch, partition, revision, row_count, delta
    FROM syntra.capture_chunk_deltas
    ORDER BY source_relation, epoch, partition, revision DESC
) AS d
WHERE c.source_relation = d.source_relation AND c.epoch = d.epoch
    AND c.partition = d.partition AND d.revision > c.revision;
DROP TABLE syntra.capture_chunk_deltas;
