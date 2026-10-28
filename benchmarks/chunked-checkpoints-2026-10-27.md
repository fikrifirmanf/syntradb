# Partitioned checkpoint findings, 27 October 2026

A two-event checkpoint apply now rewrites affected hash partitions and a manifest.
On the local 10,000-row probe, median apply latency fell from 0.812 ms to 0.180 ms.
Median backend WAL fell from 134,593 to 9,443 bytes, about 93% less. Reads,
unchanged apply and pruning became slower because they check 64 partitions.

All 210 tests pass: 66 engine, 33 harness and 111 PostgreSQL tests. The production
build also passed deterministic visibility, process-crash and logical-restore
checks, plus a 60-second concurrent run. No production-readiness or database
ranking claim follows from these results.

## Isolated operation costs

Both cost reports use release Rust, assertion-disabled PostgreSQL 18.6, macOS
ARM64, 10,000 rows, 51 prepared requests and one excluded warmup per case. Setup,
source changes and prior cleanup stay outside each timing. Separate EXPLAIN calls
measure backend WAL and exclude the outer commit record. Builds and tests do not
run during timing. These are separate local runs, without confidence intervals.

| Operation | Previous median, ms | Partitioned median, ms | Previous WAL, bytes | Partitioned WAL, bytes |
|---|---:|---:|---:|---:|
| `native_summary` | 0.320625 | 0.314250 | 0 | 0 |
| `checkpoint_summary` | 0.064625 | 0.096042 | 0 | 0 |
| `apply_two_events` | 0.812417 | 0.180167 | 134,593 | 9,443 |
| `apply_unchanged` | 0.069500 | 0.169458 | 0 | 0 |
| `prune_two_events` | 0.157834 | 0.256416 | 179 | 363 |

Changed apply took about 4.5 times less time in this specific test. This gain
comes from updating two affected partitions rather than encoding the entire
source. A checkpoint summary remains faster than native SQL in this warm case,
but takes more time than the previous single-segment summary. Unchanged apply and
prune validate all keyed and columnar partitions; their added checks cost time.
Read and unchanged-apply samples generated zero backend WAL throughout.

The test does not include initial checkpoint construction, which writes all 64
partitions. Deliberately skewed keys can concentrate data in one partition and
increase its rewrite cost. The [partition contract](../docs/chunked-checkpoints.md)
describes those bounds and the unchanged heap fallback rule.

## Concurrent run

The 60-second worker phase passed 59,381 aggregate comparisons and 8,465
keyed-row reconstructions. Writers committed 7,911 transactions, emitting
47,466 events. Both appliers and the independent pruner made progress.
The pruner alone deleted 15,282 events. An old checkpoint snapshot remained valid for
the whole phase. Final catch-up left zero retained events and passed another
keyed reconstruction and checkpoint read.

Worker-phase node WAL was 160,294,496 bytes, versus
414,402,552 in the earlier full-blob run. Normalized by committed
writer transactions, that is about 20,262 versus 60,233 bytes per transaction.
These node intervals include readers, maintenance, intentional aborts and
background work. They do not attribute WAL to an individual writer.

Allocated checkpoint storage at worker stop was 74,629,120 bytes,
including 18,817,024 manifest bytes and 55,812,096 partition bytes.
The earlier run used 344,350,720 checkpoint bytes at that boundary.
After final catch-up and VACUUM, the new total was 70,746,112 bytes.
These are allocated catalog, TOAST and index bytes, rather than process memory.
Old snapshots still retain previous versions; partitioning does not impose a disk
quota or eliminate VACUUM requirements.

Ordinary readers used a checkpoint 2,278 times and lag fallback 56,823 times.
About 96.1% of their checks therefore scanned the heap. This remains the next
read-path problem. Writer cycle p99 values were 32.367 and 32.015 ms, including their
transactions and periodic native own-write checks. Other latencies and lock
rejections remain in the raw report.

The monitor now validates 64 keyed/columnar partitions and vacuums the additional
partition catalog. Its work differs from the earlier full-blob monitor. Both
runs are saturated, unpaced correctness tests. Their throughput and latency do
not establish a general comparative performance claim.

## Verification and reproduction

Seven new manifest tests check independent Python bytes and SHA-256 vectors,
key routing, limits, reserved fields, corruption and cancellation. Seven new
PostgreSQL tests check untouched tuples, cross-partition rollback when manifest
publication fails, missing partitions, wrong revisions, inherited damage, initial
pending changes and deliberate skew. CRC bypass and all-partitions-dirty mutations
made focused tests fail. The correct code was restored before the final runs.

The committed-session gates now read the partitioned layout. Crash recovery keeps
committed manifests and partitions together, discards uncommitted apply/prune, and
applies a surviving source suffix once. A populated partition catalog is omitted
from logical restore along with its manifest and capture history. Restored capture
requires explicit reseeding.

```sh
bash scripts/benchmark-production.sh --mixed-seconds 0 --rows 10000 \
  --repetitions 51 --recovery-checks true --refresh-cost-checks true \
  --output benchmarks/results/chunked-checkpoint-costs-2026-10-27.json
bash scripts/checkpoint-stress.sh --recovery-checks true \
  --output benchmarks/results/chunked-checkpoint-stress-60s-2026-10-27.json
```

- [New cost samples](results/chunked-checkpoint-costs-2026-10-27.json)
- [Previous cost samples](results/durable-checkpoint-2026-10-27.json)
- [New stress report](results/chunked-checkpoint-stress-60s-2026-10-27.json)
- [Previous stress report](results/checkpoint-stress-60s-2026-10-27.json)
- [Cost source archive](results/source-variants/chunked-checkpoint-costs-2026-10-27.tar.gz)
- [Cost manifest](results/source-variants/chunked-checkpoint-costs-2026-10-27.manifest.json)
- [Stress source archive](results/source-variants/chunked-checkpoint-stress-60s-2026-10-27.tar.gz)
- [Stress manifest](results/source-variants/chunked-checkpoint-stress-60s-2026-10-27.manifest.json)

Both new reports match source fingerprint
`83788aa3579c2d139679389b784b7fe44b5a1a05d0dca738b00ffd0f5504f58b`.
Independent Python hashing verified 86 declared source inputs in each archive
against both embedded binary fingerprints. Markdown is outside that fingerprint.

## Next work

Implement a bounded reader tail over affected partitions. It must use the source
query's snapshot, validate the manifest and event prefix, and apply before images
without changing persisted progress. Exact min/max after deletes must survive.
Keep heap fallback for gaps, unsupported cases and exhausted limits. Test old
snapshots, own writes, pruning and crashes before measuring the read benefit.

Also evaluate cheaper metadata decoding and scheduled maintenance with bounded
retry delays. No automatic worker or planner routing is enabled. Adaptive skew
handling, release upgrades, physical restore, power loss and failover remain open.
