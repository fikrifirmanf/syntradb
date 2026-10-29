-- SyntraDB 0.5.0 to 0.6.0. Run through ALTER EXTENSION syntradb UPDATE, never directly.
\echo Use "ALTER EXTENSION syntradb UPDATE TO '0.6.0'" to load this file. \quit

-- 0.6.0 fires the capture row trigger at commit, so the head-row lock it takes
-- is held through commit only, not from a transaction's first captured change.
-- The event and checkpoint formats are unchanged. Each captured source's row
-- trigger is replaced by a deferred constraint trigger and the new OID recorded.
-- Only a trigger that is still the feed's valid one is replaced: a missing or
-- disabled trigger may already have missed writes, and stays invalid.
-- The DDL guards would treat the swap as a source change and retire the feed;
-- it changes no data, so they are off for this script only.
SET LOCAL event_triggers = off;
DO $$
DECLARE
    feed record;
    replacement oid;
BEGIN
    FOR feed IN
        SELECT h.source_relation, h.row_trigger
        FROM syntra.capture_heads AS h
        JOIN pg_catalog.pg_trigger AS t ON t.oid = h.row_trigger AND t.tgrelid = h.source_relation
        WHERE t.tgname = '_syntra_capture_row' AND t.tgenabled = 'A' AND t.tgtype = 29
            AND t.tgfoid = 'public.syntra_capture_row()'::regprocedure
            AND NOT t.tgdeferrable AND NOT t.tginitdeferred AND t.tgconstraint = 0
        ORDER BY h.source_relation
    LOOP
        EXECUTE pg_catalog.format('DROP TRIGGER _syntra_capture_row ON %s', feed.source_relation::regclass);
        EXECUTE pg_catalog.format('CREATE CONSTRAINT TRIGGER _syntra_capture_row AFTER INSERT OR UPDATE OR DELETE ON %s DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION public.syntra_capture_row()', feed.source_relation::regclass);
        EXECUTE pg_catalog.format('ALTER TABLE %s ENABLE ALWAYS TRIGGER _syntra_capture_row', feed.source_relation::regclass);
        SELECT t.oid INTO STRICT replacement FROM pg_catalog.pg_trigger AS t
            WHERE t.tgrelid = feed.source_relation AND t.tgname = '_syntra_capture_row';
        UPDATE syntra.capture_heads SET row_trigger = replacement
            WHERE source_relation = feed.source_relation AND row_trigger = feed.row_trigger;
        UPDATE syntra.capture_bindings SET row_trigger = replacement
            WHERE source_relation = feed.source_relation AND row_trigger = feed.row_trigger;
    END LOOP;
END
$$;
