# Design: capture once per transaction

Status: proposal, not implemented. It needs review before any code.

## Problem

The row trigger is a deferred constraint trigger, so at `COMMIT` it fires once per
changed row. Each firing runs two SQL statements (head lock and join, then event and
mark insert), reads the newest mark, and reads the bindings. A soak-writer cycle
changes five rows, so one commit does this five times.

The [per-statement EXPLAIN](../benchmarks/hold-control-linux-2026-11-01.md) puts the
two SQL statements at about 0.7 ms per firing, about 3.5 ms per cycle, which matches
the 3.5 ms by which captured exceeded plain in the Linux control at 0.12.0. Reusing
kept plans removed about a third of that and moved the latency cliff right by about
12,000 of horizon age, without closing the gap to plain (1,880 ms against 64 ms at
about 75,000). The remaining cost is per firing, so the next step must reduce the
number of firings that do work.

## Proposal

Make the row callback cheap and move the work to one point per transaction.

1. **Lock and position once.** The first firing in a transaction takes the head-row
   lock, reads the floor and limit, and reads the newest mark, as now. It keeps
   `(source, epoch, emitted, live, limit)` in backend-local state tied to the
   transaction.
2. **Buffer, do not insert.** Every firing, including the first, appends its changes
   (key, before, after, command ID) to a backend-local buffer and advances the
   local `emitted` and `live`. It runs no SQL.
3. **Flush before commit.** A `XACT_EVENT_PRE_COMMIT` callback, which runs after all
   deferred triggers have fired, inserts every buffered event with one statement and
   appends one mark holding the last ordinal and live count. It then clears the
   state. The head lock is held from the first firing to commit, as now.
4. **Abort cleanup.** `XACT_EVENT_ABORT` clears the state. A `SUBXACT_EVENT_ABORT_SUB`
   callback drops buffer entries and local counters from the aborted
   subtransaction, so ordinals stay gap-free.

Expected effect: one head lock, one position read, one insert and one mark per
transaction, instead of five of each. By the EXPLAIN figures that is about 0.7 ms
plus the buffer work, against about 3.5 ms. This is an estimate until measured with
the control.

## What must not change

- **Events and marks are the same rows.** Ordinals follow commit order and are
  gap-free; readers and `capture_marks` need no format change. One mark per
  transaction instead of one per row is the only visible difference, and readers
  already take the newest mark as the position. Check that pruning and
  `syntra_capture_lag` assume nothing about marks per event.
- **Errors still surface at `COMMIT`.** A full log is detected at the first firing
  against the limit and again at the flush, and raises the same message.
- **Savepoints.** Deferred firings happen only at top-level commit, so a rolled-back
  subtransaction has already removed its queued events. Immediate firings (old
  0.5.0 triggers, `SET CONSTRAINTS ... IMMEDIATE`) can occur inside subtransactions
  and need the subtransaction callback above.
- **Prepared transactions.** `PREPARE TRANSACTION` must flush at `XACT_EVENT_PRE_PREPARE`
  or refuse. The current trigger writes events in the transaction, so prepared
  transactions work today and must keep working.
- **Command IDs.** Each event keeps its own command ID, taken at firing time, so the
  diagnostic field is unchanged.

## Risks

- A bug in abort or subtransaction cleanup would leave stale buffered events, or an
  ordinal gap, which readers treat as a history gap. This is the main risk.
- Running SQL inside a pre-commit callback must be allowed after deferred triggers
  and must not leave the transaction in a state where a flush error cannot be
  reported. Verify with a full-log test at flush.
- Backend-local state must be keyed by transaction so a stale buffer cannot leak
  into the next transaction after an unexpected error path.

## Tests to write first

1. Many rows in one transaction produce the same events as today (compare event
   rows before and after the change on a randomized workload).
2. Rollback of the transaction and of a savepoint leave no events and no ordinal gap.
3. Immediate mode inside a subtransaction that aborts, then continues and commits.
4. A full log at the first firing and at the flush raises the existing message.
5. Two sources written in one transaction.
6. `PREPARE TRANSACTION` and `COMMIT PREPARED`.
7. Planted bugs: skip the subtransaction cleanup, flush twice, flush before the last
   firing. Each must fail a test.
8. The existing 274 PostgreSQL tests, the upgrade and downgrade checks, and the Linux
   control rerun against the 0.12.0 and kept-plans results.

## Decision needed

Whether to accept the added commit-time machinery for a throughput gain that is
estimated, not measured. A cheaper step to try first is a prototype of steps 1 to 3
only for the deferred, top-level case, behind a check that falls back to the current
per-firing path in every other case, so the unusual modes keep today's behaviour.
