# Optimized and mixed baseline: 2026-10-27

This run used PostgreSQL 18.6 built with `-O2`, assertions and debug checks off,
plus release Rust. `fsync`, synchronous commit and full-page writes were on.
The clients and server shared one macOS ARM64 host. Tables contained 100,000 rows.

[The archived JSON](results/production-baseline-2026-10-27.json) retains settings,
query text, samples, histograms and plans. Generated JSON stays local. The
[workload contract](production-and-mixed.md) describes pacing and freshness.

## Fixed-data scans

Median server execution time in milliseconds, seven samples per case. These
warm, explicit scans include segment validation and exclude client and planning
latency. They do not demonstrate automatic live-table acceleration.

| Layout | Query | PostgreSQL | Enabled heap | Segment |
|---|---|---:|---:|---:|
| ordered | All rows | 5.029 | 4.664 | 0.311 |
| ordered | 10% range | 2.347 | 2.291 | 0.143 |
| ordered | 0.1% range | 2.028 | 1.944 | 0.123 |
| shuffled_md5 | All rows | 4.702 | 4.672 | 0.342 |
| shuffled_md5 | 10% range | 3.209 | 2.732 | 0.435 |
| shuffled_md5 | 0.1% range | 2.383 | 2.362 | 0.438 |

## Concurrent workload

Each phase offered 1,000 transfers per second to four clients for three seconds.
Three rounds rotated six phases. Analytical phases used one saturated reader.
All 54,000 offered transfers succeeded, with no missed arrivals or SQL errors.
All analytical and final heap checks preserved account count and total balance.

The table retains each round's scheduled-to-commit p99 in milliseconds. Analytical
QPS ranges cover the three rounds. Frozen projections remain stale after writes.

| Phase | Round 1 p99 | Round 2 p99 | Round 3 p99 | Analytical QPS range |
|---|---:|---:|---:|---:|
| postgresql_oltp_only | 1.163 | 1.168 | 1.251 | 0.0–0.0 |
| enabled_oltp_only | 1.178 | 1.215 | 1.182 | 0.0–0.0 |
| postgresql_fresh_heap | 1.033 | 1.035 | 1.052 | 281.0–314.3 |
| enabled_fresh_heap | 8.147 | 1.039 | 1.036 | 307.7–316.7 |
| enabled_frozen_projection | 1.041 | 1.042 | 1.039 | 2733.3–2807.0 |
| enabled_fresh_capture | 1.035 | 5.735 | 1.037 | 272.3–299.0 |

Two phases showed scheduled-to-commit p99 spikes: 8.147 ms for enabled fresh heap
in round 1 and 5.735 ms for fresh capture in round 2. Their dispatch-to-commit p99
was 0.117 ms and 0.159 ms, respectively. The difference points to pre-dispatch
queueing or scheduling delay. These histograms do not identify the precise cause.

The mixed tail-latency target is not established by this run. Client scheduling
needs investigation before attributing these spikes to PostgreSQL or SyntraDB.
Short windows, a shared client/server host and saturated analytical loops limit
the result. No phase measured incremental projection maintenance or compaction.

The frozen-copy phase produced more queries per second, but it omitted write
propagation and returned older per-account values. It cannot support a fresh-HTAP
performance claim. Fresh capture rebuilt every read; its full cost was included.

## Next measurements

Separate client pacing from server execution in longer runs, record per-request
dispatch lag around outliers, and repeat at declared loads. Add resource budgets
only after profiling contention. Incremental synchronization, source eligibility
and recovery remain required before a fresh analytical optimizer path can ship.
