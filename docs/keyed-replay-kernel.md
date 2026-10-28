# Bounded keyed replay kernel

The storage crate now has an in-memory replay kernel for int4 keys and nullable
int4 values. It applies a batch atomically and returns sorted rows for the next
publication stage. The immutable version 1 segment format stays unchanged.

This kernel does not capture PostgreSQL changes or decide snapshot visibility.
The PostgreSQL coordinator now supplies transactional capture and durable
checkpoints. Its explicit summary selects a snapshot-visible checkpoint or the
heap. Each in-memory state holds one version; PostgreSQL MVCC retains durable
generations for older readers. Automatic maintenance remains open.

## Input and progress contract

A seed has strictly increasing unique keys, an opaque stream ID, an epoch and a
sequence number. The coordinator must bind that stream to a validated source.
Epoch zero and invalid limits fail before state construction.

Each change carries a key, expected prior state and replacement state. An absent
row differs from a present NULL. Inserts require absence. Updates and deletes
require a matching prior value. A key move uses a delete and insert in one batch.
Duplicate values at different keys are valid.

The next batch must name the same stream and epoch. Its previous sequence must
match the current cursor. `apply()` advances by one batch; `apply_events()`
advances by the number of changes in a nonempty contiguous event range. Gaps,
overflow and stale batches fail. An exact retry of the most recent batch returns
`Duplicate` without changing state. SHA-256 binds its ordered contents and cursor.
A retry with different contents fails, even if it would produce the same rows.

Only that last retry digest is retained. Reseeding does not restore it. The
versioned checkpoint codec preserves rows, cursor, binding and retry digest
together. The PostgreSQL coordinator commits these bytes atomically; the codec
alone does not provide durable storage.

## Bounds and atomicity

Callers choose limits within the hard caps of 1,000,000 rows and 4,096 changes per
batch. A separate byte budget bounds requested owned vector buffers. Input slices,
allocator metadata, sorting stack and the caller's memory are outside that budget.
This is no RSS quota. Allocation failure returns an error.

The kernel sorts change indexes by key and original position. This preserves event
order for repeated keys. It checks prior values, then merges edits with the sorted
base. Final row capacity applies to the whole atomic batch, so a key move can work
at capacity. The merge avoids shifting the full base for each inserted row.

Rows, cursor and retry digest change only after all checks and a final cancellation
poll succeed. Failed validation, limits and cancellation leave the old state intact.
Polls cover input hashing, change groups and long merges, including deleted rows.
The index sort is bounded and is followed by a cancellation poll.

## Verification and integration work

Ten tests cover NULLs, integer endpoints, duplicate values, key moves, conflicting
prior values, retries, gaps, epochs, capacity, byte budgets and cancellation.
A fixed-seed reference model checks 200 batches against an independent ordered map.
Temporary mutations to prior-value checks, retry identity, byte limits and merge
cancellation all made the corresponding tests fail.

The [transactional capture layer](transactional-capture.md) supplies logged base
rows and events. The [durable checkpoint contract](durable-checkpoints.md) defines
its reader cut, atomic publication, retention and recovery rules. Ten additional
codec and event-range tests cover independent format bytes, damage, cancellation,
maximum row counts and restored retry identity. Temporary checksum, digest and
ordinal mutations each failed their focused tests.
