# Trusted live projections

The explicit live API can reuse an admin-published int4 segment. It returns a
fresh heap aggregate whenever reuse cannot be proved safe. PostgreSQL still owns
transactions, source storage, locks and snapshots. This remains an HTAP design.

```sql
-- Administrator: register the permanent source, then publish in read committed.
SELECT syntra_track_source('public.accounts', 'balance');
SELECT syntra_publish_trusted_int4('public.accounts', 'balance');

-- Reader: source SELECT and schema USAGE privileges are required.
SELECT rows, non_nulls, sum, min, max, route, reason
FROM syntra_live_int4_summary('public.accounts', 'balance');

SELECT rows, non_nulls, sum, min, max, route, reason
FROM syntra_live_int4_range_summary('public.accounts', 'balance', 0, 100);
```

Publication returns the captured row count. Both readers return one summary,
with `projection/eligible` for reuse or `heap` and a rejection reason. Range
bounds are inclusive. NULLs, empty inputs and inverted ranges follow the fresh
heap API. NULL arguments return no rows.

A repeated publication can reuse an already-current, fully validated segment.
That path returns its row count without replacing the catalog row or assigning
an XID. Transactions with an assigned XID still rebuild. See the
[refresh cost contract](refresh-costs.md) for the checks and measurement scope.

## Eligibility and visibility

The source, registration, all sixteen counters and cached binding share the
caller's exact active MVCC snapshot. Reads use native indexed catalog scans;
they do not run SPI statements that could advance the command snapshot.

Reuse requires matching database, cluster, source, attribute, type and storage
identities. The epoch and every counter must also match. Current native catalog
checks require the source statement guard and both unrestricted DDL guards to
be enabled ALWAYS. Missing metadata, changed bindings and damaged bytes cause
heap fallback. Segment validation runs before aggregation or group pruning.

An old read-only snapshot may reuse an old published segment after later commits.
A committed TRUNCATE changes storage identity, so old counters alone cannot
authorize reuse. Source read locks remain held until the transaction ends.

Any assigned transaction ID disables reuse for that transaction. This covers
own writes, modifying CTEs and nested row-trigger reads before statement counters
advance. Even unrelated writes cause fallback. Looking up that ID does not assign
one. Read-only calls can reuse segments without acquiring an ID.

Serializable transactions remain unsupported by the custom readers. Use native
PostgreSQL SQL for that isolation level. The source must be a supported heap
with a stored int4 column. RLS sources are rejected, including for their owner.
Trusted publication further requires a permanent table without inheritance or
rewrite rules. Ordinary queries receive source permission checks on every call.

## Publication and limits

Only a superuser can publish. Both the SQL grant and a native check enforce this.
The private catalog accepts no public reads or writes. User-owned fixed copies
remain historical artifacts and cannot establish live eligibility.

Publication requires a writable read committed transaction. It captures source
data and version metadata before issuing its catalog upsert. Bytes and binding
commit or roll back together. A transaction lock serializes publishers. It
never queues. A vacuum of the catalog is waited for, for up to one second, so it
does not fail publication; a concurrent publisher, or a longer vacuum, makes this
one return SQLSTATE `55P03`. Retry after the active transaction ends.
Some concurrent DDL can also hold a conflicting catalog lock.

Each segment allows at most 1,000,000 rows and 8 MiB. The catalog permits 64 live
column entries across the database. Replacing an existing entry works at capacity.
Older MVCC versions and table bloat consume extra space; this is no disk quota.
Publication scans the source synchronously. Schedule rebuilds as maintenance work.

Any committed source statement invalidates its binding, even when no rows change.
Readers then scan the heap until an administrator republishes. There is no
automatic refresh, delta replay, compaction or planner substitution in this step.

## Recovery and administration

The catalog uses logged PostgreSQL storage. Logical dumps omit trusted bindings
and tracking data. Restore requires explicit registration and republication.
Source drops remove bindings even in replica mode, where ordinary FK actions
may be skipped. These bindings do not survive restore as proof of freshness.

Superuser changes can bypass tracking. After disabling global guards or changing
engine metadata, restore the guards, rebind sources and republish. Current guard
checks cannot prove that a superuser never bypassed them in the past.

PostgreSQL tests cover publication, permissions, capacity and heap fallback.
The executable checks in `syntra-bench/src/trusted_checks.rs` use committed,
independent sessions for positive reuse and invalidation. They also test old
snapshots, nested row triggers, corrupt bytes, TRUNCATE and publication admission.
These checks establish a bounded API contract, not production readiness or a
speed advantage over other databases.

The [fixed-source measurements](../benchmarks/trusted-live-2026-10-27.md) compare
eligible reuse with fresh SQL and Rust scans. They charge publication separately.
