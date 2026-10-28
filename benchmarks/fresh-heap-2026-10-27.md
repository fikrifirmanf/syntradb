# Fresh heap aggregate measurements

The fresh Rust path improves the tested range scans without building a segment.
Full-aggregate gains are smaller and vary between runs. All results below use
normal prepared requests, rather than EXPLAIN execution time.

Each case has 51 samples after warmup, with four routes rotated per repetition.
The source has 100,000 rows, including a NULL every seventeenth row. The native
and Rust paths read the same data. There is no index on the value column.

| Layout | Case | PostgreSQL median, ms | Rust median, ms | Ratio, PostgreSQL/Rust |
|---|---|---:|---:|---:|
| Ordered | All rows | 3.0500 | 2.9362 | 1.04 |
| Ordered | 10% range | 2.4913 | 1.0348 | 2.41 |
| Ordered | 0.1% range | 2.2375 | 0.7943 | 2.82 |
| Shuffled | All rows | 3.0716 | 3.0224 | 1.02 |
| Shuffled | 10% range | 3.1983 | 1.5344 | 2.08 |
| Shuffled | 0.1% range | 2.3922 | 1.2358 | 1.94 |

The range path uses native heap scan keys before values cross into Rust. It
still reads heap pages; these gains do not establish physical IO savings.
A native index plan, cold storage, wider rows, or another dataset may change
the result. Earlier passes showed larger full-aggregate gains, so the latest
2–4% full-scan difference needs longer measurement before making a firm claim.

## Mixed workload

The final run completed all 63,000 offered transfers across 21 phases. There
were no missed arrivals, SQL errors, or balance invariant failures. Tracking was
off. Four transaction clients offered 1,000 transfers per second. Each phase ran
for three seconds, with one saturated analytical client and three rotated rounds.
Both analytical paths used the statement snapshot and current heap data.

| Round | Native analytical QPS | Rust analytical QPS | Native OLTP scheduled p99, ms | Rust OLTP scheduled p99, ms |
|---|---:|---:|---:|---:|
| 1 | 332.67 | 357.00 | 1.052 | 1.011 |
| 2 | 323.67 | 359.33 | 1.090 | 1.011 |
| 3 | 330.67 | 354.33 | 1.026 | 1.070 |

An earlier mixed pass recorded a 2.42x scheduled-p99 ratio in one fresh Rust
phase. The tail-latency budget therefore still needs sustained runs. Saturated
analytical clients complete different query counts; this is equal worker capacity,
not equal offered analytical QPS. This microbenchmark does not prove a general
HTAP advantage or production readiness.

## Evidence and reproduction

The run used release Rust and assertion-disabled PostgreSQL 18.6. Fsync,
synchronous commit, full-page writes and WAL zero filling were on. No compile
job ran during timed phases. Client and server shared the same ARM64 host.

The report retains every prepared-request sample, separate EXPLAIN plans, SQL,
settings, and matched extension/runner source fingerprints. Every prepared result
is checked against native SQL. Native per-tuple EXPLAIN instrumentation can make
an opaque function scan appear faster, which is why the table uses ordinary
prepared-request times.

Raw report: `results/fresh-heap-verified-2026-10-27.json`. The source input archive
is `results/source-variants/fresh-heap-verified-2026-10-27.tar.gz`; its declared
source fingerprint matches both measured binaries. Earlier passes remain in the
`fresh-heap-fixed`, `fresh-heap-mixed`, `fresh-heap-keys-fixed`, and `fresh-heap-final`
reports from the same date.

```sh
bash scripts/benchmark-production.sh --repetitions 51 \
  --mixed-seconds 3 --mixed-rounds 3 --recovery-checks true \
  --output benchmarks/results/fresh-heap-verified-2026-10-27.json
```

The final run also passed snapshot, generic-plan source lock, read-only, process
crash, restore/rebind, and extension removal checks. The [API contract](../docs/fresh-heap-aggregates.md)
lists supported inputs and remaining restrictions.
