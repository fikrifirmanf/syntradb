# Partitioned replay checkpoints

Checkpoint apply now rewrites affected hash partitions instead of replacing the
whole source's keyed and value blobs. A 5,200-byte manifest binds all 64 partitions
to one source epoch, row count, generation and applied event cursor. The public
SQL functions keep their existing signatures.

```sql
SELECT syntra_apply_capture('public.accounts');
SELECT syntra_prune_capture('public.accounts');
SELECT rows, non_nulls, sum, min, max, route, reason
FROM syntra_checkpoint_int4_summary('public.accounts', 'balance');
```

## Publication and visibility

The first apply reconstructs the stored base and its visible captured prefix.
It partitions those resulting rows by key and writes all 64 partitions, including
empty ones. Each partition stores sorted keys, nullable values and a value segment.
Its local revision starts at zero. The manifest records the global event cursor.

Later applies still require every retained event ordinal to be present. They
route the unapplied changes to partitions, preserving each partition's event
order. A changed partition must pass its manifest digests, binding, local revision,
row count, key routing, before-image checks and full representation comparison.
Only those partitions are decoded, merged and encoded. A local revision advances
once per affected apply, independently of the global event count.

Every apply checks metadata for all 64 partitions. It rejects missing, duplicate,
foreign-epoch or mismatched-revision rows. Untouched payload bytes are inherited
through the prior manifest references. Delta apply does not reread their contents.
Unchanged apply and prune validate both payloads in every partition. This prevents
pruning against damaged inherited state. A damaged untouched partition can allow
delta apply to finish, but it cannot authorize pruning.

One bound bulk UPSERT writes the changed partitions. A second bound UPSERT writes
the manifest and cursor. Both use the caller's PostgreSQL transaction. Failure
between them rolls back all writes. Cross-partition key moves commit together.
The manifest's total row count must match the transactional capture head.

Partition rows use a stable `(source_relation, partition)` key. Updates create
new PostgreSQL MVCC versions. Old snapshots select their older manifest and the
matching older partition versions. No separate file deletion or generation
collector bypasses PostgreSQL visibility. Pruning deletes capture events only;
VACUUM reclaims obsolete catalog versions when their snapshots permit it.

## Reader and pruning checks

A public read checks source permissions, current capture guards, source binding,
and the caller's visible head and manifest. It reuses partitions only when the
manifest's applied cursor equals that head's emitted cursor. A lagging manifest
can now use [bounded reader replay](reader-tail-replay.md) under the same snapshot
when the caller enables the three-argument overload. The two-argument call keeps
heap fallback on lag. Assigned-XID callers
continue to use the heap. Unusable history, replay limits and invalid metadata select the heap.

Reads validate each referenced columnar digest and segment checksum, then merge
all 64 summaries. They do not load unused keyed payloads. Counts, NULL aggregates,
minimum and maximum preserve the existing all-row int4 contract. An analytical
read does not assert that an unused keyed payload is healthy.

Prune validates both digests and decodes every keyed partition. It checks all key
routes, source bindings and revisions, and compares every stored columnar value
with its keyed value. Only then may it delete events through the durable applied
cursor. Newer events remain available. The retention update and deletion remain
one transaction.

Maintenance remains admin-only in writable READ COMMITTED transactions. The
existing nonblocking catalog admission serializes apply and prune. Captured
writers do not acquire that admission lock. Source locks, DDL rejection and heap
fallback rules stay in force.

## Routing and bounds

Version 1 routes an int4 key through the following frozen unsigned 32-bit mix:

```text
x = key as u32
x ^= x >> 16
x = wrapping_multiply(x, 0x85ebca6b)
x ^= x >> 13
x = wrapping_multiply(x, 0xc2b2ae35)
x ^= x >> 16
partition = x & 63
```

Sequential keys spread across partitions, as checked by an independent Python
fixture. Deliberately colliding keys remain supported. These are fixed hash
partitions, rather than a guarantee of small equal-sized chunks. A skewed source
can put many rows in one partition and make its update expensive. The existing
source limit remains 1,000,000 live rows; no tighter per-partition limit is added.
Adaptive subdivision remains future work.

Each keyed blob and columnar segment retains the existing 8 MiB cap. A replay
kernel retains its 32 MiB requested vector-buffer budget. Encoded buffers, manifest
references, PostgreSQL memory and old MVCC versions are additional. None of these
limits enforce a process-memory or disk quota. Long snapshots can retain many
old partition and manifest versions.

## Manifest format

All integers are little-endian. The fixed length is 5,200 bytes:

| Offset | Bytes | Field |
|---|---:|---|
| 0 | 8 | `SYNTRACM` magic |
| 8 | 2 | Version 1 |
| 10 | 2 | Partition count, 64 |
| 12 | 4 | Total row count |
| 16 | 32 | Source binding |
| 48 | 8 | Capture epoch |
| 56 | 8 | Applied global event ordinal |
| 64 | 8 | Manifest generation, starting at one |
| 72 | 4 | CRC32 excluding this field |
| 76 | 4 | Reserved zero bytes |
| 80 | 5,120 | 64 references, ordered by partition |

Each 80-byte reference contains an eight-byte local revision, a four-byte row
count, four reserved zero bytes, a 32-byte keyed digest and a 32-byte segment
digest. Local revisions must be smaller than the manifest generation. Row counts
must sum to the declared total and remain within the source cap. Unsupported
versions, extra bytes, nonzero reserved bytes and invalid progress fail decoding.

Each payload digest uses SHA-256 over `syntra-chunk-bytes-v1`, a zero byte, an
eight-byte payload length, and the payload. Hashing polls for cancellation every
64 KiB. Partition keyed bindings derive from `syntra-chunk-binding-v1`, a zero
byte, the source stream digest and the two-byte partition index. The existing
binding derivation then supplies its domain and length prefix; the capture epoch
remains part of the binding value. Digests detect accidental changes and bind
private catalog references; they do not replace catalog access control.

## Partition deltas (0.4.0)

A partition can carry a delta: the net change since its base state and segment
were written ([design](checkpoint-delta-design.md)). Apply writes only the delta
while it stays within `max(64, base rows / 16)` entries and 64 KiB, and leaves
`state` and `segment` unassigned, so PostgreSQL keeps their stored TOAST data.
Past that bound, or when the changes cancel out, apply writes a new base and
clears the delta. A partition whose changes net to nothing and that had no delta
is not written. A partition with no delta stores SQL NULL in `delta`.

Readers fetch a partition's delta with its segment and sum the base values minus
the delta's removed values, plus its current values. Apply, prune and tail replay
decode the base state and check that every delta entry's recorded base value
matches it.

### Delta format

All integers are little-endian. The 72-byte header holds the `SYNTRACD` magic,
version 1, key and value widths (one byte each), the entry count, the partition
binding (32-byte stream and 8-byte epoch), the base revision, a CRC32 over every
other byte, and four zero bytes. Each entry is the key, a tag byte, then the base
value and the current value at the value width. The tag's low two bits encode
the base and the next two the current value: 0 absent, 1 NULL, 2 a value. A value
that is absent or NULL is stored as zeros. Entries are sorted by key, one per key,
and an entry whose base equals its current value is rejected, as are an empty
delta, unknown tag bits and nonzero padding. The exact length follows from the
entry count, and readers check the stored length before detoasting.

### Manifest version 2

Version 2 has the same header and 64 references of 128 bytes. Each holds the
current revision and row count, the base row count (in version 1's reserved four
bytes), the state and segment digests, then the base revision, the delta entry
count, four zero bytes and the delta's digest. A reference without a delta must
repeat its revision and row count as its base and hold a zero digest. A delta's
base revision must be older than its partition's revision. Writers use version 1
whenever no partition has a delta, and a version 2 manifest without any delta is
rejected as noncanonical, so a checkpoint has exactly one encoding.

## Recovery and migration scope

Reseeding, capture retirement and source drop remove partition rows along with
their manifests. Explicit source-drop cleanup also runs in replica mode, where
ordinary foreign-key cascades can be skipped. Logical dumps omit all capture
state, including partitions. Restored capture needs explicit reseeding.

This layout is tested on fresh extension installs. Existing development layouts
need a migration or a fresh database; no upgrade migration is supplied. Physical
restore, replica failover, power loss and release upgrades remain open.

The new tests cover untouched tuple versions, cross-partition publication failure,
missing partitions, wrong revisions, corrupted inherited bytes, initial pending
changes and skewed keys. Existing snapshot, own-write, source ACL, crash and restore
checks now exercise the partitioned path. [Reader-tail replay](reader-tail-replay.md) now reconstructs eligible bounded
suffixes without publication. Automatic maintenance remains disabled.

Chunk loads now use exact source/epoch/partition/revision index probes under the
original snapshot. See the [lookup contract](catalog-lookup-probe.md) for required
schema, missing-index behavior, measurements and remaining metadata costs.
