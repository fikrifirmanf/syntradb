-- Refuse dependent user objects; preserve capture and checkpoint data.
\echo Use "ALTER EXTENSION syntradb UPDATE TO '0.11.0'" to load this file. \quit
DROP FUNCTION public.syntra_heap_group_integer_numeric_summary(regclass,text,text,integer,integer) RESTRICT;
DROP FUNCTION public.syntra_heap_group_text_numeric_summary(regclass,text,text,integer,integer) RESTRICT;
DROP FUNCTION public.syntra_heap_group_uuid_numeric_summary(regclass,text,text,integer,integer) RESTRICT;
