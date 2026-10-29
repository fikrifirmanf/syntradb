# Transactional source tracking

This experimental layer counts source mutations. No query path uses its counter
to choose a projection. It is a first step toward mutable HTAP, with known gaps
that must close before a live cache can trust it.

An administrator can install tracking on a permanent heap table:

```sql
SELECT syntra_track_source('public.accounts', 'balance');
SELECT epoch, generation, guard_config_valid, generation_vector
FROM syntra_source_tracking_status('public.accounts');
```

Setup requires a superuser, even after an EXECUTE grant. It takes an exclusive
source lock and creates an AFTER STATEMENT trigger through PostgreSQL's typed
utility API. Relation names never enter a dynamic SQL string. Setup checks the
source column and rejects RLS, partitions, inheritance, temporary and unlogged
tables. Unlogged tables can reset after a crash without firing mutation triggers.

The Rust trigger handles INSERT, UPDATE, DELETE and TRUNCATE. It fires in replica
mode too. One counter update runs per event and statement, rather than per row.
Empty statements can advance it. MERGE can fire several statement events. These
extra increments are conservative invalidations, not exact changed-row counts.

Counter updates share the source transaction. Abort undoes both. Other sessions
cannot see an uncommitted counter, and repeatable-read sessions retain their old
version. Normal writers need no access to the private counter table. The trigger
uses a fixed search path and definer rights for one bound UPDATE. Its actual
relation and trigger OIDs select the counter row. A missing registration rejects
the source write instead of silently omitting tracking.

## Current guard checks are not history proofs

The status call checks the current trigger OID, function, event mask, firing mode,
arguments, and relation type. A disabled or dropped trigger produces false. RLS
and inheritance also make the guard configuration invalid.

DDL event guards now advance the epoch when a registered source or its trigger
changes. An owner's disable/write/enable sequence changes the epoch even when
its mutation counter stays unchanged. Dropped tables and schema cascades remove
registrations. DDL rollback also rolls back epoch and registration changes.
Unrelated table DDL does not update other sources.

The guards use PostgreSQL's command and dropped-object addresses, rather than
parsing SQL text. Both fire in replica mode. The status call checks that both are
active and that the session has event triggers on. Setup refuses disabled guards.
Read-only maintenance skips epoch writes; physical storage checks remain separate.
See [PostgreSQL event trigger behavior](https://www.postgresql.org/docs/18/event-trigger-definition.html).

A superuser can still disable all event triggers, alter private state, or grant
such powers to another role. After that maintenance, restore the guards and rebind
sources before any future live use. A current configuration check cannot prove
that an administrator never bypassed tracking.

Trusted cache storage, locked eligibility reads, matching snapshots and own-command
visibility rules remain required. Neither generation nor epoch equality alone
may authorize a live scan.

TRUNCATE needs another guard. An old snapshot can see the old counter while its
first heap read sees an empty table. PostgreSQL documents this
[TRUNCATE visibility behavior](https://www.postgresql.org/docs/18/sql-truncate.html).
The recovery runner preserves that counterexample. A live reader must check the
locked source's current storage identity and reject a binding after a rewrite.
An MVCC counter or DDL epoch alone does not cover this case.

`syntra_source_identity(regclass, column)` now exposes the locked native storage
locator and column metadata. It includes cluster, database, relation, tablespace,
file, attribute and type identity. The call checks source read privileges and
the same supported relation rules as capture. It is volatile and reads the
current relation descriptor, rather than an old SQL catalog row.
Types without collation return NULL for `source_collation`. Shared storage returns
NULL for `storage_database`; the logical source database remains explicit.

The old-snapshot test now verifies that this locator changes after a committed
TRUNCATE, while the counter remains old. A truncate of newly created storage can
also happen in place. Own-command changes and transactional counters must cover
that case. Storage identity alone never proves unchanged contents or freshness.

## Counter shards and cost

Since 0.5.0, writes to a captured source skip the counter update. Its feed
already records every committed row change, and trusted reuse compares the feed
position instead ([upgrades](upgrades.md#040-to-050)). Under a held snapshot,
every update of a counter row walked the versions the snapshot kept
([findings](../benchmarks/held-snapshot-writers-2026-10-29.md)). The status
call's `generation` therefore stops counting while capture is installed.

Each source now has sixteen counter shards. The full transaction ID selects one
shard. A statement updates only that row, while the source registry remains read
only during DML. Transactions can still collide on a shard. This reduces the
single-row lock bottleneck without promising contention-free writes.

The status call returns a vector ordered by shard ID. Its scalar `generation`
is the sum, for diagnostics. A future cache must compare the full vector and
epoch, not just that sum. A missing shard makes the guard configuration invalid.
Rebind repairs the vector and starts a new epoch.

Each counter stops at 576460752303423487. The sum of all sixteen counters fits
int8. Exhaustion rejects and rolls back the source write until an admin rebinds
it. Counter storage and metadata reads stay bounded per source.

The [counter comparison](../benchmarks/counter-shards-2026-10-27.md) records both
costs and gains. At 5,000 offered TPS, the extra metadata lookup raised request
cost. At 20,000 offered TPS with eight clients, request p99 improved, while client
scheduling spikes remained. Tracking stays off by default. These results do not
establish a broad OLTP or HTAP speed advantage.

## Recovery and rebind

Counter state uses logged PostgreSQL rows. Process-crash checks verify committed
increments survive and aborted increments disappear with their source changes.
Logical backup deliberately omits counter data. Restored user triggers reject
writes until an administrator calls `syntra_track_source` on each restored source.

That call accepts an existing compatible guard, assigns a new epoch and resets
all sixteen counters. It refuses a disabled or altered guard. Rebinding an existing source
also starts a new epoch. Future cache bindings must include that epoch and the
database identity; old OIDs and counters cannot establish freshness after restore.

The [trusted live API](trusted-live-projections.md) now compares the full vector,
epoch and current storage identity. It falls back after an assigned transaction
ID or any failed check. Readers do not refresh stale segments.

Release upgrades and cross-cluster repair automation remain unfinished.
Managed fixed projections retain their snapshot contract throughout these steps.
