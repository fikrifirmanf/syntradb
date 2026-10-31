-- SyntraDB 0.9.0 to 0.8.0. Load through ALTER EXTENSION, never directly.
\echo Use "ALTER EXTENSION syntradb UPDATE TO '0.8.0'" to load this file. \quit

-- No catalog or byte conversion is needed. Install the 0.8.0 library and restart
-- sessions after this update to restore its serializable fresh-read rejection.
