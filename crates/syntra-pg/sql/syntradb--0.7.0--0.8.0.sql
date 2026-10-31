-- SyntraDB 0.7.0 to 0.8.0. Load through ALTER EXTENSION, never directly.
\echo Use "ALTER EXTENSION syntradb UPDATE TO '0.8.0'" to load this file. \quit

CREATE FUNCTION public.syntra_heap_group_integer_summary(
    source regclass, key_column text, value_column text, max_groups integer DEFAULT 1024
)
RETURNS TABLE(group_key bigint, rows bigint, non_nulls bigint, sum numeric, min bigint, max bigint)
LANGUAGE c STABLE STRICT PARALLEL UNSAFE
AS 'MODULE_PATHNAME', 'syntra_heap_group_integer_summary_wrapper';
