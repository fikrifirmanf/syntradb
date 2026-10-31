# Serializable fresh heap reads

Before 0.9.0, fresh heap summaries and integer grouping rejected serializable
transactions with SQLSTATE `0A000`. Removing that restriction requires proof
that PostgreSQL sees the read dependencies. Snapshot equality alone is
insufficient: two transactions could otherwise both commit a write-skew cycle.

## PostgreSQL scan contract

The pinned PostgreSQL 18.6 source supplies a useful starting point:

- `src/include/access/tableam.h`, `table_beginscan`, sets `SO_TYPE_SEQSCAN`.
- `src/backend/access/heap/heapam.c`, `heap_beginscan`, acquires a relation
  predicate lock for sequential scans. That lock covers insertions, including
  an empty table.
- The heap scan also calls `HeapCheckForSerializableConflictOut` while checking
  tuple visibility. A relation predicate lock alone does not replace those
  outgoing-conflict checks.

SyntraDB's `SourceScan` uses that table scan API. Its integer predicate path
also starts a sequential scan, with native scan keys. The source relation lock
and the caller's active MVCC snapshot remain necessary throughout the scan.

This supports investigating serializable execution for the fresh heap paths.
It does not establish serializable execution for checkpoint or projection reuse,
which can return values without scanning the source heap.

## Intended change

Split active-MVCC snapshot validation from the existing serializable rejection.
Fresh heap summaries and grouping can use the first helper after SSI tests
pass. Capture, checkpoint and live reuse paths keep the current restriction
until their dependencies have separate evidence. Do not remove the shared guard
for every caller.

The function continues to run on PostgreSQL's backend thread. Native scan setup,
tuple visibility checks, relation locks and cancellation remain in PostgreSQL.
Any serialization failure must surface as `40001`; the client retries the entire
transaction. No internal retry may take a newer snapshot partway through a read.

## Required verification

Use independent real PostgreSQL sessions and explicit transaction barriers:

1. Compare a fresh summary and grouped results with native SQL in serializable
   read-write and read-only transactions.
2. Check the relation `SIReadLock` during a read-write transaction, including an
   empty input and a range with no matching rows.
3. Run write skew with both readers using the custom path, then with either
   reader using native SQL. Exactly one transaction must abort and the final
   native invariant must hold.
4. Repeat with empty-table and range-insertion phantoms. An empty result must
   still contribute a read dependency.
5. Verify that read-only deferrable snapshots retain native behavior. PostgreSQL
   may avoid predicate locks after obtaining a safe snapshot; do not require a
   lock that its safe-snapshot optimization deliberately removes.
6. Keep rejection tests for capture and reused state. Re-run existing MVCC,
   permissions, DDL and cancellation checks.

First run the new acceptance checks against the current implementation. They
must fail with `0A000`. After implementing the split, all acceptance and conflict
checks must pass. Temporarily bypassing the native scan in a conflict case must
make the SSI regression fail before restoring the correct implementation.

The [0.9.0 implementation](serializable-heap-reads.md) follows this split and has
local and native Linux conflict and recovery evidence.
The source inspection alone is not the support evidence, and cached-state SSI
remains unimplemented.
