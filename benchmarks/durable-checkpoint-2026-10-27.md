# Durable checkpoint findings, 27 October 2026

The checkpoint path passed 13 committed-session scenarios, process-crash retry
and omission of a populated checkpoint on logical restore. All summaries matched
native PostgreSQL across counts, sum, minimum and maximum at the same snapshot.
The build also passed 194 tests: 59 engine, 31 harness and 104 PostgreSQL tests.

## Local cost probe

This run used release Rust binaries and assertion-disabled PostgreSQL 18.6 on
macOS ARM64. It measured one warm 10,000-row source, 51 prepared requests per case,
and one excluded warmup. Cases ran sequentially without concurrent writers or
build jobs. Separate EXPLAIN calls measured backend WAL. Setup, prior cleanup,
initial checkpoint creation, aggregate verification and the outer commit record
are excluded from those operation costs.

| Operation | Median request, ms | p95 request, ms | Median backend WAL, bytes |
|---|---:|---:|---:|
| `native_summary` | 0.320625 | 0.329708 | 0 |
| `checkpoint_summary` | 0.064625 | 0.069459 | 0 |
| `apply_two_events` | 0.812417 | 0.887750 | 134,593 |
| `apply_unchanged` | 0.069500 | 0.076583 | 0 |
| `prune_two_events` | 0.157834 | 0.170250 | 179 |

Native SQL took about 4.96 times as long as the checkpoint summary in this
specific warm aggregate test. Both read paths and unchanged apply generated zero
backend WAL in every sample. Changed apply still rewrote the full keyed and
columnar blobs. Its roughly 135 KB of WAL for two events exposes the next storage
problem. Prune validates the checkpoint and deletes the applied prefix.

These measurements exclude automatic worker scheduling, mixed-load interference,
large sources, physical IO and amortized end-to-end maintenance. They do not
establish a ClickHouse comparison or general OLTP, OLAP or HTAP superiority.
A single-node process crash also does not simulate power loss or replica failover.

## Reproduce and inspect

```sh
bash scripts/benchmark-production.sh --mixed-seconds 0 --rows 10000 \
  --repetitions 51 --recovery-checks true --refresh-cost-checks true \
  --output benchmarks/results/durable-checkpoint-2026-10-27.json
```

- [Raw report](results/durable-checkpoint-2026-10-27.json)
- [Source archive](results/source-variants/durable-checkpoint-2026-10-27.tar.gz)
- [Source and report manifest](results/source-variants/durable-checkpoint-2026-10-27.manifest.json)
- [Checkpoint contract](../docs/durable-checkpoints.md)

Both binaries report source fingerprint
`a8368156989d844c4a17763d6ac555210d9d7f5c44feaf70f685c65c92911938`.
The archive contains 76 declared source inputs. An independent Python hash checked
the archived bytes against both embedded fingerprints. Markdown is excluded from
the build fingerprint.

## Correctness checks and next work

The committed scenarios cover old checkpoint snapshots, lag fallback, retained
event versions, uncommitted publication, rollback and no-op maintenance. They also
exercise concurrent source writers during initial checkpoint publication,
maintenance admission, source permissions, corrupt segments, reseeding, column
binding and DDL gaps. Each result is compared with native SQL.

Crash recovery retains committed checkpoint progress and rolls back an uncommitted
apply/prune transaction. The captured source change remains committed, and retry
applies it once. Logical restore drops the populated checkpoint and capture state;
restored source triggers require explicit reseeding.

Temporary implementation mutations made focused checks fail for CRC validation,
restored retry identity, event advancement and pruning past the applied prefix.
Removing the reader's applied-cursor comparison failed before timing began.
Original code was restored and the complete cost/recovery run passed afterward.

Next, stress capture, apply, prune and readers together while reporting OLTP tail
latency, feed occupancy, WAL and storage retention. Then replace whole-blob apply
with bounded chunk updates. Automatic maintenance and planner integration should
follow those correctness and cost checks. Production readiness remains open.
