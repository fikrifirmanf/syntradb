# SyntraDB

SyntraDB is an experimental PostgreSQL-derived HTAP database written in Rust.
PostgreSQL supplies transactional SQL, heap storage and MVCC. The planned custom
columnar engine will accelerate analytical queries over the same logical data.

The current prototype has a PostgreSQL-independent integer aggregate kernel,
versioned single-column segments, group scans, SQL functions and a pass-through
planner hook. You can persist encoded segments in ordinary PostgreSQL `bytea`
columns and scan them explicitly. Source capture records snapshot provenance for
fixed projections. Named copies now have an owner-isolated catalog and atomic refresh. Opt-in capture and durable checkpoints synchronize supported int2, int4 and int8 sources, with int2, int4, int8 or `numeric(p,s)` (p ≤ 18) values; see [numeric columns](docs/numeric-design.md).

Fresh [time-window aggregates](docs/temporal-aggregates.md) filter `date`,
`timestamp` or `timestamptz` columns while summarizing integer or fixed-scale
decimal values. They use explicit SQL functions and the caller's MVCC snapshot.
Fresh [text and UUID grouping](docs/text-uuid-grouping.md) also supports integer
measures, with group-count and retained-key-byte limits.
Automatic analytical paths remain unimplemented. The [baseline harness](benchmarks/README.md) compares the prototype
with PostgreSQL on declared workloads.

Read the [design specification](syntradb_spec.md) for scope and correctness rules.

Fresh analytical calls can stream a heap column directly into the Rust kernel:

```sql
SELECT rows, non_nulls, sum, min, max
FROM syntra_heap_int4_summary('public.accounts', 'balance');
```

The [fresh heap contract](docs/fresh-heap-aggregates.md) covers ranges, snapshots,
permissions and limits. This path reads current data without rebuilding a segment.

The [trusted live API](docs/trusted-live-projections.md) can reuse explicitly
published segments after checking their snapshot and source binding. It falls
back to a fresh heap scan after writes or failed eligibility checks. Automatic
refresh and planner routing remain unfinished.

The [concurrency stress harness](docs/trusted-concurrency-stress.md) checks live
reads against native SQL while writers, publishers and maintenance run together.

The [refresh probe](docs/refresh-costs.md) separates tracking and publication WAL
costs. A [bounded keyed replay kernel](docs/keyed-replay-kernel.md) now applies
atomic int4 changes in memory. Opt-in [transactional capture](docs/transactional-capture.md)
now logs keyed changes with source writes. [Durable checkpoints](docs/durable-checkpoints.md)
apply those changes atomically, support explicit snapshot-safe summaries, and free
feed capacity through pruning. [Partitioned checkpoints](docs/chunked-checkpoints.md)
now update affected key partitions and an atomic manifest. [Bounded reader replay](docs/reader-tail-replay.md)
can summarize a lagging checkpoint without publishing it through an experimental
three-argument opt-in. Default checkpoint calls retain heap fallback on lag. The [checkpoint stress harness](docs/checkpoint-concurrency-stress.md)
checks concurrent capture, replay, pruning and retained snapshots. An opt-in
[background worker](docs/background-maintenance.md) applies and prunes the
captured sources of up to 8 listed databases, one worker each. A paced [checkpoint soak](docs/checkpoint-soak.md)
holds an old snapshot for 20 minutes. Its [first run failed](benchmarks/checkpoint-soak-2026-10-27.md)
on writer throughput. On 0.2.0 the same 20-minute run passed with no missed
writer arrivals, twice; both runs were on one laptop. The [resource bounds](docs/resource-bounds.md) page lists
what is and is not bounded, with a snapshot age policy.

## Build the standalone engine

The standalone engine includes a [bounded integer grouping kernel](docs/grouped-aggregates.md),
with PostgreSQL-generated aggregate fixtures and cancellation checks. Version 0.8.0
exposes it through an explicit [fresh SQL grouping API](docs/heap-grouped-aggregates.md).
Version 0.9.0 adds [serializable fresh heap reads](docs/serializable-heap-reads.md),
with conflict tests and whole-transaction retry semantics.

The toolchain file pins Rust 1.99.0. Cargo's default workspace members exclude the
PostgreSQL adapter so the engine can be developed without PostgreSQL installed.

```sh
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
```

## Build and test the PostgreSQL adapter

The development baseline is PostgreSQL 18.6 and pgrx/cargo-pgrx 0.19.3. This is the
currently tested baseline, not a commitment to support every PostgreSQL release.
SyntraDB is licensed under [GNU GPL version 3 only](LICENSE) (`GPL-3.0-only`).
Cargo packages are not published. Contributors must follow the
[code of conduct](CODE_OF_CONDUCT.md).

Install Python 3, a C/C++ compiler, make, pkg-config, ICU development headers, libclang,
curl, tar and shasum. PostgreSQL builds may also require bison, flex and readline
development headers on your platform. On macOS, setup finds Homebrew's ICU path.
On Linux, install the equivalent development packages through your package manager.

```sh
bash scripts/setup.sh
bash scripts/check.sh
```

Setup installs tools and a debug/assertion-enabled PostgreSQL build under `.tools/`
and `.pgrx/`. PostgreSQL source downloads use the pinned version and verify the
published SHA-256 checksum. The pgrx development port is 28818 and test port is
32218. The scripts do not start a system PostgreSQL service. Run builds and tests
as a regular user, since PostgreSQL refuses to run as root.

To open the development SQL shell:

```sh
bash scripts/pgrx.sh run pg18
```

```sql
SELECT syntra_version();
SELECT rows, non_nulls, sum, min, max
FROM syntra_int4_summary(ARRAY[1, NULL, -2, 8]::int4[]);
-- rows=4, non_nulls=3, sum=7, min=-2, max=8
```

`syntra_int4_summary` accepts PostgreSQL integer arrays and returns one summary.
An empty array returns zero counts and NULL sum/min/max. A NULL array produces
no rows. Multidimensional arrays are summarized in PostgreSQL element order.
The engine uses constant aggregate state and polls cancellation every 1024 rows;
PostgreSQL still owns and materializes the input array. This API tests integration
and SQL semantics. It is not a table scan or an analytical performance benchmark.

The hook counts relation-planning callbacks without changing candidate paths.
It chains an existing hook and stays installed for the backend's lifetime after
the module is loaded. `syntra_planner_calls()` exposes a per-backend counter;
it does not measure executed queries or cached-plan executions.

## Encode and scan a segment

The experimental format supports one nullable integer column, with up to
1,000,000 rows per segment. Groups contain at most 1024 rows. Checksums and full
metadata validation run before any scan result is returned.

```sql
CREATE TEMP TABLE analytical_values (name text PRIMARY KEY, segment bytea NOT NULL);
INSERT INTO analytical_values (name, segment)
SELECT 'integers', syntra_int4_segment(array_agg(n ORDER BY n))
FROM generate_series(0, 3071) AS n;

SELECT s.rows, s.sum, s.min, s.max, s.groups_scanned, s.groups_pruned,
       s.rows_examined, s.scan_bytes, s.validation_bytes
FROM analytical_values AS p,
LATERAL syntra_segment_range_summary(p.segment, 1100, 1200) AS s
WHERE p.name = 'integers';
-- rows=101, sum=116150, min=1100, max=1200
-- groups_scanned=1, groups_pruned=2, rows_examined=1024
```

`syntra_segment_summary(segment)` aggregates every row, including NULL rows in
the row count. `syntra_segment_range_summary(segment, lower, upper)` matches SQL
`BETWEEN` and excludes NULLs. NULL arguments produce no summary; the encoder
returns SQL NULL for a NULL input array. An inverted range matches nothing.

The saved segment represents the values supplied to the encoder. It stays fixed
when source tables change. Its SQL wrapper does not retain source identity or
snapshot provenance, and never substitutes it for a live-table query.

The scanner prunes groups after validating the whole segment. `validation_bytes`
therefore includes all encoded bytes; `scan_bytes` counts only selected groups.
These are logical counters, not measured disk IO savings. See the
[format contract](docs/segment-format.md) for layout, limits and validation rules.

Stop the project-local development server when finished:

```sh
bash scripts/pgrx.sh stop pg18
```

## Capture a source and run benchmarks

Use `syntra_capture_int4('table_name'::regclass, 'column_name')` to build a fixed
projection directly from a checked source. The [projection contract](docs/projection-lifecycle.md)
defines visibility, permissions and provenance. The [SQL example](examples/projection_lifecycle.sql)
shows atomic create, refresh and delete operations in caller-owned tables.

Run `bash scripts/benchmark.sh` for the first isolated SQL baseline. See its
[methodology and limits](benchmarks/README.md) before interpreting timings.

Run `bash scripts/benchmark-production.sh` for the assertion-disabled PostgreSQL
build and concurrent transaction/analytics measurements. Its [workload contract](benchmarks/production-and-mixed.md)
explains offered load, p99 reporting and the separate fresh and frozen-copy cases.

Run `bash scripts/benchmark-diagnostics.sh` for ten-second mixed phases and
[correlated dispatch traces](benchmarks/dispatch-diagnostics.md) around latency spikes.

Next, use the measured bottlenecks to choose storage and execution improvements.
Preserve normal heap execution until a projection manager can prove snapshot
eligibility. Mutable-table acceleration still needs version history,
synchronization and recovery rules.

Use the [managed projection API](docs/managed-projections.md) for named snapshots.
Run `bash scripts/recovery-checks.sh` for [crash and restore checks](docs/recovery.md).
Run `bash scripts/upgrade-check.sh [0.1.0|0.2.0|0.3.0|0.4.0|0.5.0|0.6.0|0.7.0|0.8.0]` to [upgrade older databases](docs/upgrades.md)
with captured history and compare them with a fresh install.
Run `bash scripts/downgrade-0.9.0-check.sh` to [downgrade 0.9.0 to 0.8.0](docs/upgrades.md#090-to-080-downgrade).
Run `bash scripts/downgrade-0.8.0-check.sh` to [downgrade to 0.7.0](docs/upgrades.md#080-to-070-downgrade).
Run `bash scripts/downgrade-0.7.0-check.sh` to [downgrade 0.7.0 to 0.6.0](docs/upgrades.md#070-to-060-downgrade),
`bash scripts/downgrade-0.6.0-check.sh` to [downgrade 0.6.0 to 0.5.0](docs/upgrades.md#060-to-050-downgrade),
`bash scripts/downgrade-0.5.0-check.sh` to [downgrade 0.5.0 to 0.4.0](docs/upgrades.md#050-to-040-downgrade)
and `bash scripts/downgrade-check.sh` to [downgrade 0.4.0 to 0.3.0](docs/upgrades.md#040-to-030-downgrade),
each updating again afterwards.
Run `bash scripts/physical-restore-check.sh [int2|int4|int8|numeric] [writers] [feeds]` for a base backup and
[point-in-time restore](docs/recovery.md#physical-backup-and-point-in-time-restore)
with captured history pending.
Run `bash scripts/upgrade-scale-check.sh` to time the upgrade on large feeds, and
`bash scripts/replica-check.sh [int2|int4|int8|numeric]` for [standby reads and promotion](docs/recovery.md#streaming-standby-and-failover).
Run `bash scripts/quorum-check.sh [int4|numeric]` for [quorum synchronous commit](docs/recovery.md#quorum-synchronous-commit) and failover.
Run `bash scripts/maintenance-check.sh` to exercise the opt-in
[maintenance worker](docs/background-maintenance.md), and
`bash scripts/maintenance-load-check.sh` to run it as the only applier under
paced pgbench load with an old snapshot held.
The [readiness gates](docs/readiness.md) track the work required before production.

The [server wait findings](benchmarks/server-wait-findings-2026-10-27.md) identify
WAL allocation and other storage waits in local latency spikes. The default
benchmark keeps WAL zero filling on; an explicit flag tests the alternative.

The [experimental source tracker](docs/source-tracking.md) now counts source
mutations in their transaction. It remains separate from fixed copies. Its
DDL guards now track source and trigger changes. Trusted live reads compare the
full visible counter vector and epoch before reuse. Checkpoint reads separately
validate their capture epoch and applied progress.

The [build fingerprint contract](docs/build-provenance.md) ties new benchmark
reports to matching extension and runner source inputs.

Run `bash scripts/catalog-probe.sh` to measure retained-version lookup work. The
[probe contract](docs/catalog-lookup-probe.md) and
[results](benchmarks/catalog-lookups-2026-10-27.md) cover exact revision probes,
fresh-read overhead and the remaining replay performance gates.

Use `bash scripts/checkpoint-paced.sh` for the
[paced checkpoint workload](docs/checkpoint-paced-load.md). It reports missed
arrivals and scheduled latency, and supports explicit replay on/off comparisons.

The [metadata traversal results](benchmarks/metadata-traversal-2026-10-27.md) show
reduced retained-history lookup work using existing indexes, with matched paced
trials and remaining historical-reader and overload costs.

The [historical-reader findings](benchmarks/historical-readers-2026-10-27.md) add
old-snapshot timing and separate maintenance costs. They also retain a rejected
snapshot-direction experiment and the corrected checkpoint cost attribution.

Use `syntra_apply_prune_capture(regclass)` for explicit
[atomic apply/prune maintenance](docs/atomic-checkpoint-maintenance.md). It returns
the applied ordinal and deleted-event count, with rollback and integrity checks.

The [atomic maintenance findings](benchmarks/atomic-maintenance-2026-10-27.md)
include repeated cost probes, mixed comparisons and a 60-second correctness run.
