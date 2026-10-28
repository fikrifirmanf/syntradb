# Bounded reader-tail replay

An opt-in summary can now answer an all-row int4 query while its checkpoint cursor
trails committed source writes. The reader reconstructs affected partitions in
memory using capture events visible to its source snapshot. It publishes no
checkpoint, reserves no event cursor and does not advance pruning.

```sql
SELECT rows, non_nulls, sum, min, max, route, reason
FROM syntra_checkpoint_int4_summary('public.accounts', 'balance', true);
```

The two-argument function keeps its existing checkpoint-or-heap behavior. The
three-argument overload enables replay only when `replay_tail` is true. Passing
false preserves lag fallback. This opt-in remains experimental because concurrent
reader cycle latency regressed in the retained-snapshot test.

The enabled overload can return these routes:

| Route | Meaning |
|---|---|
| `checkpoint / eligible` | The visible checkpoint already matches the visible head. |
| `replay / bounded_tail` | A bounded visible suffix was replayed in reader memory. |
| `heap / reason` | Eligibility, history, integrity or resource checks required a fresh heap scan. |

## Snapshot and replay contract

The public entry point checks the actual caller's schema USAGE and table SELECT
privileges. Column-only SELECT remains insufficient under the existing source
contract. Supported-source, int4 column, RLS and serializable restrictions remain
unchanged. An assigned caller XID selects the heap, preserving own-write behavior
inside an unfinished source command.

The capture head, manifest, partitions and events use the query's active MVCC
snapshot. Current source identity and guard configuration must still validate.
Manifest binding and epoch must match the source and visible capture head. Its
cursor must lie between that head's pruned boundary and the visible feed position,
the newest mark above the head. Numeric event
ordinals alone do not establish PostgreSQL visibility.

If the cursors match, the existing checkpoint scan runs. Otherwise, the reader
uses the `(epoch, ordinal)` primary-key index with an exclusive lower cursor and
inclusive visible-head upper cursor. It requires every ordinal in that suffix.
Already-applied history is covered by the durable manifest and is not reread.
An old snapshot can retain deleted prefixes without forcing a source-wide event
scan. Missing history within the required suffix never authorizes a partial result.

Admin apply also uses this index, while retaining its stricter validation of the
entire retained interval before selecting unapplied changes. Prune deletes only the interval after its
previous pruning cursor through its durable applied cursor. No new index or SQL
catalog layout is needed.

The suffix routes changes to the same 64 key partitions as publication. Each
affected partition must pass its digests, source binding, revision, key routes,
row count and keyed/columnar representation comparison. Replay preserves event
order for each key and requires exact before images. Absence differs from a
present NULL. Cross-partition key moves reconstruct both sides under one snapshot.

The reader rescans the resulting affected rows for all five aggregates. It merges
those results with validated columnar summaries from unaffected partitions. This
preserves exact minimum and maximum after deletes, including empty and all-NULL
results. It does not approximate extrema with arithmetic aggregate deltas.
The final row count must match the visible capture head.

A repeatable-read snapshot can continue to replay its older suffix after newer
apply and prune transactions commit. PostgreSQL MVCC retains the visible event,
manifest and partition versions. A fresh snapshot instead uses the newer checkpoint
or its newer tail. Replay does not require the maintenance admission lock.

## Limits and fallback

Reader replay uses fixed initial bounds:

- At most 256 unapplied events. A 257-event suffix selects the heap.
- At most 65,536 affected-row work units. This is the sum of original rows in
  affected partitions plus the number of suffix events. Counting every event as
  possible growth is conservative, even when updates or deletes reduce work.
- At most 8 MiB of retained encoded partition payloads. This includes affected
  keyed blobs and all columnar segments needed for the summary. The fixed manifest,
  bounded event vectors, decoded rows, PostgreSQL detoasting and allocator overhead
  are additional. This limit is not an RSS quota.

The global source and storage bounds still apply. Even a one-event tail can exceed
the row budget if its hash partition contains many colliding keys. Such a source
remains readable through the heap. A full capture feed still rejects new source
writes until an administrator applies and prunes; reader replay frees no capacity.

Limits return `checkpoint_tail_limit`, `checkpoint_tail_row_limit` or
`checkpoint_tail_byte_limit`. Bad before images return `checkpoint_tail_rejected`.
Missing history, corrupt bytes, unsupported bindings and invalid metadata keep
their corresponding fallback reasons. Each failure discards private replay state
and scans the source heap at the same snapshot. Cancellation and PostgreSQL errors
still propagate; they are not converted into a successful heap result.

## Writes, cancellation and cost

The replay path performs native catalog reads and Rust computation. It issues no
SPI mutation, encodes no replacement checkpoint and assigns no transaction ID.
Committed-session checks compare checkpoint and event tuple identifiers, revisions,
xmins and progress before and after replay. Those visible tuples remain unchanged.

This is a logical read-only contract. PostgreSQL can perform page maintenance
while reading heap-backed catalogs or the source. Such work may generate backend
WAL without a caller XID. EXPLAIN WAL therefore remains a measured counter rather
than a guaranteed zero. The cost probe includes native SQL controls after the
same 2-, 256- and 257-event setup schedules.

Cancellation checks cover event scans, payload hashing, keyed decoding, replay
merges and aggregate scans. Limits bound event grouping and sorting. The existing
requested vector-buffer budget applies to replay kernels. No private result is
returned until validation, reconstruction and final row-count checks finish.

A replay still reads all columnar segments and decodes affected keyed partitions.
It can cost more than a current checkpoint scan, and some tails may cost more
than a heap scan. The initial policy uses fixed bounds rather than a cost model.
Prepared-request timing excludes setup, while separate EXPLAIN probes retain WAL
and page activity. Neither probe establishes a cross-database performance ranking.

## Verification and remaining work

Eight PostgreSQL tests cover extrema, NULLs, empty results, repeated keys, history
gaps, bad before images, corruption, event boundaries, skew, byte-budget checks
and exclusion of already-applied history.
The test harness assigns an XID, so these tests call the shared snapshot engine
directly. Fourteen committed-session scenarios exercise the public entry points,
including read-only transactions, old snapshots after pruning, no assigned XID,
unchanged persisted tuples, permissions, default-off admission and conservative
fallback.

The concurrent harness explicitly opts in and counts replay separately from
checkpoint reuse and heap fallback. Its aggregate oracle and independent keyed
reconstruction remain in force. Process-crash and restore checks now verify that
a surviving committed suffix is readable before the next apply. No new durable
format is introduced. Fresh installs register the new overload; existing development
databases still need a migration or reinstall under the current extension policy.

The [measured findings](../benchmarks/reader-tail-2026-10-27.md) include warm wins
and a concurrent reader latency regression. Next, profile retained-version catalog
lookup, measure paced mixed workloads and calibrate admission against observed read
cost. Keep the fixed bounds and heap path until broader types, skew handling,
long soaks and failure schedules have evidence. Automatic maintenance, planner
routing, release upgrades, physical restore and failover remain open.

Chunk loads now use exact source/epoch/partition/revision index probes under the
original snapshot. See the [lookup contract](catalog-lookup-probe.md) for required
schema, missing-index behavior, measurements and remaining metadata costs.
