-- SyntraDB 0.5.0 to 0.4.0. Run through ALTER EXTENSION syntradb UPDATE, never directly,
-- while the 0.5.0 library is still installed, then install 0.4.0 and restart.
\echo Use "ALTER EXTENSION syntradb UPDATE TO '0.4.0'" to load this file. \quit

-- 0.5.0 writers to a captured source leave its counter rows unchanged, and 0.4.0
-- reuses a projection whose counters match. Every projection of a captured
-- source must go, including one published before 0.5.0: its counters stopped
-- moving at the update. The check keeps the 0.5.0 library, still loaded until the
-- restart, from publishing another. Projections of sources that are only tracked
-- stay, because their counters kept moving. The lock holds publishers off.
LOCK TABLE syntra.trusted_int4_projections IN ACCESS EXCLUSIVE MODE;
DELETE FROM syntra.trusted_int4_projections AS p
WHERE p.capture_position IS NOT NULL
    OR EXISTS (SELECT 1 FROM syntra.capture_heads AS h WHERE h.source_relation = p.source_relation);
-- The column stays: 0.5.0 reads it by attribute number, so dropping it would leave
-- a later update to 0.5.0 unable to reuse any projection.
ALTER TABLE syntra.trusted_int4_projections
    ADD CONSTRAINT trusted_int4_projections_downgraded_check CHECK (capture_position IS NULL);
