# Text and UUID grouping keys

Version 0.11.0 adds two explicit fresh heap aggregate functions:

```sql
SELECT group_key, rows, non_nulls, sum, min, max
FROM syntra_heap_group_text_summary('events', 'category', 'value');

SELECT group_key, rows, non_nulls, sum, min, max
FROM syntra_heap_group_uuid_summary('events', 'customer_id', 'value');
```

The key column must be built-in `text` or `uuid`, matching the function name.
The value column accepts `smallint`, `integer` or `bigint`. SUM returns `numeric`
and preserves sums beyond the bigint range. Counts, minimum and maximum return
`bigint`. Decimal values and multiple grouping columns remain outside these APIs.

For example:

```sql
CREATE TABLE events (
    customer_id uuid,
    category text COLLATE "C",
    value bigint
);

INSERT INTO events VALUES
 ('00000000-0000-0000-0000-000000000001', 'orders', 1200),
 ('00000000-0000-0000-0000-000000000001', 'orders', 800),
 (NULL, NULL, NULL);

SELECT group_key, rows, sum
FROM syntra_heap_group_text_summary('events', 'category', 'value')
ORDER BY group_key COLLATE "C" NULLS FIRST;
```

The `orders` group has two rows and sum 2000. The NULL group has one row,
zero non-NULL values and NULL aggregates. An empty source returns no groups.
All arguments are strict: a NULL argument also returns no groups.

## Text equality and collation

Text grouping requires a UTF8 database and a deterministic source-column
collation. PostgreSQL's deterministic collation rules distinguish unequal byte
strings even when their locale sort weights tie. This allows Rust byte equality
to preserve the same groups. Composed and decomposed Unicode strings therefore
remain separate groups unless their bytes match. See PostgreSQL's
[collation rules](https://www.postgresql.org/docs/18/collation.html).

Nondeterministic collations can equate different strings. These APIs reject
such a source with SQLSTATE `0A000`; use native SQL GROUP BY for that collation.
The `char(n)`, `varchar`, `citext` and domain types are also rejected. They need
their own type and equality contract before admission.

Output text follows SQL function-expression collation rules. The source column's
collation does not attach itself to the returned field. Apply the source's
COLLATE explicitly to outer comparisons or ORDER BY when needed. Results are
emitted in byte order with NULL first; use SQL ORDER BY for the desired order.

UUID grouping compares all 16 bytes. Different accepted spellings of the same
UUID join the same group. UUID grouping also passed the LATIN1 database check.

## Limits and errors

Both functions accept optional `max_groups` and `max_key_bytes` arguments:

```sql
SELECT group_key, rows, sum
FROM syntra_heap_group_text_summary('events', 'category', 'value', 4096, 8388608);
```

The defaults are 1,024 groups and 8 MiB of distinct key payloads. Hard limits
are 65,536 groups and 64 MiB. Invalid limits raise `22023`. Exceeding a valid
limit raises `54000` before any result row, including under SQL LIMIT 1.

NULL counts as one group and zero key bytes. Repeated keys consume no additional
retained key bytes. Text counts UTF8 bytes, including trailing spaces; UUID
counts 16 bytes per distinct non-NULL key. A text key's uncompressed size is
checked before copying or detoasting an oversized value.

These bounds cover logical key payloads and group count. They do not impose a
backend RSS quota or replace PostgreSQL work_mem. Hash-table state, transient
input copies, tuple buffers and result storage add memory overhead.

## Visibility and access

The functions use the caller's active MVCC snapshot. They preserve statement
snapshots, prior own writes and repeatable-read key/value versions. Fresh SSI
reads use PostgreSQL relation predicate locks and visibility conflict checks.
Cancellation and limit errors must leave the backend reusable.

The normal fresh-scan restrictions apply: schema USAGE, table SELECT, ordinary
heap tables, no RLS, no partitions or descendants and no generated key/value
columns. The source relation stays locked against DDL through transaction end.

This adds fresh grouping APIs. It does not change capture key types, checkpoint
formats, automatic planner routing or the normal native SQL executor. No speed
improvement over native GROUP BY is claimed.

Install the new library and restart sessions that loaded an older library;
restart PostgreSQL if preloaded. Then run `ALTER EXTENSION syntradb UPDATE` in
each database. Downgrade to 0.10.0 removes only the two new functions. Dependent
views block removal atomically. See [verification](verification-0.11.0.md).
