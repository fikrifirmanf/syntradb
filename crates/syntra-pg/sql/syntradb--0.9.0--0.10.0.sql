-- Additive fresh temporal aggregate APIs; no captured bytes change.
\echo Use "ALTER EXTENSION syntradb UPDATE TO '0.10.0'" to load this file. \quit

CREATE FUNCTION public.syntra_heap_date_summary(source regclass,value_column text,time_column text,lower date,upper date)
RETURNS TABLE(rows bigint,non_nulls bigint,sum numeric,min numeric,max numeric)
LANGUAGE c STABLE STRICT PARALLEL UNSAFE
AS 'MODULE_PATHNAME','syntra_heap_date_summary_wrapper';

CREATE FUNCTION public.syntra_heap_timestamp_summary(source regclass,value_column text,time_column text,lower timestamp,upper timestamp)
RETURNS TABLE(rows bigint,non_nulls bigint,sum numeric,min numeric,max numeric)
LANGUAGE c STABLE STRICT PARALLEL UNSAFE
AS 'MODULE_PATHNAME','syntra_heap_timestamp_summary_wrapper';

CREATE FUNCTION public.syntra_heap_timestamptz_summary(source regclass,value_column text,time_column text,lower timestamptz,upper timestamptz)
RETURNS TABLE(rows bigint,non_nulls bigint,sum numeric,min numeric,max numeric)
LANGUAGE c STABLE STRICT PARALLEL UNSAFE
AS 'MODULE_PATHNAME','syntra_heap_timestamptz_summary_wrapper';
