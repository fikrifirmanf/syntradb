# Reproducible SQL baseline

For a manually triggered Linux checkpoint soak, see the
[GitHub Actions instructions](github-actions.md).

For the assertion-disabled build and concurrent transfer workload, use the
[optimized and mixed workload harness](production-and-mixed.md).

Run the development setup first, then start the benchmark from the project root:

```sh
bash scripts/benchmark.sh --rows 100000 --repetitions 7
```

The script builds and installs the release extension into the project-local
PostgreSQL installation. It then builds the release Rust runner. The runner
creates a disposable cluster with a private Unix socket and no TCP listener.
It never connects to an existing database. Shutdown removes the temporary cluster;
if shutdown fails, the runner preserves its files and reports the location.

The default report is `benchmarks/results/latest.json`. Override it with `--output`.
Generated reports are ignored by Git. Rows must be between 1 and 1,000,000;
repetitions must be between 3 and 101. The source-capture segment limit prevents
larger datasets in this first harness.

## Correctness gates

Before timing, independent sessions check repeatable-read snapshots, read-your-writes,
the next read-committed statement snapshot and atomic rollback of a projection
refresh. Each query case must also return identical aggregates on all three paths.
Any failure aborts the run before a report is written. Trusted live checks also
exercise committed reuse, invalidation, nested row triggers, source permissions
and old snapshots before timing starts.

Prepared request samples now include a fifth route: trusted live reuse. Each
sample verifies the aggregate and requires the `projection` route. All five
routes rotate after one warmup. EXPLAIN samples retain the four earlier routes.
Tracking setup and trusted publication costs appear separately per dataset.
Only the fixed analytical source is registered here; mixed workload sources keep
their existing phase-specific tracking policy.

## Compared paths

1. PostgreSQL in a database without the SyntraDB extension.
2. Native heap queries in a database with the Rust library and planner hook loaded.
3. Explicit scans of a fixed segment captured in the enabled database.

All paths use the same PostgreSQL binary, hardware and server settings. Two copies
of deterministic data live in separate databases in the disposable cluster. The
first layout inserts increasing integers. The second inserts the same rows in
`md5(id::text)` order. Every seventeenth value is NULL.

Each layout runs all-row aggregates and ranges spanning roughly 10% and 0.1% of
the value domain. Reports include the exact bounds, matched row counts and SQL.
The runner disables parallel query and JIT, and keeps filesystem durability on.

## What the report measures

Each route receives a warmup. Measured repetitions rotate the first route in a
fixed order. The report retains every server execution and planning time, root
buffer counters and the last `EXPLAIN (ANALYZE, BUFFERS)` plan for each route.
Root buffer counters already include children; the runner does not sum them twice.

Median and nearest-rank p95 describe those samples. With seven repetitions, p95
is the maximum sample; it does not establish a population tail latency. Execution
times exclude client round trips and planning. Planning times remain in the raw
samples, so execution-only comparisons cannot establish total planner-hook overhead.

The report also records load time, capture plus table-creation time, encoded bytes,
destination storage size, pruning counters, server version, settings, configure
flags and actual Rust build profiles. Projection scan time includes full segment
validation. Low `scan_bytes` does not imply low physical IO because validation
reads the whole encoded value.

The default script's PostgreSQL development build enables assertions and debug checks.
The JSON records those settings. Results from this build guide development; they
are not production performance claims. The default command measures one client
on fixed, warm data. The production script adds the mixed phases documented above.
Neither harness measures cold cache, data beyond RAM, ingestion lag, crash recovery
or ClickHouse.

The fixed-data report also retains repeated source capture request times. These
include the client round trip, heap read and encoding, but no destination write
or table creation. Each result checks the row count and encoded length. They run
after warm scans against the unchanged source. Use `scripts/recovery-checks.sh`
for separate process-crash and logical restore checks.

The current harness rotates four routes: native PostgreSQL, enabled native heap,
fixed projection, and fresh Rust heap aggregation. Each case also records normal
prepared-request times for all four routes, with correctness checked on every
request. These complement EXPLAIN timings, whose per-tuple instrumentation can
favor an opaque function scan. Mixed runs now include a seventh fresh Rust phase.
See [fresh heap results](fresh-heap-2026-10-27.md) for the measured scope.
