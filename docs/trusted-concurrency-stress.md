# Trusted concurrency stress

```sh
bash scripts/trusted-stress.sh
bash scripts/trusted-stress.sh --trusted-stress-seconds 300 --rows 100000
```

The default run lasts 60 seconds with 10,000 source rows. It uses an isolated
PostgreSQL cluster and release builds. The duration can be 5 to 300 seconds;
zero disables stress. This source uses 4 to 100,000 rows, even when the ordinary
benchmark uses a larger data set. Rust worker threads live in the external runner.

Eight client sessions run at once:

- Two writers transfer values between disjoint account pairs. They also insert
  or delete NULL-valued rows. Every eleventh transaction rolls back. Periodic
  own-write reads must use the heap and match native SQL.
- Two read committed readers check all rows and a range. They use an ordinary
  role with source SELECT access and no private catalog grants.
- One read-only repeatable read reader checks three aggregates per transaction.
  Both all-row results must stay equal while other sessions commit.
- Two administrators compete to publish trusted segments.
- One worker cycles through source VACUUM/ANALYZE, REINDEX, a source DDL epoch
  change, and trusted-catalog VACUUM/ANALYZE. Every kind must finish successfully.

Each completed aggregate must match native SQL in the same statement snapshot.
Checks compare row count, non-NULL count, sum, min and max. All-row sums must also
preserve the initial transfer total. Read-only sessions must keep their XID
unassigned. After workers stop, publication must restore eligible reuse.

## Failure and resource reporting

Publication admission and maintenance lock conflicts use SQLSTATE `55P03`.
The report counts them as expected conflicts for those workers. Other SQL errors,
reader or writer lock timeouts, invariant failures and worker panics fail the run.
Each worker must make progress. Both publishers need a successful publication,
and both writers must exercise commit and rollback.

Worker failure stops peers. Queries have a 15-second statement timeout and a
250-millisecond lock timeout. Connections and idle transactions have timeouts too.
Workers finish their current bounded cycle after the deadline. The report records
drain time and startup skew. No compile or test jobs should overlap this run.

Reports contain per-worker cycle counts, service latency histograms, route/reason
counts, commits, rollbacks and expected conflicts. They also record allocated
source, counter and trusted-catalog bytes before and after stress and final VACUUM.
Those catalog totals include the fixed benchmark artifacts. WAL bytes use the
node's insert-LSN difference, including background work during the window.

This is saturated correctness stress. Writers have no offered arrival schedule.
Cycle latency includes the oracle queries and maintenance contention. The results
cannot establish comparative OLTP tail latency, peak RSS, a disk quota, or a
production release gate. Longer soak tests, physical recovery and upgrade checks
remain separate work.

## Next: bounded change replay

Repeated full publication remains explicit maintenance work. Replay needs a
separate visibility and retention contract before implementation:

- Define stable row identity and supported source types. Test primary-key changes,
  deletes, duplicate values, NULLs and rollback before adding more operators.
- Create the base snapshot and change-feed boundary under a tested lock protocol.
  Capture transactions already in flight; an LSN or counter alone is insufficient.
- Apply only changes visible to the reader and absent from its base. Keep heap
  fallback for old snapshots, unsupported DDL, gaps and exhausted retention.
- Bound queued changes, working memory and retained versions. Apply backpressure
  before a source write can outrun those budgets. Make rebuild and repair explicit.
- Publish bytes and progress atomically. Prove process-crash recovery, replay
  idempotence and logical restore before measuring fresh analytical throughput.
- Measure write latency, replay lag, WAL and storage cost with equal offered load.
  Retain native SQL as the baseline and fallback throughout those tests.

The current stress harness tests trusted reuse and invalidation. It does not add
change replay, automatic refresh or planner substitution.

The [first 60-second result](../benchmarks/trusted-stress-2026-10-27.md) records
both passing data checks and the WAL cost of the combined workload.
