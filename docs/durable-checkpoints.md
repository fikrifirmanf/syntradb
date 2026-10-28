# Durable replay checkpoints

Captured int4 changes can now produce a durable analytical checkpoint. PostgreSQL
commits its keyed partitions, analytical bytes and applied event manifest in one
transaction. Readers use a checkpoint visible to their own snapshot or scan the heap.
This remains an explicit SQL API over the same transactional source.

```sql
-- Administrator, writable READ COMMITTED transaction.
SELECT syntra_seed_capture('public.accounts', 'id', 'balance', 4096);
SELECT syntra_apply_capture('public.accounts'); -- Returns applied event ordinal.

-- Ordinary SQL writes append capture events in their source transaction.
UPDATE public.accounts SET balance = balance + 1 WHERE id = 1;

-- Explicit maintenance; these may share one transaction.
SELECT syntra_apply_capture('public.accounts');
SELECT syntra_prune_capture('public.accounts'); -- Returns events deleted.

-- Requires normal source privileges.
SELECT rows, non_nulls, sum, min, max, route, reason
FROM syntra_checkpoint_int4_summary('public.accounts', 'balance');
```

## Apply and publication

Apply validates current source identity, capture guards and tracking epoch. It
loads the latest committed capture head after lock admission. The first apply
seeds from the stored base. Later calls decode keyed state and replay only the
unapplied suffix. Every ordinal must be present. Before values must match,
including the distinction between absent rows and present NULLs.

The replay cursor advances by event count. A key move consumes two ordinals.
Apply checks the resulting row count against the transactional head. It then
encodes the affected keyed partitions and columnar segments. A bulk UPSERT writes
those partitions, followed by a manifest UPSERT in the same transaction. The
manifest binds every partition to its revision and digests.
Validation failures leave the old checkpoint unchanged.

Maintenance requires a superuser in writable READ COMMITTED mode. It retains a
source read lock until transaction end. A catalog lock admits one
maintenance transaction at a time across capture sources. The lock never queues.
A competing apply or prune call fails with `55P03` at once; only a vacuum-mode
holder is waited for, every 10 ms for up to one second. The retry exists because VACUUM and ANALYZE of the
catalog hold a conflicting lock for a few milliseconds; before 0.3.0 it was not
retried, and autovacuum alone could make maintenance fail as busy. Source writers do not acquire that catalog lock.
Use short transactions and explicit statement and lock timeouts.

An unchanged apply validates both representations without replacing the catalog
row. An unchanged prune also performs no write. Those calls assign no XID unless
the surrounding transaction has already acquired one. Invalid state fails with
`55000`; it never authorizes pruning or silently resets progress.

## Reader snapshot rule

The public summary reads the capture head and checkpoint using the source query's
active MVCC snapshot. Reuse requires matching capture epochs, the requested value
column, and `checkpoint.applied` equal to the visible feed position (the newest
mark, else the head) for direct reuse. A lagging
checkpoint may use [bounded reader replay](reader-tail-replay.md) through the
three-argument overload with `replay_tail = true`. The two-argument call retains
heap fallback on lag. Segment checksums, metadata and
row counts must also pass. Current source identity and guard checks remain
mandatory. A cursor by itself is not proof of visibility.

A missing, invalid or unsupported checkpoint, a default-mode lag, or a tail beyond
its opt-in replay limits,
selects a fresh heap scan
under that same snapshot. Assigned-XID transactions also use the heap, preserving
own-write behavior during trigger execution. Source ACLs and schema permissions
are checked for the actual caller. Private catalog privileges are never needed.
The current summary supports all-row int4 aggregates. Serializable transactions
remain outside the custom scan contract.

An old repeatable-read snapshot can still use its older checkpoint and head
versions. A snapshot between source commit and checkpoint publication uses
opt-in bounded replay or the default heap scan, even after newer maintenance
completes. Pruning uses DELETE rather than
TRUNCATE, so existing snapshots retain visible event and checkpoint versions.

## Retention and limits

Prune first decodes the durable keyed state and validates every columnar value
against it. It advances the retained boundary only to that checkpoint's applied
cursor. Newer source events remain available for the next apply. Boundary update
and event deletion share one transaction. Rollback restores both.

A feed retains at most its configured budget of 1–4,096 events. Its monotonic
ordinal can exceed 4,096; it is bounded by signed 64-bit arithmetic. Apply followed
by prune frees logical capacity. A full feed rejects the source statement until
maintenance frees space or an administrator reseeds. No event is dropped to admit
a write.

Existing capture scope still applies: 16 sources and 100,000 rows per initial
base. Captured inserts may grow a source to 1,000,000 live rows. Replay uses a
32 MiB requested vector-buffer budget. Encoded input and output, PostgreSQL
memory, allocator overhead, old MVCC versions, indexes and WAL are additional.
These bounds do not enforce a disk or process-memory quota. Old snapshots can
hold dead versions until VACUUM can reclaim them.

Apply now rewrites only affected partitions plus a small manifest. It does not
rescan the source heap. Full validation still applies to no-op maintenance and
pruning. The [partition contract](chunked-checkpoints.md) defines routing, bounds,
digests and visibility. Automatic maintenance and adaptive subdivision remain open.

## Keyed partition format

Each keyed partition uses the existing version 1 codec. It has little-endian
integers and a 112-byte header:

| Offset | Bytes | Field |
|---|---:|---|
| 0 | 8 | `SYNTRACP` magic |
| 8 | 2 | Version 1 |
| 10 | 2 | Last-retry-digest flag |
| 12 | 4 | Row count |
| 16 | 32 | SHA-256 source binding |
| 48 | 8 | Capture epoch |
| 56 | 8 | Local partition revision |
| 64 | 32 | Last batch retry digest, zero when absent |
| 96 | 8 | Payload length |
| 104 | 4 | CRC32, excluding this field |
| 108 | 4 | Reserved zero bytes |

The payload has a NULL bitmap followed by sorted `(int4 key, int4 value)` pairs.
A set bitmap bit means NULL; its value slot must be zero. Duplicate keys, trailing
bytes, bitmap padding and unsupported flags fail validation. Checksums and size
bounds are checked before allocating decoded rows. Encode and decode poll for
cancellation. One million rows occupy 8,125,112 bytes, within the 8 MiB bytea cap.
The value segment keeps its existing independent format and checksum. A separate
5,200-byte manifest records the global event cursor and all partition references.

## Recovery and remaining work

Committed rows and progress survive a process crash together. An uncommitted
apply and prune roll back together; a later apply replays the committed source
suffix once. Logical dumps omit checkpoint partitions, manifests, base rows, events and heads.
Restored source triggers require explicit reseeding before writes. This catalog
layout is tested on fresh installs; upgrades from older development layouts need
a migration.

Verification covers empty state, NULLs, key moves, history gaps, corrupt bytes,
rollback, old snapshots, concurrent producers, permissions and no-op maintenance.
The [checkpoint stress harness](checkpoint-concurrency-stress.md) now exercises
concurrent capture, apply, prune and reads. It reports cycle latency, WAL and
retained storage together. Physical restore,
power-loss behavior, release upgrades, more SQL types and planner integration
remain open. These checks do not establish production readiness or superiority
over another database.

For one atomic administrative operation, use
`syntra_apply_prune_capture(regclass)`. It returns applied ordinal and deleted-row
count. The [contract](atomic-checkpoint-maintenance.md) covers call-local
validation reuse, post-publication checks, captured pruning cuts and rollback.
The separate functions remain available.
