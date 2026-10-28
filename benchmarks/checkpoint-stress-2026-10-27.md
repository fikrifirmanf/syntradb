# Checkpoint stress findings, 27 October 2026

A 60-second run passed with eight concurrent workers and a retained old snapshot.
It processed 41,280 capture events, 50,895 worker aggregate comparisons
and 9,220 keyed-row reconstructions. No unexpected error or data mismatch occurred.
The old checkpoint stayed eligible across the full worker phase. Final catch-up
left zero retained events and passed another keyed and aggregate check.

The run also passed the separate process-crash and logical-restore checks. The
workspace passed 196 tests: 59 engine, 33 harness and 104 PostgreSQL tests.

## Workload and progress

Release Rust binaries ran against assertion-disabled PostgreSQL 18.6 on macOS
ARM64. The source had 10,000 rows, an int4 primary key and a nullable int4 value.
The [workload contract](../docs/checkpoint-concurrency-stress.md) describes each
actor, transaction schedule and native oracle. Workers ran without arrival pacing.
Builds and tests finished before timing began.

Both writers committed 3,440 transactions and intentionally rolled back 344.
Each commit emitted six events. The independent pruner deleted 11,826 events;
the appliers deleted the rest of the committed prefix. Sampled pending and
retained head intervals peaked at 72 events, within the 4,096-event budget.
The terminal sample had 36 pending events, all removed by final catch-up.

| Worker | Completed commits | Intentional rollbacks | Expected rejections | Cycle p99, ms |
|---|---:|---:|---:|---:|
| `writer_0` | 3,440 | 344 | 0 | 37.311 |
| `writer_1` | 3,440 | 344 | 0 | 37.311 |
| `read_committed` | 25,478 | 0 | 0 | 4.311 |
| `repeatable_read` | 2,797 | 0 | 0 | 36.543 |
| `applier_0` | 1,625 | 109 | 242,971 | 31.087 |
| `applier_1` | 1,664 | 99 | 237,737 | 34.655 |
| `pruner` | 3,262 | 0 | 328,923 | 24.879 |
| `keyed_monitor` | 9,219 | 0 | 1 | 56.639 |

Cycle timings include the workload's native checks and transaction commands.
Applier cycles may publish only, or publish and prune. Monitor cycles can include
two VACUUM commands. These timings cannot establish isolated OLTP latency or a
database speed ranking. Accepted lock rejections count separately from completed
cycles. One monitor cycle completed its keyed check before VACUUM hit a lock
timeout; that check still contributes to the keyed-check count.

## Costs exposed by the run

The worker phase generated 414,402,552 bytes of node WAL, about
414.4 MB. This includes commits, intentional aborts, VACUUM and background
work. Setup, final catch-up and final VACUUM are excluded.

Allocated checkpoint catalog storage grew from 196,608 to
344,350,720 bytes while the old reader remained open. It was
344,473,600 bytes after catch-up and VACUUM. The old snapshot
preserved earlier versions during the run. VACUUM makes space reusable but did not
shrink these allocated checkpoint files. These sizes include TOAST and indexes;
they are storage measurements, rather than process memory measurements.

Ordinary readers used checkpoints in 3,586 checks and lag fallback in
47,065, so 92.92% of their checks scanned the heap. Writers separately verified
own-write heap routing. The earlier warm checkpoint speedup therefore does not
represent this continuously changing workload. Correct fallback remains essential.

Competing maintenance workers hit 809,631 nonblocking lock rejections.
This deliberately saturated retry schedule is expensive. A future worker needs
bounded retry delays and per-source scheduling. No automatic worker is enabled.

## Verification and source inputs

Two CLI tests verify that this phase stays disabled by default and accepts only
bounded durations. Mutating its default or limits made the corresponding tests
fail. A temporary source-specific bypass of checkpoint cursor matching passed
earlier deterministic gates, then failed inside the concurrent read oracle.
Inverting the keyed-row assertion also failed its monitor. All changes were
restored before the final run.

An earlier attempt stopped on a coordinator lock timeout. Setup and terminal
measurements now use a separate five-second lock timeout with phase diagnostics.
Workers retain their 250 ms lock timeout and 15-second statement timeout.
Worker errors, including unclassified SQL errors, still fail the run.

```sh
bash scripts/checkpoint-stress.sh --recovery-checks true \
  --output benchmarks/results/checkpoint-stress-60s-2026-10-27.json
```

- [Raw report](results/checkpoint-stress-60s-2026-10-27.json)
- [Matching source archive](results/source-variants/checkpoint-stress-60s-2026-10-27.tar.gz)
- [Source and report manifest](results/source-variants/checkpoint-stress-60s-2026-10-27.manifest.json)

Both binaries report source fingerprint
`5b29af16237d1dd2b33a15512c0bbd4ec0ac6ceb119e32fd1929ddfe3bdb5f08`.
An independent Python hash verified all 79 declared archived source inputs against
both fingerprints. PostgreSQL process crashes and logical restore passed in this
run. Physical restore, power loss, failover and release upgrades remain untested.

## Next implementation

Replace whole-source blob updates with bounded chunk updates. Publish changed
chunks and their manifest cursor in one PostgreSQL transaction. Keep the epoch,
row count, checksums and applied boundary tied to that manifest. Key moves across
chunks must remain atomic, and old reader snapshots must retain their referenced
chunks. Test skew, NULL transitions, deletes, cancellation and crash rollback
before measuring WAL savings.

Then consider replaying a bounded pending suffix for readers. It must use the
same snapshot as the source, prove event continuity and preserve exact minimum
and maximum behavior after deletes. Keep heap fallback for gaps and unsupported
cases. Measure end-to-end cost before enabling that path. Chunked storage and
reader-tail replay are design work, not features delivered by this stress run.
