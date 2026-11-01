# Grouped decimal measures

Version 0.12 adds fresh grouping over fixed-scale decimal measures. Choose the
function for your key column:

| Function | Stored key types | Result key |
| --- | --- | --- |
| `syntra_heap_group_integer_numeric_summary` | `int2`, `int4`, `int8` | `bigint` |
| `syntra_heap_group_text_numeric_summary` | `text` | `text` |
| `syntra_heap_group_uuid_numeric_summary` | `uuid` | `uuid` |

Each function takes `(source regclass, key_column text, value_column text,
max_groups integer DEFAULT 1024, max_key_bytes integer DEFAULT 8388608)`.
The measure must be stored `numeric(p,s)`, with precision 1 through 18 and scale
0 through precision. Generated columns, domains, unconstrained numeric, negative
scales and larger precisions are refused with SQLSTATE `0A000`.

```sql
CREATE TABLE invoice_lines (
    invoice_id bigint,
    category text COLLATE "C",
    amount numeric(18,2)
);
INSERT INTO invoice_lines VALUES (1,'parts',0.99),(1,'parts',0.02),(2,NULL,NULL);
SELECT group_key,rows,non_nulls,sum,min,max
FROM syntra_heap_group_text_numeric_summary('invoice_lines','category','amount');
```

The `parts` group returns rows 2, non_nulls 2, sum 1.01, min 0.02 and max 0.99.
The NULL key forms another group. An empty source returns no groups.

Rows and non_nulls return `bigint`; sum, min and max return `numeric`. Finite
values use exact scaled integers, with a wider accumulator for sums. No floating
point conversion occurs. Sums can exceed the source column's precision.

NULL measures count toward rows only. All-NULL groups return NULL aggregates.
Numeric NaN counts as non-NULL and makes sum and max NaN. Min returns the least
finite value when one exists, otherwise NaN. These rules match PostgreSQL.

## Limits and snapshots

Each call admits 1 through 65,536 groups and 1 through 67,108,864 distinct key
payload bytes. NULL uses one group slot and zero key bytes. Repeated keys incur
no additional payload charge. Integer keys cost their stored width: 2, 4 or 8
bytes. UUID keys cost 16 bytes. Text keys cost UTF8 payload bytes.

Invalid limits return `22023`. Exceeded limits return `54000` before any result
rows, including calls under `LIMIT 1`. These limits bound retained key payloads,
not backend RSS. Hash maps, input values and PostgreSQL output storage add memory.
Decimal conversion uses a resettable PostgreSQL scratch context for temporary
allocations.

Text keys require a UTF8 database and deterministic collation. Equality preserves
bytes, including trailing spaces and distinct Unicode normalization forms. The
result uses the calling SQL expression's collation. Apply an explicit outer
`COLLATE` and `ORDER BY` when you need a particular text order. UUID and integer
keys do not depend on database text encoding.

Calls read the caller's active MVCC snapshot and check source permissions and
row-level security restrictions. They support fresh serializable scans through
PostgreSQL's heap conflict checks. Cancellation discards the attempt; callers
retry in a valid transaction after handling the error.

These functions do not change capture formats, checkpoint keys or planner
routing. Existing integer-measure grouping signatures keep their current result
types. Time-window grouping and arbitrary SQL aggregation remain outside this
API.
