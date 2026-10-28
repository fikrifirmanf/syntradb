# Optimized PostgreSQL and mixed workloads

Run the existing development setup once, then use the separate baseline script:

```sh
bash scripts/benchmark-production.sh --rows 100000 --repetitions 7
```

The script builds PostgreSQL 18.6 with `-O2`, debug and assertion checks disabled.
It verifies the pinned source archive's SHA-256 checksum. This installation lives
under `.tools/postgresql-production/` and leaves the development server unchanged.
The extension and runner use release Rust builds in `target/production/`.

The runner requires assertions off and `fsync`, `synchronous_commit` and
`full_page_writes` on. It rejects an incorrect configuration before timing.
These checks establish a production-style build and durability settings. They
do not qualify the database for production use or tune it for particular hardware.
JIT and parallel query remain disabled for the compared paths.

The benchmark creates its own disposable cluster with a private Unix socket.
It runs the existing fixed-data baseline, then the concurrent workload. Reports
go to `benchmarks/results/production-latest.json`. Every report contains the
actual build profiles, server configuration, query text and measurements.

## Transaction workload

Defaults are four transactional clients, one analytical client, 1,000 offered
transactions per second and three seconds per phase. Three rounds rotate phase
order. Each phase starts with a freshly loaded account table and its primary key.
All clients and the server share the same host.

Each prepared transaction moves one unit between a deterministic account pair in
one atomic two-row `UPDATE`. No retry hides an abort. Account count and total
balance must remain constant. Analytical reads and the final heap check verify
these invariants. Changed account values also prove that writes actually ran.
This narrow transfer workload is not TPC-C or CH-benCHmark.

Clients use an absolute arrival schedule, partitioned across workers. A slow worker
does not reduce the declared offered rate. Requests that cannot start before the
window ends are counted as missed arrivals. Started requests may drain afterward.
Throughput counts successful completions inside the window; the report also
records total successes and elapsed time including drain.

## Compared phases

| Phase | Analytics and freshness |
|---|---|
| PostgreSQL transaction-only | No extension in the database, no analytical client |
| Enabled transaction-only | Library and pass-through planner hook loaded in each worker |
| PostgreSQL fresh heap | Saturated analytical client reading statement snapshots |
| Enabled fresh heap | Same native heap query with SyntraDB loaded |
| Enabled frozen projection | Segment captured before writes; deliberately stale |
| Enabled fresh capture | Heap capture, encoding, validation and segment scan on every read |

The frozen phase measures contention from the current segment reader. Its copy
does not ingest writes, so it cannot demonstrate fresh HTAP performance. The
report labels it stale and records changed source accounts.

Fresh capture provides a statement-correct result by rebuilding on every read.
Its timing includes that work. It measures the current explicit API, not a future
incrementally maintained analytical engine. Ordinary table queries still use
PostgreSQL's heap paths.

Analytical clients run as fast as each query permits. Their offered query rate is
therefore not identical across implementations. Read the measured analytical QPS
alongside transaction latency. One analytical query per client warms the path
before timing; there is no timed transactional warmup.

## Latency and comparisons

`request_to_commit` measures a successful transaction from dispatch through commit.
`scheduled_to_commit` also includes client queueing and scheduling delay. This
prevents a slow dispatcher from hiding latency by measuring only requests it
managed to send. Percentiles use bounded HDR histograms with three significant
digits and microsecond units. Zero-duration readings use a one-microsecond bucket.

Errors are counted by SQLSTATE. Percentiles describe successes, so a phase has
`latency_complete = true` only when every offered request succeeds. The runner
withholds p99 ratios when either phase is incomplete or offered load differs.
Ratios remain separate by round; percentiles are not averaged across rounds.
Histogram bucket counts remain in the JSON for inspection.

The [dispatch diagnostics runner](dispatch-diagnostics.md) uses longer phases and
retains bounded traces pairing pre-dispatch lag with request duration for the same
request. Its histograms include every attempted transfer, including failures.

Reports include transaction p50/p95/p99, committed TPS, analytical QPS and latency,
missed arrivals, errors and cluster WAL bytes. WAL counts include background work
and drain. They are not a direct measure of per-row amplification. Transaction
statements time out after two seconds, analytical statements after five, and lock
waits after one second.

## Options and limits

```sh
bash scripts/benchmark-production.sh --rows 100000 \
  --mixed-seconds 5 --mixed-rounds 3 \
  --oltp-clients 4 --olap-clients 1 --offered-rate 1000
```

Set `--mixed-seconds 0` to run only the fixed-data baseline. Active phase duration
is bounded to 2–30 seconds, rounds to 1–5, transaction clients to 1–16 and analytical
clients to 1–4. Offered transaction rate is 10–100,000 per second. The segment cap
still limits the table to 1,000,000 rows. Mixed tests require at least two accounts.

These are warm, local microbenchmarks. The default three-second phases expose
integration problems and early contention; they do not establish stable production
tail latency. The harness does not test cold storage, data beyond RAM, sustained
compaction, incremental synchronization, replicas, recovery or ClickHouse.

The current harness also includes `enabled_fresh_rust_heap`. It reads the same
statement snapshot directly into the Rust aggregate kernel without a segment
build. Its comparison uses `postgresql_fresh_heap`, preserves the equal-load and
completeness checks, and does not establish a mixed-latency acceptance result.
