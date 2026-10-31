# Date and timestamp filters

Version 0.10.0 adds fresh heap aggregates over a time window. Use the function
that matches the time column's built-in PostgreSQL type:

| Time column | Function |
|---|---|
| `date` | `syntra_heap_date_summary` |
| `timestamp without time zone` | `syntra_heap_timestamp_summary` |
| `timestamp with time zone` | `syntra_heap_timestamptz_summary` |

Each function takes `(source, value_column, time_column, lower, upper)` and
returns one row: `rows`, `non_nulls`, `sum`, `min` and `max`. The two counts are
`bigint`; the other results are `numeric`. Values may be `smallint`, `integer`,
`bigint` or fixed-scale `numeric(p,s)` with `1 <= p <= 18` and `0 <= s <= p`.
Integer sums can exceed the `bigint` range without a floating-point conversion.

```sql
CREATE TABLE orders (
    id bigint PRIMARY KEY,
    created_at timestamptz NOT NULL,
    amount numeric(18,2)
);

SELECT rows, non_nulls, sum, min, max
FROM syntra_heap_timestamptz_summary(
    'orders', 'amount', 'created_at',
    timestamptz '2026-11-01 00:00:00+00',
    timestamptz '2026-12-01 00:00:00+00'
);
```

This matches the aggregates from this native query:

```sql
SELECT count(*), count(amount), sum(amount), min(amount), max(amount)
FROM orders
WHERE created_at >= timestamptz '2026-11-01 00:00:00+00'
  AND created_at <  timestamptz '2026-12-01 00:00:00+00';
```

## Window and NULL semantics

Windows include the lower bound and exclude the upper bound. Adjacent windows
therefore do not count a boundary row twice. Equal or reversed bounds produce
zero rows, zero non-NULLs and NULL aggregates. NULL time values never match.
NULL value columns still count toward `rows`, but not `non_nulls` or the sum.
Decimal NaN follows native PostgreSQL aggregate ordering and sum behavior.

The SQL functions are strict. A NULL source, column name or bound returns no
result row, rather than an aggregate over an open-ended window. Use explicit
`-infinity` or `infinity` bounds when needed. The exclusive upper comparison
also excludes a stored positive-infinity value when the upper bound is infinity.

PostgreSQL parses the typed bounds and compares stored temporal values. There
is no Unix-epoch or floating-point conversion in the filter. A `timestamptz`
window compares instants; a `timestamp` window compares local date/time values.
Use explicit offsets when a daylight-saving transition could make a local time
ambiguous. See PostgreSQL's [date/time types](https://www.postgresql.org/docs/18/datatype-datetime.html)
and [comparison rules](https://www.postgresql.org/docs/18/functions-datetime.html).

The time column must match the selected function's type. For example, the
`timestamptz` function rejects a `timestamp` column rather than silently casting
the source using the session time zone. PostgreSQL still applies its normal
type coercion rules to SQL arguments before calling the function.

## Snapshot and access contract

These functions use the caller's active MVCC snapshot. They see prior writes
in the same transaction and retain the statement snapshot in a modifying CTE.
Fresh scans use PostgreSQL's relation predicate locks and visibility conflict
checks under serializable isolation. A serialization failure requires retrying
the whole transaction.

The source must be an ordinary heap table without partitions or descendants.
The caller needs schema USAGE and table SELECT. RLS tables, generated columns,
domains, floating-point values and unconstrained numeric values are rejected.
The relation lock remains held to transaction end, protecting the checked
attributes from concurrent DDL. Cancellation must leave the backend reusable.

## Scope

This is an explicit fresh scan. It does not add automatic planner routing,
time buckets, temporal grouping, index scans or timestamp checkpoint storage.
The filter uses native heap scan keys and the existing Rust integer/decimal
aggregate path. No speed improvement over native PostgreSQL is claimed.

Install the 0.10.0 library, restart sessions that loaded the older library, and
run `ALTER EXTENSION syntradb UPDATE` in each database. A preloaded library
requires a PostgreSQL restart. The SQL update adds three functions without
rewriting capture data. Downgrading to 0.9.0 removes them; dependent user views
block that downgrade rather than being dropped.

Local and Linux verification status is recorded in
[the 0.10.0 verification record](verification-0.10.0.md).
