# Transactional counter shards

Sixteen counter shards reduce the shared-row lock bottleneck under concurrent
writes. They also add a metadata lookup. Local runs show that tradeoff rather
than a general speed gain.

At 5,000 offered TPS, four clients ran two-second phases for three rounds:

| Transaction-only metric, median across rounds | Single counter | Sixteen shards |
|---|---:|---:|
| Request p50, ms | 0.064 | 0.077 |
| Request p99, ms | 0.127 | 0.134 |
| Scheduled-to-commit p99, ms | 0.282 | 0.284 |

The sharded path was slower here. Both transaction-only cases completed all
offered requests. Across all eighteen phases, the single-counter run missed one
arrival and the sharded run missed none. Affected comparisons remain incomplete.

At 20,000 offered TPS, eight clients used the same phase duration and rounds:

| Transaction-only metric, median across rounds | Single counter | Sixteen shards |
|---|---:|---:|
| Request p50, ms | 0.076 | 0.085 |
| Request p99, ms | 0.398 | 0.191 |
| Scheduled-to-commit p99, ms | 4.599 | 11.815 |
| Committed TPS inside the window | 19998.5 | 19998.0 |

Request p99 fell about 52%, while request p50 rose about 12%. Scheduled latency
did not improve. Native PostgreSQL also had scheduling spikes in the second run.
These measurements cannot assign every stall to counter contention or storage.

Across all high-load phases, the single counter completed 719,986 transactions
and missed fourteen arrivals. Shards completed 719,985 and missed fifteen. Neither
run had SQL errors or balance invariant failures. Counters matched every successful
enabled transaction. Percentiles include successful requests and drain; they are
not complete offered-load latency distributions. The runner withholds comparisons
when an arrival is missed.

Both runs used 100,000 source rows, release Rust, assertion-disabled PostgreSQL,
and one saturated analytical client in analytical phases. WAL zero filling was off;
fsync, synchronous commit and full-page writes stayed on. No wait observer or
compile job ran during timed phases. The phases rotate within each run, but the
counter variants ran in sequence. Host drift remains a limit. This is not a
maximum-throughput or mixed-latency acceptance result.

Raw reports are `results/single-counter-5000tps-2026-10-27.json`,
`results/sharded-counter-5000tps-2026-10-27.json`, and the matching `20000tps` files.
The high-load reports have source input archives under `results/source-variants/`.
Recomputing each source fingerprint from its archive matches both binaries.
The archive manifest also records a separate checksum for each compressed file.
The legacy variant
ran from an isolated source copy; it did not alter the working source tree.

To repeat the current sharded path:

```sh
bash scripts/benchmark-production.sh --mixed-seconds 2 --mixed-rounds 3 \
  --offered-rate 20000 --oltp-clients 8 --track-source true \
  --wal-init-zero false --repetitions 3
```

The tracker remains an optional correctness foundation. No live query selects a
projection from its counters yet. See the [tracking contract](../docs/source-tracking.md)
for epoch, vector, permissions and recovery rules.
