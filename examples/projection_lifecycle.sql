-- Run in a database with the current SyntraDB extension installed.
-- Temporary tables keep this example private to the SQL session.
CREATE TEMP TABLE lifecycle_source (id int4 PRIMARY KEY, value int4);
INSERT INTO lifecycle_source (id, value) VALUES (1, 10), (2, NULL), (3, 30);

CREATE TEMP TABLE lifecycle_projection AS
SELECT 'example'::text AS name, 1::bigint AS generation,
       c.segment, c.source_database, c.source_relation, c.source_schema,
       c.source_table, c.source_column, c.source_attribute, c.source_type,
       c.source_snapshot, c.source_transaction, c.source_command,
       c.captured_at, c.source_rows
FROM syntra_capture_int4('lifecycle_source'::regclass, 'value') AS c;
ALTER TABLE lifecycle_projection ADD PRIMARY KEY (name);

SELECT p.name, p.generation, p.source_rows, s.sum
FROM lifecycle_projection AS p,
LATERAL syntra_segment_summary(p.segment) AS s
WHERE p.name = 'example';
-- generation=1, source_rows=3, sum=40

UPDATE lifecycle_source SET value = 20 WHERE id = 1;
-- A source update does not change an existing fixed projection.
UPDATE lifecycle_projection AS p
SET generation = p.generation + 1,
    segment = c.segment,
    source_database = c.source_database, source_relation = c.source_relation,
    source_schema = c.source_schema, source_table = c.source_table,
    source_column = c.source_column, source_attribute = c.source_attribute,
    source_type = c.source_type, source_snapshot = c.source_snapshot,
    source_transaction = c.source_transaction, source_command = c.source_command,
    captured_at = c.captured_at, source_rows = c.source_rows
FROM syntra_capture_int4('lifecycle_source'::regclass, 'value') AS c
WHERE p.name = 'example';

SELECT p.name, p.generation, p.source_rows, s.sum
FROM lifecycle_projection AS p,
LATERAL syntra_segment_summary(p.segment) AS s
WHERE p.name = 'example';
-- generation=2, source_rows=3, sum=50

DELETE FROM lifecycle_projection WHERE name = 'example';
