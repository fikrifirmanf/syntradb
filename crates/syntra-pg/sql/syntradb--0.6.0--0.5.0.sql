-- SyntraDB 0.6.0 to 0.5.0. Run through ALTER EXTENSION syntradb UPDATE, never directly.
\echo Use "ALTER EXTENSION syntradb UPDATE TO '0.5.0'" to load this file. \quit

-- 0.5.0 accepts only an immediate capture row trigger. Each deferred one is
-- replaced by the immediate trigger 0.5.0 installs, and the new OID recorded.
-- The formats are the same, so no history is lost. Writers hold the source
-- locks the swap needs, so it waits for them and no queued firing is dropped.
-- A missing or disabled trigger stays invalid, as in the 0.6.0 update.
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
            AND t.tgdeferrable AND t.tginitdeferred AND t.tgconstraint <> 0
        ORDER BY h.source_relation
    LOOP
        EXECUTE pg_catalog.format('DROP TRIGGER _syntra_capture_row ON %s', feed.source_relation::regclass);
        EXECUTE pg_catalog.format('CREATE TRIGGER _syntra_capture_row AFTER INSERT OR UPDATE OR DELETE ON %s FOR EACH ROW EXECUTE FUNCTION public.syntra_capture_row()', feed.source_relation::regclass);
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
