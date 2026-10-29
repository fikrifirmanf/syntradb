-- SyntraDB 0.4.0 to 0.5.0. Run through ALTER EXTENSION syntradb UPDATE, never directly.
\echo Use "ALTER EXTENSION syntradb UPDATE TO '0.5.0'" to load this file. \quit

-- 0.5.0 writers to a captured source stop updating its counter rows, and trusted
-- reuse compares the feed position instead. NULL means a projection published
-- without one: right for a source that is only tracked, and never equal to a
-- captured source's position, so those projections are rebuilt before reuse.
-- ADD COLUMN without a default changes only the catalog. A database downgraded
-- from 0.5.0 already has the column and its check, plus the downgrade's check.
ALTER TABLE syntra.trusted_int4_projections
    ADD COLUMN IF NOT EXISTS capture_position bigint CHECK (capture_position >= 0);
ALTER TABLE syntra.trusted_int4_projections
    DROP CONSTRAINT IF EXISTS trusted_int4_projections_downgraded_check;
