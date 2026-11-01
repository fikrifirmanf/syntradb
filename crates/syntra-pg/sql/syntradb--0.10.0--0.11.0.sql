-- Add owned text/UUID grouping keys without changing captured bytes.
\echo Use "ALTER EXTENSION syntradb UPDATE TO '0.11.0'" to load this file. \quit

CREATE FUNCTION public.syntra_heap_group_text_summary(source regclass,key_column text,value_column text,max_groups integer DEFAULT 1024,max_key_bytes integer DEFAULT 8388608)
RETURNS TABLE(group_key text,rows bigint,non_nulls bigint,sum numeric,min bigint,max bigint)
LANGUAGE c STABLE STRICT PARALLEL UNSAFE
AS 'MODULE_PATHNAME','syntra_heap_group_text_summary_wrapper';

CREATE FUNCTION public.syntra_heap_group_uuid_summary(source regclass,key_column text,value_column text,max_groups integer DEFAULT 1024,max_key_bytes integer DEFAULT 8388608)
RETURNS TABLE(group_key uuid,rows bigint,non_nulls bigint,sum numeric,min bigint,max bigint)
LANGUAGE c STABLE STRICT PARALLEL UNSAFE
AS 'MODULE_PATHNAME','syntra_heap_group_uuid_summary_wrapper';
