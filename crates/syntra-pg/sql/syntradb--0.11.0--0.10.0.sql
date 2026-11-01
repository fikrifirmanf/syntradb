-- Dependent objects block removal; no CASCADE or captured-data conversion.
\echo Use "ALTER EXTENSION syntradb UPDATE TO '0.10.0'" to load this file. \quit

DROP FUNCTION public.syntra_heap_group_text_summary(regclass,text,text,integer,integer);
DROP FUNCTION public.syntra_heap_group_uuid_summary(regclass,text,text,integer,integer);
