# Design: capture ordinals without a row lock held to commit

Status: proposal, not implemented. It changes what readers may assume about the
capture log, so it needs a decision before any code. Background: issue #11.

## Problem

Every captured transaction takes `FOR NO KEY UPDATE` on the source's single row in
`syntra.capture_heads` and keeps it until commit. That lock is what makes ordinals
gap-free and ordered by commit: no other writer to the same source can reserve an
ordinal until this one is visible.

Under a held snapshot the head row also grows a version chain (the pruner updates
it every maintenance cycle), so each lock acquisition gets slower with horizon age.
The measurements in issue #11 show the result at the latency cliff:

- EXPLAIN in an idle session: the head statement takes 0.08 to 0.17 ms as buffers
  go from 12 to 98 across horizon ages 6,000 to 82,000. No capture statement is
  anywhere near the seconds seen at the cliff.
- With 2 writers, captured runs show `Lock:transactionid` and `Lock:tuple` waiters
  (6 to 15 per sample at rounds 9 to 15). Plain runs show none.
- With 1 writer, captured latency stays within 1.5x of plain.
- Raising the prune interval from 200 ms to 1,000 ms shrinks the head table 4x but
  did not reliably close the gap (latency 3.3x, 1.8x, 1.9x, 5.0x across four runs),
  and 5,000 ms lets the 4,096-event log fill (issue #17).

[Commit-time batching](capture-batching-design.md) is already implemented, so the
lock is taken at the first deferred firing, which is at commit. The hold time is now
the flush, the commit WAL flush, and the chain walk. There is little left to cut
without changing what the lock protects.

## What the lock protects

1. Ordinals are gap-free, so a reader can tell a missing event from a late one.
2. Ordinals follow commit order, so the newest mark is a consistent position.
3. `live_rows` on the mark is the exact row count at that position.
4. The log bound (4,096 events) is checked against one authoritative counter.

Any alternative has to give each of these up or replace it.

## Options

### A. Accept and document

Keep the design. State that concurrent writers to one captured table queue on its
head row, and that queueing grows with horizon age. The absolute and relative load
gates stay open and the readiness doc says why. No format risk. It leaves the
`readiness.md` Load row open until someone shows the cliff is outside the target
workload.

### B. Allocate ordinals from a sequence; readers apply below a safe horizon

Writers stop locking the head row. Each source gets a sequence; the flush draws a
block of ordinals with `nextval` (not rolled back, so gaps appear on abort and are
normal). Events carry `origin_xid`, which they already do.

Readers apply events only up to a safe horizon: the lowest xid still running,
taken from a snapshot at read time (`pg_snapshot_xmin`). Anything below it has
either committed (visible) or aborted (gone), so a gap below the horizon is not a
loss. This is the usual outbox-reader rule.

Why per-key order still holds: two transactions that change the same source row
serialize on that row's lock. The second cannot change it until the first commits,
and the first draws its ordinals before it commits, so the second draws a larger
one. Order across different keys is not preserved, and the aggregates (count, sum,
min, max, group measures) do not need it.

Costs and open questions:

- **A long-running writer stalls the horizon.** Its xid holds the safe horizon
  back, so the applier cannot move past it and the log keeps growing until it
  commits. A held read-only snapshot with no xid does not (this is the control's
  case). The soak has no long writers; real workloads might. Needs a rule for the
  log bound when the horizon is stuck, and `syntra_capture_lag` must report it.
- **`live_rows` on the mark** cannot be read at a point any more. Either store a
  per-event row delta and sum it in the applier, or keep an approximate counter for
  the bound only. Both change the mark format.
- **The 4,096-event bound** needs a non-locking check, for example the sequence
  value against the floor. It may overshoot under concurrency, so it needs margin;
  this interacts with issue #17.
- **Format and upgrade.** New columns and a sequence per source. The extension
  version must change, and the downgrade check must show the old reader refuses or
  reads the new log correctly. Not a 0.12.x change.
- **Prune** deletes by ordinal below the applied position; gaps do not matter, but
  its `m.ordinal = $3` mark lookup changes.

### C. Several head rows per source

Spread writers over N head rows, each with its own gap-free ordinal stream, and
have readers merge the streams. Per-key order needs a key-to-shard rule (a hash of
the key), but a transaction touches many keys, so it locks several heads, in shard
order to avoid deadlock. This reduces queueing by about N for independent
transactions and keeps gap-free ordinals per stream, but it keeps the lock hold
time and adds a merge. It is more machinery than B for a partial gain, and
`live_rows` still needs a combined count. Listed to be rejected unless B fails.

## Recommendation

Do not write the format change yet. First run a **throwaway experiment** that
answers whether the lock is the cause and how much of the gap B could close:

1. On a scratch branch, make the flush draw ordinals from a sequence and skip the
   head-row lock. Leave correctness broken (no horizon, no live count); the control
   workload only checks sums.
2. Run the hold control with 2 clients, 5 or more runs, and compare the captured to
   plain ratio with the 200 ms and 1,000 ms runs already recorded.
3. If the worst latency ratio does not fall clearly (below 2x in most runs) then
   the lock is not the main cause or the gain is too small. Choose A and stop.
4. If it does, write the full B design (reader horizon rule, lag reporting, bound,
   format version, upgrade and downgrade) and review it before implementing.

## What must be proven before B ships

- Every aggregate matches native SQL under concurrent writers, aborts, savepoints,
  prepared transactions and a writer that stays open while others commit.
- A reader never applies an event above a still-running lower writer (planted bug:
  use the newest ordinal as the horizon; a test must fail).
- The applier makes progress when a long writer finishes, and reports the stall
  while it runs.
- The 4,096-event bound holds with 8 concurrent writers (issue #17 sizing).
- Upgrade, downgrade and crash recovery checks pass; the native-relative gate from
  issue #10 passes on the hold control.

## Experiment result (split tables)

The throwaway experiment ran in a different form than step 1. A sequence-ordinal
hack would break the applier's gap checks and make the drain unreliable, so the
branch `exp/head-chain-hypothesis` (c7fb364) gives each of the 2 clients its own
captured table and head row at the same total load. That removes writer-on-writer
queueing on the head row, which is what B would remove. Three runs, 20 rounds at
200 tps, 200 ms maintenance:

| Run | Worst latency vs plain | Rounds over 2x | Worst throughput ratio | Correctness |
|---|---|---|---|---|
| 41667564573 | 4.3x (round 7) | 2 | 0.84 | pass |
| 41667582245 | 4.6x (round 7) | 1 | 0.84 | pass |
| 41667599148 | 3.9x (round 8) | 1 | 0.84 | pass |

The latency check fails in all three, in the same 3.8x to 7.1x range as the five
single-table 200 ms runs. Step 3 applies: the head-row lock is not the main cause
of the gap, and B would not close it. Writers did queue on that lock (issue #11),
but removing the queue is not enough. Caveat: three runs, and the cliff position
moves with runner noise.

## Decision

Choose **A**: accept and document the limit, and keep the Load gate open with this
explanation. Do not write B's format change on this evidence. If the remaining
cause is pursued, profile the batched insert path first (`hold-explain.sh` still
mirrors the per-firing SQL, not the batched insert).
