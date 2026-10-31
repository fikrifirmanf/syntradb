-- SyntraDB 0.8.0 to 0.7.0. Load through ALTER EXTENSION, never directly.
\echo Use "ALTER EXTENSION syntradb UPDATE TO '0.7.0'" to load this file. \quit

DROP FUNCTION public.syntra_heap_group_integer_summary(regclass,text,text,integer);
