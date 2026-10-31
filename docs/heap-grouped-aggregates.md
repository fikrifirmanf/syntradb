# Fresh integer grouping from SQL

Version 0.8.0 adds `syntra_heap_group_integer_summary`. It groups two stored integer
columns directly from the heap at the caller's active PostgreSQL snapshot.
Capture registration and projection maintenance are optional for this read path.

```sql
CREATE TABLE grouped_readings(category int4, value int8);
INSERT INTO grouped_readings VALUES (1,10),(1,NULL),(2,7),(NULL,-2),(NULL,NULL);

SELECT group_key, rows, non_nulls, sum, min, max
FROM syntra_heap_group_integer_summary('grouped_readings','category','value',1024)
ORDER BY group_key NULLS FIRST;
```

This returns three groups: NULL with two rows and sum -2, category 1 with two
rows and sum 10, and category 2 with one row and sum 7.

## Types and aggregate semantics

Key and value columns independently accept built-in `int2`, `int4` or `int8`.
The result key, counts, minimum and maximum are `bigint`. SUM returns `numeric`,
matching `sum(value::bigint)` for every input layout. The engine uses checked
`i128` arithmetic and makes no floating-point conversion.

NULL keys share one group. NULL values count toward `rows` but not `non_nulls`.
A NULL-only group has NULL sum, minimum and maximum. Empty input returns no groups.
The same column can be used for key and value. SQL NULL input arguments return
no rows because the function is strict.

## Limits and errors

The optional fourth argument defaults to 1,024 groups. It accepts 1 through
65,536, including the NULL group. Other limits fail with SQLSTATE `22023`.
A new key beyond the budget fails with `54000`, including under `LIMIT 1`.
Aggregation completes before the function emits any group, so a limit failure
cannot return partial results.

The cap bounds Rust grouping state. PostgreSQL also owns tuple slots, numeric
results and its output tuplestore; this is not a backend RSS quota. The input is
streamed, so row count is independent of the group-count budget.

PostgreSQL cancellation is polled during the scan and result conversion.
`statement_timeout` stops a long grouping query with SQLSTATE `57014`.
An allocation failure is reported as `53200`; checked arithmetic failures use
`22003`.

## Permissions and visibility

The caller needs schema USAGE and table SELECT privileges. Column-only grants
are insufficient, matching the existing fresh heap API. The function runs with
the caller's privileges and rejects row-level security, including for an owner.
It supports ordinary heap tables without partitions or inheritance descendants.
Views, generated columns, domains and other column types are rejected explicitly.

The key and value come from the same visible tuple. Repeatable read retains its
snapshot across other sessions' commits. Read committed sees subsequent committed
changes at the next statement. Prior writes in the caller's transaction are visible;
a data-modifying CTE retains the statement snapshot, matching native SQL.
Read-only calls do not assign a transaction ID. Version 0.9.0 adds
[serializable fresh grouping](serializable-heap-reads.md) through PostgreSQL's
native predicate locks and conflict checks. Capture and cached reads retain
their separate `0A000` restriction.

Source AccessShareLock references are retained until transaction end to protect
the checked column types and heap from DDL. Long transactions can therefore hold
up a schema change after the grouping call has returned.

## Installation and verification

Install the 0.8.0 library, then run `ALTER EXTENSION syntradb UPDATE`. The upgrade
adds the SQL function and preserves capture storage. Downgrading to 0.7.0 removes
the function. Dependent user views block that downgrade atomically; remove their
dependencies explicitly before retrying. Capture continues with either library.

PostgreSQL tests compare all nine integer layouts with native grouped aggregates,
including NULLs, wide sums and the maximum result cardinality. The benchmark's
pre-timing checks use independent sessions to check snapshots, own writes,
read-only calls, DDL locking and cancellation with subsequent reuse. The
[standalone kernel tests](grouped-aggregates.md) also retain independently generated
PostgreSQL fixtures and differential fuzzing.

Call this function to select the Rust path. Normal SQL GROUP BY uses PostgreSQL's
existing executor. SQL predicates, multiple grouping columns, spillable Rust state
and automatic planner routing remain future work. Performance comparisons for this
API are pending.
