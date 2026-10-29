# Transactional keyed capture

```sql
-- Administrator, in writable read committed mode.
SELECT syntra_seed_capture('public.accounts', 'id', 'balance', 4096);

-- Normal SQL writes now append private transactional changes.
UPDATE public.accounts SET balance = balance + 1 WHERE id = 1;

-- Administrator: apply a durable checkpoint and free retained event capacity.
SELECT syntra_apply_capture('public.accounts');
SELECT syntra_prune_capture('public.accounts');

-- Discard history and capture a fresh base when required.
SELECT syntra_seed_capture('public.accounts', 'id', 'balance', 4096);

-- Retire capture while preserving the source and ordinary statement tracking.
SELECT syntra_stop_capture('public.accounts');
```

Capture is opt-in. Sources must be permanent heap tables with one nondeferrable
int2, int4 or int8 primary-key column and a stored int2, int4, int8 or
`numeric(p,s)` value column, with p ≤ 18. Values may be NULL. The widths are fixed at seed and stored in
`syntra.capture_bindings`; changing a column's type needs a reseed.
`syntra_checkpoint_int4_summary` reads an int2 or int4 value column and
`syntra_checkpoint_int8_summary` an int8 one, whose sum is `numeric` as in
PostgreSQL, and `syntra_checkpoint_numeric_summary` a `numeric` one. See the
[int8 design](int8-design.md), [int2 columns](int8-design.md#int2-columns-040)
and [numeric columns](numeric-design.md).
Partitioning, inheritance, RLS and rewrite rules remain unsupported.
Other source triggers and foreign-key triggers are also rejected. Nested writes
from those callbacks can reverse capture order. This first path accepts only its
own row, truncate and statement guards.

The private logged tables hold a keyed base, a feed head and ordered changes.
No public grants permit reading or editing those tables. The admin functions
also check superuser status in Rust, even after an accidental EXECUTE grant.
Source writers need only their normal source privileges.
The catalogs are tested on fresh installs and on an upgrade from 0.1.0; see
[upgrades](upgrades.md).

## Base boundary and write contract

Seeding holds an exclusive source lock until transaction end. It rejects a source
already in use by an active executor in the same session. That prevents trigger
installation midway through a statement that could omit later rows.

The seed installs or adopts compatible ALWAYS triggers, then pins a latest MVCC
snapshot after lock admission. It does not reuse the older calling statement's
snapshot. A writer that commits while seeding waits must appear in the base.
The head records the snapshot text, seed transaction and command ID. Own earlier
source writes may also appear in that base; snapshot text alone is no proof of
the boundary. Source data, metadata and installed guards commit together.

AFTER ROW capture records inserts, updates and deletes. A key move produces a
delete and insert together. Each event distinguishes an absent row from a present
NULL and carries the prior value, replacement, origin transaction and append
command ID. That command ID is diagnostic, not a SQL statement grouping key.

Events and cursor reservations use the source transaction. Abort and savepoint
rollback remove them with the source changes. A row lock on the feed head orders
writers per source and keeps committed event prefixes contiguous. It can serialize
writes and cause ordinary transaction deadlocks or serialization errors. Retry the
whole transaction when PostgreSQL requires it.

From 0.6.0 the row trigger is a constraint trigger, `DEFERRABLE INITIALLY
DEFERRED`. It fires for each changed row when the transaction commits, so the
head lock is held only from commit-time capture to commit, not from the first
captured statement on. Capture errors, a full log among them, are raised at
`COMMIT` and roll back the whole transaction; a savepoint around one statement
does not catch them. A transaction's own uncommitted changes are not in the
feed. `SET CONSTRAINTS ALL IMMEDIATE`, or naming `_syntra_capture_row`, fires
the trigger per statement again. This initial path makes no
write-scaling claim. Capture remains off by default.

Writers lock the head row but do not update it. Each row callback appends one
`capture_marks` row holding the last ordinal it reserved and the live row count
after it. The feed's position is the newest mark above the head, read under a
latest snapshot so a writer sees every mark committed before its lock was granted.
Pruning folds old marks into the head. Until 0.2.0 every captured write updated
the head row; a long-held snapshot then left one unremovable version per write,
and every lookup of that row walked them all. Readers reject an event past the
newest mark as a history gap, so losing the newest mark is detected as losing the
newest event was before.

Each row callback checks current registration, storage identity, trigger definitions
and global DDL guards. It pins a latest metadata snapshot even when the writer has
an older repeatable-read snapshot. A stale epoch or guard configuration rejects
the write. Owner DDL that disables capture can still create a history gap; the
stream then needs reseeding before use. Superuser bypass cannot be proved absent.

TRUNCATE is rejected while capture is installed. Retirement removes capture
triggers and private history under an exclusive source lock. It retains the
source and statement tracker, and changes the tracking epoch. On an ordinary
registered source, SQL writes and TRUNCATE can then resume without row capture.

## Capacity and recovery

The database permits 16 live capture sources. Each base allows 100,000 rows;
each feed allows at most 4,096 retained events. A key move consumes two events.
Filling the log rejects and rolls back the source statement. Apply and prune
can free its retained budget. No older event is
silently deleted to admit a write. Reseeding discards old history and creates a
fresh epoch and base; retirement also discards history. Both actions are explicit.

The event limit counts changes, rather than rows in the current source. Inserts
can grow a source to 1,000,000 live rows, beyond the 100,000-row seed cap. One `int4` source grown to 1,000,000 rows
by captured inserts read back exactly, before and after a crash
([resource bounds](resource-bounds.md#growth-past-the-seed-cap)).
A write exceeding the live-row cap fails. Reseeding an oversized source requires retiring
capture and reducing the source first. Transaction working memory is not capped.
Old MVCC versions, indexes, WAL and allocator overhead consume extra space. These
logical caps are not a disk or RSS quota. Setup also holds a private catalog lock
that can temporarily block captured writers on other sources. Use bounded lock
and statement timeouts for maintenance calls.

Process-crash checks preserve committed bases and events and discard uncommitted
source changes and reservations. Logical dumps omit capture history. Restored
triggers reject writes until an administrator explicitly reseeds. Retirement after
restore removes capture, but ordinary tracking still needs repair before writes.
Native extension removal also removes dependent capture triggers.

## Verified scope and next step

PostgreSQL tests cover event values, provenance, source scope, private permissions,
rollback, limits, retirement, replica mode and source-drop cleanup. Committed
session checks wait on a real blocked seed, test old snapshots and reconstruct
source rows through the Rust replay kernel. Deliberately using old seed or metadata
snapshots made those checks fail. Bypassing the event budget failed its test too.

The [durable checkpoint layer](durable-checkpoints.md) now persists materialized
rows and applied progress together. Its explicit summary uses a checkpoint only
when it matches the reader's visible head. Lagging readers fall back to the heap.
Pruning retains events newer than the applied checkpoint and preserves old MVCC
versions. Automatic maintenance and planner integration remain unfinished.

The [capture findings](../benchmarks/transactional-capture-2026-10-27.md) record
passing recovery checks, raw write costs and matching source inputs.
