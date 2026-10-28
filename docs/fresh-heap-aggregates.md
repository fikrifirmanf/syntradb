# Fresh Rust heap aggregates

These SQL functions aggregate the caller's current snapshot directly from a heap
table. They create no segment, cache entry, or change log.

```sql
SELECT rows, non_nulls, sum, min, max
FROM syntra_heap_int4_summary('public.accounts', 'balance');

SELECT rows, non_nulls, sum, min, max
FROM syntra_heap_int4_range_summary('public.accounts', 'balance', 100, 500);
```

The all-row function matches COUNT(*), COUNT(column), SUM, MIN and MAX. Empty input
returns zero counts and NULL sum/min/max. NULL values count toward rows but not
non_nulls. The range function uses inclusive BETWEEN semantics and excludes NULLs.
An inverted range returns zero counts and NULL aggregates. NULL arguments produce
no result row, following the existing strict function API.

## Visibility and access

The adapter uses PostgreSQL's active MVCC snapshot, table access method and tuple
slots. Rust holds only aggregate state. Read committed, repeatable read, prior
own writes, and a modifying CTE share the tested native snapshot behavior.
Read-only calls assign no transaction ID. An AccessShare lock stays until commit
or rollback, including calls through generic prepared plans. This prevents a
concurrent table rewrite after a function returns but before its transaction ends.

Calls require schema USAGE and table SELECT. Column-only grants are insufficient.
Sources must be ordinary heap tables with a stored int4 column. RLS, partitions,
inheritance parents, generated columns and other types remain outside the scope.
Serializable calls return SQLSTATE 0A000 until predicate-lock tests establish that
scope. Normal PostgreSQL queries remain available for unsupported cases.

## Execution and limits

Range bounds become native PostgreSQL heap scan keys. Filtering happens before
matching values cross into Rust; Rust also checks the predicate. PostgreSQL owns
key storage and applies native visibility checks. Its heap scanner checks for
interrupts at least once per page, including scans that match no rows. The Rust
stream kernel checks cancellation every 1,024 values and at completion.

This path still scans heap pages. It does not select indexes, prune columnar
groups, or imply lower physical IO. A selective native index plan may be faster.
No planner hook automatically substitutes these functions for native SQL.

Aggregate state stays constant as the table grows. The segment encoder's one
million row cap does not apply. Checked sums and counts report SQLSTATE 22003 on
overflow. PostgreSQL statement timeouts and cancellation still apply.

## Verification and measurements

Tests compare native SQL results for NULLs, integer endpoints, empty and inverted
ranges, own writes, modifying CTEs and a source above the segment row cap. Real
two-session checks cover snapshots, read-only calls and transaction lock retention.
Removing the retained lock made the generic-plan lock check fail. Changing an
inclusive comparison to a strict one also caused a test failure.

The [fresh heap measurements](../benchmarks/fresh-heap-2026-10-27.md) include normal
prepared requests and separate EXPLAIN timings. Prepared requests avoid per-tuple
EXPLAIN instrumentation, which can otherwise exaggerate the apparent gain.
Mixed phases compare equally fresh data and report errors and missed arrivals.
