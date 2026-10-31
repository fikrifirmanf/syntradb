-- Remove only the additive temporal APIs; dependent objects block this update.
\echo Use "ALTER EXTENSION syntradb UPDATE TO '0.9.0'" to load this file. \quit

DROP FUNCTION public.syntra_heap_date_summary(regclass,text,text,date,date);
DROP FUNCTION public.syntra_heap_timestamp_summary(regclass,text,text,timestamp,timestamp);
DROP FUNCTION public.syntra_heap_timestamptz_summary(regclass,text,text,timestamptz,timestamptz);
