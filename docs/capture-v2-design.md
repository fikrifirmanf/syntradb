# Capture v2 design: no hot head row

Status: **not pursued; a smaller change shipped in 0.2.0.** EXPLAIN on the real
system later measured the cost directly: each head-row lookup walked every
version written since the old snapshot began, 5 ms at window 5. 0.2.0 removes the
per-write update without changing ordering. Writers lock the head row and append
an insert-only mark, so ordinals stay gap-free and the integrity checks below are
not needed. In the same soak writer loss fell from 26.4% to 11.7%, and the head
lookup to 0.3–0.45 ms. After the pruner's mark delete was also bounded, the
20-minute soak passed with no missed writer arrivals. See [transactional capture](transactional-capture.md#base-boundary-and-write-contract)
and the [measured fixes](../benchmarks/checkpoint-soak-2026-10-27.md#fixes-measured-2026-10-28).

The sequence and snapshot-cut design below would also remove writer
serialization, which marks keep. Revisit it only if write scaling, rather than
held-snapshot cost, becomes the limit. Phase 0 below is kept as the record of the
model that failed to reproduce the slowdown.

## Problem

Every captured write runs the row trigger, which updates one `capture_heads` row
(`emitted`, `live_rows`) and inserts events whose `ordinal` it just reserved
(`capture.rs`). The pruner updates the same row (`pruned`). The events table also
has a foreign key to that row, so each event insert takes a key-share lock on it.

PostgreSQL normally copes with a hot row because page pruning removes dead
versions. A snapshot, prepared transaction or slot that stays open stops that. In
the [failed soak](../benchmarks/checkpoint-soak-2026-10-27.md) head lookups per
writer commit rose from 63 to 150, writers queued on the row, and throughput fell
about 70% until the snapshot ended.

The head row does three jobs, and each needs a different answer:

| Job today | What depends on it |
|---|---|
| Gap-free `ordinal` for each event | replay order, the `checkpoint_history_gap` check, "applied" cursor |
| `emitted - pruned <= max_events` | writers stop before the feed passes 4,096 events |
| `live_rows <= 1,000,000` | checkpoints that fit in memory |

Sharding the counters alone does not work: shard-local ordinals have no global
order, and the gap-free check cannot survive it.

## Idea

Stop using the counter for order. Use two things PostgreSQL already provides
without row versions.

1. **Order from a sequence.** Each event takes its id from `nextval` on a sequence
   created with `CACHE 1`. A sequence has no MVCC versions, so no chain grows.
   Cache 1 matters: with a larger cache, session A can hand out 2 after session B
   handed out 33, and ids stop respecting commit order.
2. **Cut from a snapshot.** The checkpoint records the snapshot it was built from
   instead of an ordinal. An event is applied if its `origin_xid` is visible in
   that snapshot. Pending events are the rest.

### Why order by sequence id is enough

Replay only needs one thing: events on the same key apply in the order they
committed. Two writers changing the same key cannot overlap. The second waits for
the first's row lock (or unique-index entry), so its trigger runs after the first
committed, and its id is larger. Events on different keys commute. A mis-ordering
would still be caught: replay already checks each event's `before` value against
state and fails as corrupt on a mismatch.

Aborted transactions burn ids and leave gaps. Crash recovery may skip up to 32
sequence values, so ids stay increasing but are not contiguous. The gap check
changes meaning, below.

### Why a snapshot cut is enough

Today `applied = N` means "events 1..N are in the checkpoint". That works only
because ordinals are gap-free and assigned inside one serialized update. With
sequence ids, a transaction that took id 7 can commit after one that took id 9, so
"ids up to 9" is not a stable prefix.

A snapshot is a stable cut. The apply transaction takes its statement snapshot `S`
and applies every event visible in `S`, sorted by id. Anything that commits later
is not visible in `S` and stays pending for the next apply. The extension already
stores such a snapshot as text for the seed (`base_snapshot`).

A reader with snapshot `R` can use checkpoint `S` when `R` is not older than `S`:
`R.xmax >= S.xmax` and no xid in `R`'s in-progress list is visible in `S`. It then
replays events visible in `R` and not in `S`, at most 256 as today. Otherwise it
falls back to the heap, as readers older than the checkpoint do now.

## Counters without the hot row

Only the two limits still need counters, and neither needs an ordering.

**Event capacity.** Replace the head counters with 16 rows in a new
`capture_slots(source, slot, emitted, pruned)` table. A writer updates the slot
chosen by `pg_backend_pid() % 16` and checks `emitted - pruned <= max_events / 16`
on that slot only. Total capacity can never pass `max_events`. Under skew a busy
slot fills first, so the error can come before the whole feed is full; the
headroom function reports per-slot room too. Each slot sees one sixteenth of the
writes, so its version chain is about 16 times shorter at the same snapshot age.
The pruner updates all 16 slots once per prune, not per write.

**Live rows.** Drop it from the write path. Writers no longer fail when the table
passes 1,000,000 rows. Apply and seed refuse to build a checkpoint above that, and
readers fall back to the heap. This weakens the writer-side bound and keeps the
checkpoint-side bound. Per-slot deltas can go negative, so no local check could
enforce the sum exactly without reading every slot.

The `capture_heads` row becomes configuration that is read, never written, after
seeding. The events table's foreign key moves to it from a row that no longer
changes, or is dropped and enforced by teardown code. Either way, event inserts
stop locking a hot row.

## Integrity checks

The contiguous-ordinal check goes away. Its replacement is stronger because
counter and event rows commit together:

- For snapshot `S`, the sum over slots of `emitted - pruned` must equal the number
  of event rows visible in `S`, and per slot must equal that slot's rows.
- Event ids must be unique and each event must carry the slot it was counted in.
- Replay keeps its `before`-value check, now also the proof that same-key order
  held.
- Pruning deletes only events visible in the checkpoint's snapshot, and still
  verifies the stored bytes under a fresh snapshot first.

## Format and compatibility

- New format version `2` per source, chosen when seeding. Existing sources stay
  on version 1 and keep working unchanged. The default stays 1 until the gates
  below pass, then flips for new seeds.
- Version 2 adds `capture_slots`, a sequence, `slot` on events and a table holding
  each checkpoint's snapshot text. The manifest stays exactly as it is: it is a
  fixed 5,200 bytes (80 plus 64 partitions of 80) with no spare header, and its
  `applied` field becomes unused for version 2. Version 1 tables are not altered.
- A source moves from 1 to 2 by `syntra_stop_capture` then a new seed. The
  captured table is the source of truth, so nothing is lost. A migration helper
  that does this under one lock is a small addition.
- The two trigger paths are separate functions chosen by the stored version, so
  the version 1 path is not touched.

## Costs and risks

- One sequence call per event batch. `nextval` takes a short buffer lock, so it is
  one global serialization point, but it has no row versions and no lock queue.
  It writes WAL once per 32 values. Measure it; do not assume it.
- Chains still grow on slot rows while a snapshot is held, 16 times slower. The
  [age policy](resource-bounds.md#snapshot-age-policy) still applies, with
  thresholds scaled by measurement.
- Chunk and manifest rows are updated by appliers and are also hot under a pinned
  snapshot. They are written once per apply, not once per write. The soak showed
  flat chunk fetches, but v2 must be rechecked.
- Correctness now rests on the same-key ordering argument and on snapshot
  visibility rules. Both need adversarial tests, not only the existing oracles.
- Serializable and repeatable-read writers behave as today: a conflicting writer
  gets a serialization error rather than a reordered event.
- Reserving `n` ids for one trigger call with `generate_series` relies on
  evaluation order inside one statement. A test must pin that, or use one sequence
  call per event.

## Rejected

- **Sharded ordinals with a merge:** loses global order; the merge would need the
  same commit-order argument plus a cut, so the sequence is simpler.
- **Logical decoding:** gives true commit order but needs `wal_level=logical`, a
  slot that holds WAL, and a consumer process. Revisit if sequence ordering fails
  testing.
- **Advisory lock to serialize writers:** a hot lock instead of a hot row. Same
  throughput ceiling, and it blocks writers behind a slow transaction.
- **Tuning vacuum or `fillfactor`:** cannot clean versions a live snapshot needs.

## Plan and gates

Phase 0, one day, no format change. Build a throwaway variant of the version 1
trigger that updates one of 16 slot rows and drops the foreign key lock, with
ordering and the oracle's gap check deliberately broken. Run the diagnostic
10-minute soak at 60 cycles per second with a retained snapshot and read only
writer commits per window and the per-table fetch counts. If writers stay near
3,200 commits per window, the diagnosis holds and the rest is justified. If not,
stop and re-diagnose; do not build version 2.

Phase 1. Version 2 write path and sequence ordering behind the seed option, with a
model test that interleaves commits, aborts and rollbacks against an oracle of
"apply everything committed, sorted by id".

Phase 2. Snapshot cuts for apply, prune and reader replay, the new integrity
checks, and corruption tests for each (duplicate ids, wrong slot, missing event,
slot counter drift, snapshot field tampering).

Phase 3. Existing stress and 240-second soak pass on version 2. Crash and
restore checks pass.

Production gate for flipping the default: the same 20-minute soak with a held
snapshot passes its 5% writer limit, no existing test is weakened, version 1
sources still pass their suites, and writer throughput with no held snapshot is
within 10% of version 1 on the paced stress workload.

## Still not covered by this change

Physical backup and restore, release upgrades, monitoring integration, wider SQL
types and background maintenance. Version 2 only removes the write-path hot spot.

## Phase 0 result

Model: source table of 10,000 rows, tracked-sources join, head row with
`emitted`, `pruned` and `live_rows`, events with the same foreign key to the head
row, a pruner that deletes events and updates `pruned`, and a repeatable-read
snapshot held for the whole run. Load: 55 writes and 23 prunes per second, six
clients, 300 seconds, `fsync` off. Script and log:
`benchmarks/results/capture-v2-phase0-model-2026-10-28.sh` and `.log`.

| Variant | Mean latency, first 30 s → last 30 s | Writes per second |
|---|---|---|
| A: head row with foreign key (today) | 2.30 → 2.95 ms | 55.2 |
| B: head row, foreign key dropped | 2.34 → 2.97 ms | 55.0 |
| C: 16 slot rows, sequence ids, no foreign key | 2.46 → 2.63 ms | 55.0 |

No variant lost throughput; the offered rate was met in all three. The baseline
did not reproduce the soak's collapse (about 70%) or anything close to it, and the
earlier single-counter model did not either. The gate in this document was that
writers must hold near their offered rate with the changes and collapse without
them. The "collapse without them" half failed, so the experiment cannot show that
slots help. Variant C's latency grew least (14% against 28%), which is a hint and
not a result at this size.

What the model leaves out, any of which could be the real cause: concurrent
readers on the checkpoint tables (read committed, repeatable read and keyed
monitor, roughly 30 reads per second each), the appliers' large chunk and manifest
rows with out-of-line storage, vacuum running every five seconds, five tables
sharing one snapshot horizon, and the real trigger's per-write catalog work.

The earlier evidence stands as observation only: head fetches per writer commit
rose from 63 to 150, and sessions queued on row locks from window 2 of the
diagnostic run. Rising head fetches could be a symptom of slower commits as easily
as the cause of them.

## Re-diagnosis

Remove actors from the real soak one at a time with the same held snapshot and
read writer commits per window: writers only; writers and pruner; writers and
appliers; writers with readers but no maintenance. The actor whose removal restores
throughput points at the cost. Then sample where the writers spend time with the
extension's own timing around the trigger's head update and event insert.
