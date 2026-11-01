-- Add fixed-scale decimal measures to fresh heap grouping.
\echo Use "ALTER EXTENSION syntradb UPDATE TO '0.12.0'" to load this file. \quit

CREATE FUNCTION public.syntra_heap_group_integer_numeric_summary(source regclass,key_column text,value_column text,max_groups integer DEFAULT 1024,max_key_bytes integer DEFAULT 8388608)
RETURNS TABLE(group_key bigint,rows bigint,non_nulls bigint,sum numeric,min numeric,max numeric)
LANGUAGE c STABLE STRICT PARALLEL UNSAFE
AS 'MODULE_PATHNAME','syntra_heap_group_integer_numeric_summary_wrapper';

CREATE FUNCTION public.syntra_heap_group_text_numeric_summary(source regclass,key_column text,value_column text,max_groups integer DEFAULT 1024,max_key_bytes integer DEFAULT 8388608)
RETURNS TABLE(group_key text,rows bigint,non_nulls bigint,sum numeric,min numeric,max numeric)
LANGUAGE c STABLE STRICT PARALLEL UNSAFE
AS 'MODULE_PATHNAME','syntra_heap_group_text_numeric_summary_wrapper';

CREATE FUNCTION public.syntra_heap_group_uuid_numeric_summary(source regclass,key_column text,value_column text,max_groups integer DEFAULT 1024,max_key_bytes integer DEFAULT 8388608)
RETURNS TABLE(group_key uuid,rows bigint,non_nulls bigint,sum numeric,min numeric,max numeric)
LANGUAGE c STABLE STRICT PARALLEL UNSAFE
AS 'MODULE_PATHNAME','syntra_heap_group_uuid_numeric_summary_wrapper';
