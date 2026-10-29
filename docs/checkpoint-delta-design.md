# Checkpoint delta design (0.4.0)

Status: implemented in the 0.4.0 workspace, 2026-10-28, and measured once per check in step 4. Not released.

## Problem

An apply rewrites every partition a batch touches, in full. Random keys touch all
64, so at 1,000,000 rows each apply writes about 12 MB of new keyed state and
segments, however few rows changed. In a 600 s churn run that was about 0.65 MB of
WAL for a transaction changing about 52 rows, and the chunk table grew from 465 MB
to 3.9 GB against 12.4 MB live
([measurements](resource-bounds.md#chunk-table-growth-under-sustained-churn)).
Vacuum tuning slowed the growth but did not stop it.

## Goal

Make the bytes written per apply follow the number of changed rows, not the source
size, while keeping every existing guarantee: exact reads, heap fallback, MVCC
visibility through PostgreSQL row versions, crash and restore behaviour, before-image
checks, pruning only after full validation, and the existing limits.

Existing feeds must keep working after the upgrade without a reseed, and the
v1 golden fixtures stay byte-for-byte unchanged.

## Design

Each partition keeps its base, the keyed state and value segment it has today, plus
an optional **delta**: the net change to that partition since its base was written.

A delta entry is `(key, base, current)`:

- `base` is the key's value in the base: absent, NULL, or a value.
- `current` is its value now: absent, NULL, or a value.

Entries are sorted by key, one per key, and an entry whose `current` equals its
`base` is dropped. Storing `base` lets a reader adjust aggregates without decoding
the keyed state.

### Apply

For each touched partition, apply decodes the base keyed state, as it does now, to
check before images, and overlays the delta. It replays the new changes onto that
view and produces the new delta. Then:

- If the new delta is within its bound, apply writes only the delta. The base
  `state` and `segment` columns are not assigned, so PostgreSQL keeps their
  existing out-of-line TOAST data and writes only a small new tuple version.
- Otherwise apply **folds**: it writes a new base from the merged rows and an empty
  delta, as today.

The bound is a fixed rule so results are reproducible: fold when the delta would
hold more than `max(64, base_rows / 16)` entries or more than 64 KiB. With about
15,600 rows per partition at 1,000,000 rows, that is about 977 entries. A simple
cost model, with about 16 changes per partition per apply, gives roughly 0.6 KB
written per changed row instead of about 12 KB. This is an estimate, to be
replaced by measurements.

Prune validates every base and delta in full before deleting events, as it
validates every base now.

### Read

A reader sums each partition from its segment summary and its delta, without
decoding keyed state:

- `rows` and `non_nulls` change by the entries whose presence or NULL-ness changed.
- `sum` subtracts each non-NULL `base` value and adds each non-NULL `current`
  value. This is exact in the existing wide accumulator.
- `min` and `max` take the base extreme combined with the `current` values, unless
  a `base` value removed by the delta equals the base extreme. Then the reader
  rescans that partition's segment values, skipping one occurrence of each removed
  value, and combines the result with the `current` values. Values are a multiset
  here; which key held them does not matter.

Bounded tail replay starts from base plus delta instead of base alone.

### Storage

- `syntra.capture_chunks` gains `delta bytea NOT NULL`, holding an empty delta for
  every existing partition.
- New delta format `SYNTRACD`: magic, version, key and value widths, entry count,
  partition binding, base revision, CRC, then sorted entries with presence flags.
  It has an exact length for its entry count, so readers check the stored length
  before detoasting, as they do for states and segments.
- Manifest version 2 adds a 32-byte delta digest and the base revision to each
  partition reference. A partition with an empty delta has the empty-delta digest.
  Readers accept version 1, which means every delta is empty. Writers keep
  writing version 1 while every delta is empty, so a fresh or fully folded
  checkpoint is unchanged on disk.
- The local revision still advances once per affected apply. The base revision
  records which revision wrote the base, so a delta can only apply to its own base.

### Corruption and fallback

A delta that fails its CRC, digest, length, sorting, binding, base revision or
width check fails like a damaged state or segment does now: readers use the heap,
and maintenance rolls back without pruning. A delta entry whose `base` disagrees
with the keyed base is a corruption error in apply and prune.

## Upgrade

`syntradb--0.3.0--0.4.0.sql` adds the column with an empty delta, matching each
row's width layout, and relaxes the manifest length check to accept both
versions. No checkpoint is rewritten. The upgrade check, the scale check and the
upgrade-under-load check must pass from 0.1.0, 0.2.0 and 0.3.0.

## Order of work

0. Confirm that updating only `delta` leaves `state` and `segment` TOAST data
   unwritten. **Done 2026-10-28** on PostgreSQL 18.6, with 64 rows holding 12 MB
   of out-of-line data: updating only a small column wrote 16 KB of WAL, an
   upsert that did not assign the large column wrote 14 KB, and rewriting the
   large column wrote 26 MB. The TOAST table grew only in the last case.
1. Storage crate: delta format, golden fixtures from an independent Python
   generator, apply onto base plus delta, fold rule, summaries with the multiset
   rescan. Property tests: any sequence of applies with folding gives the same rows
   and summaries as full rewrites. **Done 2026-10-28.** `syntra_storage::delta` and manifest
   version 2. Fixtures come from `build-support/fixtures/gen_delta_fixtures.py`,
   and the v1 manifest fixture is unchanged. A randomized test runs 120 feeds of
   400 batches in three layouts against the full-rewrite kernel, with values past
   the int4 range in the 8-byte layouts. Three planted bugs each made it or another
   test fail. A stored "no delta" is SQL NULL, so the upgrade writes no bytes, and
   the base row count sits in the reference's spare four bytes.
2. PostgreSQL side: catalog column, manifest version 2, apply, readers, tail
   replay, prune validation, corruption tests that damage each delta field.
   **Done 2026-10-28.** `crates/syntra-pg/src/delta_tests.rs` checks that a small
   change leaves base bytes untouched, runs 40 rounds of churn on `int4` and
   `int8` feeds against PostgreSQL's aggregates with folds in between, and
   damages, drops and misplaces deltas. Readers that ignored removals, an apply
   that never folded and one that always folded each failed a test. Three
   existing tests read raw base segments and now read through the checkpoint.
3. Upgrade script and upgrade checks. **Done 2026-10-28.** The upgrade, scale and
   under-load checks pass from 0.3.0, and the upgrade check from 0.2.0 and 0.1.0
   ([upgrades](upgrades.md#030-to-040)).
4. Measurement: rerun the 600 s churn at 1,000,000 rows and the 20-minute soak,
   and compare WAL per transaction and chunk table size with the current numbers. **600 s churn done 2026-10-28**, one run: 63,251 transactions against
   28,453, at most 0.10 MB of WAL per transaction against 0.66 MB, and a chunk
   table of 83 to 198 MB against 465 MB to 3.9 GB
   ([measurements](resource-bounds.md#chunk-table-growth-under-sustained-churn)).
   The WAL figure includes the growth phase. At about 52 changed rows per
   transaction it is at most about 2 KB per changed row, above the 0.6 KB
   estimate. A 1,800 s run committed 211,297 transactions at
   0.09 MB of WAL each, and the table stayed between 92 and 236 MB with no rising
   trend. The 20-minute `int4` soak passed on 0.4.0, after its keyed monitor
   learned to merge deltas. Two runs each of 0.4.0 and a control built from the
   0.3.0 baseline, in back-to-back pairs, missed 1,148 and 27 writer arrivals
   (0.4.0) against 632 and 22 (control); the slow pair slowed in both trees
   ([soak](checkpoint-soak.md#040-and-a-control-2026-10-28)). That shows no
   latency cost from the deltas, without ruling out a small one.

## 0.7.0: one row per delta

Through 0.6.0 a delta lived in the `delta` column of its partition's chunk row,
and every apply that changed the partition updated that row. Under a held
snapshot each update left a version that the next apply walked, so applies
slowed as the hold aged and pruning stalled behind them. 0.7.0 keeps the base in
the chunk row, at its base revision, and writes each delta as a row of
`syntra.capture_chunk_deltas` at the manifest revision:

- apply inserts the new delta row and deletes the one it replaces at the
  physical position its read found, as a TID scan, so no statement walks the
  versions a snapshot keeps of other revisions;
- a fold rewrites the chunk row and deletes the partition's delta row;
- readers look for a delta row at the manifest revision. With one, the chunk
  row must hold the base at its base revision with no inline delta; without
  one, the chunk row must be at the manifest revision, as 0.6.0 wrote it;
- before `ALTER EXTENSION UPDATE` creates the table, every delta is inline.

The manifest records the same references, so no format changes.

## Not changed

Partition count and routing, the 1,000,000-row and 4,096-event limits, the public
SQL functions and their results, and the heap fallback rules.
