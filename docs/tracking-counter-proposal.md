# Proposal: stop updating counter rows on every write

Status: option A is implemented in 0.5.0; see
[upgrades](upgrades.md#040-to-050). The rest is kept as the record of the
decision.

## Problem

Statement tracking adds 1 to one of 16 rows in
`syntra.source_generation_shards` on every source statement
([source tracking](source-tracking.md)). Seeding capture installs tracking, so
every captured source pays this cost. A held snapshot keeps every version of
those rows, and each later update walks them. At 80,000 versions per row, a
captured single-row update took 6.3 ms against 0.4 ms with none, while a plain
table stayed under 0.1 ms
([findings](../benchmarks/held-snapshot-writers-2026-10-29.md)). The two-hour
soak's writers slowed past a horizon age near 89,000.

0.2.0 removed the same pattern from the feed head row: writers append a mark
and pruning folds the marks into the head.

## Who reads the counters

- Trusted live projections compare the full 16-entry vector and the epoch with
  the vector stored when the segment was written (`live.rs`). Equality is what
  allows reuse, so any committed statement must change what a reader sees.
- `syntra_source_tracking_status` returns the vector and its sum.
- Capture's checkpoint readers do not read the counters. They use the head row
  and marks.

## Options

**A. Captured sources skip the counter.** A captured source already records
every committed row change in its feed. Its version for reuse could be the feed
position, read as the head plus the newest mark, under the reader's snapshot.
Writers would stop updating the counter rows for captured sources.

- Cost: removes the hot-row write for captured sources only. Tracked sources
  without capture keep it.
- Risk: trusted reuse would need a second version kind. Statements that change
  no rows advance the counter today but would not move the feed. That is safe
  for reuse, because no row changed, but it changes what the status call
  reports.
- Format: no new tables. The stored projection version would need a field for
  the feed position.

**B. Append-only counter rows.** Each statement inserts one row instead of
updating one, and maintenance folds old rows into a base value, as marks are
folded into the head. A reader's version is the base plus the number of visible
appended rows. The largest appended ID alone would not work: a statement that
commits after a later one would leave the largest ID unchanged while still
changing the data.

- Cost: removes the hot-row write for every tracked source. Readers pay for the
  rows appended since the last fold.
- Risk: needs a folding job for sources without capture, which the maintenance
  worker does not cover today. A reader that counts rows under a held snapshot
  pays the same growth the writers pay today, moved to the reader.
- Format: a new catalog and a new vector meaning; stored projection versions
  would need to be rebuilt.

**C. More shards.** Spreading updates over more rows divides the versions per
row but does not stop their growth. The 16-entry vector is part of the stored
format.

## Recommendation

Option A, for captured sources first. It removes the measured cost where the
soak found it, adds no catalog, and leaves tracked sources without capture as
they are. Before any change, a decision is needed on whether the status call's
count may stop advancing for statements that change no rows on a captured
source.

## What a change must keep

- An upgrade from 0.4.0 that keeps existing feeds and checkpoints, and a
  downgrade that refuses while it would lose information, as 0.4.0 to 0.3.0
  does.
- Trusted reuse that never serves a segment after a committed change: the
  existing trusted live tests, plus a test that fails if a captured write does
  not change the reader's version.
- A held-snapshot soak past an age of 89,000 that holds writer speed, before
  any claim that the slowdown is fixed.
