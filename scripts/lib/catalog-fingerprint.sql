-- Canonical extension catalog contract, including function defaults and settings.
SELECT f FROM (
    SELECT 'rel '||c.relname||' '||c.relkind::text||' '||coalesce(c.relacl::text,'-') AS f
        FROM pg_class AS c WHERE c.relnamespace='syntra'::regnamespace
    UNION ALL SELECT 'col '||c.relname||'.'||a.attname||' '||a.attnum||' '||format_type(a.atttypid,a.atttypmod)
        ||' '||a.attnotnull::text||' '||a.attidentity::text||' '||coalesce(pg_get_expr(d.adbin,d.adrelid),'-')
        FROM pg_attribute AS a JOIN pg_class AS c ON c.oid=a.attrelid
        LEFT JOIN pg_attrdef AS d ON d.adrelid=a.attrelid AND d.adnum=a.attnum
        WHERE c.relnamespace='syntra'::regnamespace AND c.relkind='r' AND a.attnum>0 AND NOT a.attisdropped
    UNION ALL SELECT 'con '||conrelid::regclass::text||' '||conname||' '||pg_get_constraintdef(oid)
        FROM pg_constraint WHERE connamespace='syntra'::regnamespace
    UNION ALL SELECT 'idx '||pg_get_indexdef(i.indexrelid)
        FROM pg_index AS i JOIN pg_class AS c ON c.oid=i.indrelid WHERE c.relnamespace='syntra'::regnamespace
    UNION ALL SELECT 'fn '||p.oid::regprocedure::text||' '||pg_get_function_result(p.oid)||' '||p.provolatile::text
        ||p.proparallel::text||p.prosecdef::text||p.proisstrict::text||p.proleakproof::text
        ||' '||coalesce(p.proacl::text,'-')||' '||p.prosrc
        ||' '||pg_get_function_arguments(p.oid)||' '||coalesce(p.proconfig::text,'-')
        ||' '||l.lanname||' '||p.procost||' '||p.prorows
        FROM pg_proc AS p JOIN pg_language AS l ON l.oid=p.prolang
        JOIN pg_depend AS d ON d.classid='pg_proc'::regclass AND d.objid=p.oid AND d.deptype='e'
        JOIN pg_extension AS e ON e.oid=d.refobjid AND e.extname='syntradb'
    UNION ALL SELECT 'evt '||evtname||' '||evtevent||' '||evtenabled::text||' '||evtfoid::regproc::text FROM pg_event_trigger
    UNION ALL SELECT 'member '||pg_describe_object(classid,objid,objsubid)
        FROM pg_depend WHERE deptype='e' AND refobjid=(SELECT oid FROM pg_extension WHERE extname='syntradb')
) AS q ORDER BY 1;
