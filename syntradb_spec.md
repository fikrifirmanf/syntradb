# SyntraDB: Rust HTAP database specification

**Project name:** SyntraDB  
**Tagline:** One database. Every workload.  
**Status:** Phase 0 and a limited Phase 1 prototype in progress: source-aware fixed captures, immutable int4 segments, explicit scans and a reproducible SQL baseline harness implemented.
**Implementation language:** Rust for the extension and custom columnar engine; PostgreSQL remains the transactional foundation.  
**Intended audience:** Codex and project contributors.  
**Design direction:** PostgreSQL supplies transactional and analytical SQL. SyntraDB adds an original columnar OLAP engine. **Do not depend on ClickHouse**.
**Product identity:** SyntraDB is intended to become an independently branded database distribution, not merely a PostgreSQL extension.

## Product naming and client compatibility

- **Product name:** SyntraDB (working name; verify trademarks, domains and package namespaces before public release).
- **Tagline:** One database. Every workload.
- **Repository:** `syntradb`.
- **Future standalone server executable:** `syntradb` (proposed; applies if/when the project becomes a PostgreSQL fork or independent distribution).
- **Future CLI:** `syntrasql` (proposed). During the extension MVP, standard PostgreSQL commands and `psql` remain valid.
- **Extension MVP name:** `syntradb` or `syntradb_columnar`, subject to packaging conventions. The MVP runs inside PostgreSQL; it is not yet an independent server.
- **Client protocol:** Retain the PostgreSQL wire protocol initially. Client connections may use `postgresql://user:password@host:5432/database` (or another configured port) even when connecting to a future SyntraDB server. A `syntradb://` scheme would require driver/adapter support; changing the URI scheme is not required for an independent product.
- **Compatibility target:** PostgreSQL driver compatibility is a goal, not a guarantee of complete SQL/extension compatibility. Publish tested compatibility levels.
- **Attribution:** Preserve PostgreSQL copyright notices and comply with its license when distributing derivative code.

## 1. Vision

Build **SyntraDB**, a PostgreSQL-compatible **HTAP (Hybrid Transactional/Analytical Processing)** database that serves transactional and analytical queries from **one logical database and one SQL interface**. PostgreSQL supplies mature transactional infrastructure while a custom-built OLAP subsystem supplies column-oriented scans, compression, vectorized execution, and analytical query optimization.

The application should not have to explicitly route queries to an OLTP database versus an OLAP database.

### Non-goals for MVP
- Rewriting PostgreSQL transaction management from scratch.
- Integrating ClickHouse or embedding another complete OLAP engine.
- Building a distributed database before single-node correctness and performance are demonstrated.
- Claiming strict freshness without implementing snapshot-valid visibility.

## 2. Architecture decision

**Recommended strategy:** Begin as a PostgreSQL **extension (Option A)** to validate a working columnar representation and analytical executor. Advance to a **PostgreSQL fork (Option B)** if measurements or required internals show extension APIs to be limiting. A largely independent new engine (Option C) is not recommended at the outset.

| Approach | Advantages | Costs and risks | Decision |
|---|---|---|---|
| A — Extension | Retains PostgreSQL OLTP behavior, easier upgrades, fast iteration | Boundaries in executor/planner integration and transaction interfaces | **MVP** |
| B — Fork | More control over executor, planner, scheduling, storage and memory | High merge/upgrade/testing burden | **Long-term candidate** |
| C — Independent engine | Full architectural freedom | Rebuilds substantial reliability and database infrastructure | **Defer** |

A fork has **higher optimization freedom, not inherently higher measured performance**.

## 3. Conceptual system

```text
                         Applications / BI / SQL clients
                                      |
                          PostgreSQL wire protocol
                                      |
                       PostgreSQL parser + catalog
                                      |
                         Hybrid cost-based planner
                             /                \
               Transactional paths       Analytical paths
                       |                         |
              PostgreSQL row heap        Custom columnar engine
              Index / tuple access       Segment scan / vector ops
                       \                         /
                        Consistent snapshot semantics
                                      |
                      PostgreSQL WAL / MVCC / recovery
```

**Important architectural qualification:** The columnar copy must not be presumed to have PostgreSQL heap MVCC or WAL guarantees automatically. The integration must explicitly implement crash recovery, commit visibility, updates/deletes, and replica/columnar lag handling.

### 3.1 OLTP path
- Standard PostgreSQL heap is the **authoritative write store** in the MVP.
- Preserve PostgreSQL transaction, index, constraint, WAL and MVCC semantics.
- Avoid slowing ordinary point reads, single-row inserts, or small transactions.

### 3.2 Custom OLAP path
- Store analytical projections in immutable or mostly immutable **columnar segments**.
- Implement segment metadata, min/max statistics (zone maps), compression, pruning, batch/vectorized filtering, projection and aggregation.
- Add compaction and retention/garbage collection compatible with active snapshots.
- Treat advanced vectorized joins, sorting and mixed execution as subsequent work.

### 3.3 Query selection
- Use PostgreSQL planning hooks and/or custom paths/scans where appropriate.
- Estimate row vs column cost, accounting for cardinality, column width, selectivity, freshness overhead and operator support.
- Fall back to PostgreSQL when unsupported or when columnar state cannot satisfy the requested snapshot.
- Validate plans with `EXPLAIN (ANALYZE, BUFFERS)` and reproducible query sets.

### 3.4 Committed-change propagation
- Use a transactionally safe mechanism to publish committed inserts/updates/deletes to columnar storage. PostgreSQL logical decoding/WAL-related infrastructure is a possible starting point, but must be evaluated for same-instance design constraints and recovery semantics.
- Maintain a **progress/watermark** indicating which committed changes the columnar representation includes.
- For queries requiring the latest valid snapshot, **either** merge relevant delta versions with columnar segments **or** wait/rebuild/fallback to heap, without exposing partial transactions.
- Explicitly define behavior for `UPDATE`, `DELETE`, `TRUNCATE`, schema changes, restart/recovery, long-running snapshots, and aborted transactions.

## 4. Consistency contract (must be specified before implementation)

A good initial target is **snapshot-correct reads** at a PostgreSQL transaction snapshot, even if some analytical reads fall back to heap. Never claim that an asynchronously maintained columnar copy always contains every recent commit.

Example invariant:

```sql
BEGIN;
UPDATE accounts SET balance = balance - 100 WHERE id = 1;
UPDATE accounts SET balance = balance + 100 WHERE id = 2;
COMMIT;

SELECT SUM(balance) FROM accounts;
```

The analytical query must never expose **only one side** of a committed transfer. Tests must also verify isolation across concurrent transactions, deletes and updates, crash/restart, and long snapshots.

## 5. Implementation phases

### Phase 0 — Baseline and repository setup
- Write the snapshot eligibility, publication and recovery contracts in Sections 11–13 before freezing the segment format.
- Select and pin a supported PostgreSQL major version; document build, debug and test workflow.
- Build vanilla PostgreSQL baselines for transactional and analytical workloads.
- Establish CI, regression tests, crash tests and reproducible benchmark scripts.

**Exit:** reproducible builds, benchmarks and test infrastructure.

### Phase 1 — Experimental columnar reader
- Implement segment file format, type handling for a small type subset, zone maps and a scan interface.
- Build read-only data loading from a PostgreSQL table or exported snapshot. Initially expose a named, immutable experimental projection with explicit snapshot provenance. Do not substitute it for live-table reads without a proven eligibility check.
- Integrate a custom scan or explicit query path with correct fallback.

**Exit:** selected scans and aggregates match PostgreSQL at the same source snapshot; changed or unsupported live sources cannot silently use stale projections.

### Phase 2 — Vectorized analytical execution
- Implement batched predicate evaluation, projection and `COUNT`/`SUM`/`MIN`/`MAX`/`GROUP BY` for a carefully limited SQL subset.
- Add compression and segment pruning.
- Measure CPU, memory, query latency and speedup relative to PostgreSQL.

**Exit:** meaningful analytical improvement for benchmarked cases, without overstating unsupported operators.

### Phase 3 — MVCC-correct synchronization
- Ingest committed changes, track watermarks, apply updates/deletes, and support safe compaction.
- Define query freshness modes or transparent fallback/wait behavior.
- Test snapshot isolation, transaction boundaries and restart/recovery under failure injection.

**Exit:** no torn transactions or stale results where strict freshness is promised.

### Phase 4 — Hybrid optimizer and workload isolation
- Automatic path selection based on estimated cost and query capabilities.
- Resource budgeting for scans, maintenance and memory; protect OLTP p95/p99 latency.
- Investigate combined row/delta/column plans where justified.

**Exit:** favorable concurrent OLTP+OLAP results rather than just isolated OLAP wins.

### Phase 5 — Fork decision checkpoint
- Profile extension boundary costs and blocked planner/executor requirements.
- Fork **only if** targeted performance/correctness requirements cannot reasonably be met through extension APIs.
- If forking, document patch surface, upstream merge strategy and compatibility tests.

**Exit:** measured decision, not an assumed need for a fork.

### Phase 6 — Advanced features (future)
- Vectorized joins, parallel execution, advanced statistics, adaptive planning.
- Optional replication, distribution or disaggregated storage **after** single-node correctness, recovery and mixed-load gates pass.

## 6. Performance and success criteria

Do **not** use hypothetical speedups as achieved results. Benchmark against:
1. Vanilla PostgreSQL on identical hardware and data.
2. The prototype with OLAP disabled (OLTP overhead check).
3. The prototype with OLAP enabled under concurrent mixed load.
4. Optionally PostgreSQL + ClickHouse **as an external benchmark only**, not as an implementation dependency.

Measure:
- **OLTP:** TPS, p50/p95/p99 transaction latency, abort/conflict rate.
- **OLAP:** median/p95 query latency, scans/sec, bytes scanned, throughput, result correctness.
- **Mixed:** OLTP p99 degradation while OLAP runs, OLAP throughput under OLTP load.
- **System:** CPU, memory, IO, storage amplification, compaction cost, recovery time.
- **Freshness:** commit-to-query visibility latency / fallback rate / watermark lag.

Suggested suites: TPC-C-like transactional workloads, TPC-H-like analytical workloads, and **CH-benCHmark-style concurrent mixes**. Observe benchmark licensing and implementation rules when reporting formal results.

## 7. Major technical risks

1. **Snapshot consistency:** column segments and committed deltas must respect PostgreSQL visibility rules.
2. **Write amplification:** duplicate columnar data and maintenance can penalize OLTP.
3. **Executor impedance:** PostgreSQL's tuple-at-a-time conventions can constrain full vectorized pipelines in an extension.
4. **Planner accuracy:** wrong cost estimates can make hybrid query routing slower than heap execution.
5. **Resource contention:** background compaction and OLAP scans can increase OLTP tail latency.
6. **Schema/type compatibility:** PostgreSQL data types, collations, NULL semantics, extensions, DDL and indexes complicate correctness.
7. **Recovery and lifecycle:** crashes, restarts, vacuum, retained snapshots and upgrades must not corrupt derived state.
8. **Fork maintenance:** deep PostgreSQL patches create ongoing upstream reconciliation work.

## 8. Early repository layout (suggestion)

```text
syntradb/
  README.md
  docs/
    architecture.md
    consistency-model.md
    benchmark-plan.md
    design-decisions/
  extension/
    src/
    include/
    sql/
    expected/
  columnar/
    format/
    encoding/
    execution/
    compaction/
  tests/
    correctness/
    concurrency/
    recovery/
  benchmarks/
    oltp/
    olap/
    mixed/
  scripts/
```

Keep the columnar layer behind narrow, explicit interfaces so it can migrate into a fork with minimal rewriting.

## 9. Initial Codex instructions

> You are helping implement **SyntraDB**, a PostgreSQL-derived native HTAP database. PostgreSQL is the foundation for all SQL, catalog, transactions, MVCC, WAL, and OLTP. We are building our **own columnar OLAP storage and vectorized execution subsystem from scratch**; do not integrate ClickHouse or another complete analytical engine. Begin as a PostgreSQL extension and consider a fork only if tests show extension limitations. Prioritize transactional correctness, testability, incremental milestones, and performance measurements under concurrent OLTP and OLAP. Never assume that a derived columnar copy automatically inherits PostgreSQL transaction visibility or crash safety. Use Rust for new implementation code. Follow the concrete Phase 0/Phase 1 scope and interfaces below, and resolve the visibility contract before freezing storage metadata. Do not implement distributed functionality yet.

## 10. Open design decisions

- **Initial baseline:** PostgreSQL 18.6, Rust 1.99.0, and pgrx/cargo-pgrx 0.19.3. Pin these versions for the adapter spike; review patch updates deliberately.
- **Decided:** Rust for new extension and columnar code. Evaluate `pgrx` as the PostgreSQL integration layer in Phase 0; isolate any required FFI.
- **Initial decision:** explicitly enable projections per table. Automatic recommendations can follow measured storage and maintenance costs.
- **Initial decision:** heap fallback when snapshot eligibility cannot be proved. Delta merging follows a tested visibility model; waiting alone is insufficient.
- Which column types, expressions and SQL operators belong in the MVP?
- What are target datasets, write rates, query concurrency and hardware budgets?
- What licensing and upstream compatibility strategy will the project follow?


## 11. Rust implementation strategy

**Decision:** build new SyntraDB code in Rust. Rust does not by itself establish transaction correctness or outperform another engine.

- Evaluate `pgrx` for SQL bindings, extension packaging and access to PostgreSQL hooks. Pin the Rust toolchain, PostgreSQL major/minor, `pgrx`, and `cargo-pgrx` versions after a build-and-hook spike. Commit the dependency lockfile.
- Keep a standalone engine crate free of PostgreSQL pointers, `Datum` values, memory contexts and backend calls. Pass owned buffers or explicitly scoped borrowed data across the adapter boundary.
- Keep `unsafe` and PostgreSQL FFI inside small modules with documented lifetime, ownership, error and cancellation rules. Do not unwind across an unguarded foreign boundary.
- Start with PostgreSQL backend processes and supported worker facilities. Do not introduce Tokio or arbitrary worker threads into the backend execution path. Any later engine-only threads must own their inputs and must never call PostgreSQL internals or access its pointers.
- Make memory budgets, cancellation checks and temporary-file cleanup part of the execution interface. Account for Rust allocations as well as PostgreSQL memory; a separate Rust allocator is not automatically constrained by `work_mem`.
- Allow supporting crates for compression, checksums, buffers and testing after license review. Building an original engine does not require rewriting every codec. Do not embed another database or query engine.

The [`pgrx` project documentation](https://github.com/pgcentralfoundation/pgrx) describes its integration facilities and threading caveats. Verify the pinned release rather than assuming every PostgreSQL internal API has a safe wrapper.

### 11.1 Proposed Cargo workspace

```text
Cargo.toml
Cargo.lock
rust-toolchain.toml
crates/
  syntra-core/       # Types, batches, expressions, budgets, cancellation
  syntra-storage/    # Segment encoding, metadata, validation, publication
  syntra-exec/       # Scan, filter, projection, aggregate kernels
  syntra-pg/         # pgrx extension, hooks, catalogs, snapshot adapter
  syntra-bench/      # Engine benchmarks and reproducible data generation
```

This refines the suggested layout in Section 8. Keep `docs/`, integration `tests/`, workload `benchmarks/`, and `scripts/` alongside the workspace. Add a synchronization crate only when its contract is settled.

### 11.2 Proposed interfaces

These are design contracts, not implemented APIs:

| Interface | Responsibility |
|---|---|
| `SegmentWriter` / `SegmentReader` | Write or validate a versioned segment; reject invalid offsets, lengths and checksums |
| `Batch` / `Selection` | Typed column buffers, validity bits and selected row positions |
| `BatchSource::next_batch` | Pull bounded batches with memory accounting and cancellation |
| `AggregateState` | Update and finalize supported aggregates with PostgreSQL-compatible semantics |
| `ProjectionManifest` | Identify complete published generations, schema versions and source provenance |
| `SnapshotEligibility` | Return an eligible pinned generation or an explicit rejection reason |
| PostgreSQL adapter | Enforce permissions, build supported plans, manage snapshots and convert final results |

A batch layout may follow Arrow conventions where useful. The persistent segment format remains separately versioned. Batch sizes are benchmark parameters, not assumptions baked into SQL semantics.

## 12. Snapshot eligibility and the first useful scope

A progress LSN describes ingestion progress. It is **not a complete PostgreSQL visibility test**. A query can need older versions excluded by a newer projection, or its own uncommitted writes absent from committed-change ingestion.

For each accelerated read, establish all of the following:

1. Every source row version visible to the query is represented exactly once.
2. Versions invisible to that query are excluded, including later commits and aborted changes.
3. The query sees its own writes according to PostgreSQL command visibility, or executes on the heap.
4. The projection schema, supported expressions, permissions and isolation behavior match the source query.
5. The generation and any required delta history remain pinned until execution ends.

Default to heap execution when any condition is unproven. Decide before emitting results; restarting after partial output is not a valid fallback. Eligibility must be checked at execution with the actual snapshot, including for cached plans.

PostgreSQL documents statement snapshots, transaction snapshots and visibility of a transaction's own writes in its [transaction isolation reference](https://www.postgresql.org/docs/18/transaction-iso.html). These rules constrain the design; an LSN comparison cannot replace them.

### 12.1 Phase 1 boundary

- Build a named experimental projection from a consistent source snapshot. Record its provenance and expose that it represents a fixed dataset.
- Compare projection results against PostgreSQL using that same snapshot while it is available. Do not imply that the old snapshot can be restored later from a stored identifier.
- Keep normal live-table SQL on the heap initially. An explicit projection API is acceptable for this experimental phase and is not the final transparent-routing experience.
- Prototype transparent acceleration only for sealed tables or partitions once there is a proven write/DDL exclusion protocol and snapshot eligibility rule. A user promise that data is immutable is insufficient.
- Design how mutation, `TRUNCATE`, table rewrite, drop/recreate, extension removal and restart invalidate projections before enabling this path. Cover concurrent DDL and writes without a check/use race.
- Preserve authorization. For the first prototype, reject acceleration for row-level security, security-barrier contexts or other permission cases the adapter cannot preserve.
- Use heap paths for locking reads, modifying statements, serializable transactions and unsupported expressions until their required behavior is implemented and verified.

### 12.2 Mutable HTAP gate

Before Phase 3 implementation, choose how version identity and visibility survive compaction. Do not use a heap `ctid` as a permanent logical row identifier. An initial mutable-table scope may require an immutable primary key; key changes still need explicit delete/insert handling.

Specify base-snapshot/bootstrap alignment, transaction identities, insert/delete versions, subtransactions, prepared transactions, transaction-ID wraparound and retention limits. Unsupported cases must disable acceleration safely. Atomic ingestion of a transaction prevents torn publication but does not by itself prove snapshot correctness across tables or segments.

For a future base-plus-delta plan, merge only versions visible to the query. Retain old versions while eligible readers need them. If history is unavailable, use the heap rather than guessing. A bounded wait may reduce lag but cannot remove versions newer than a query's snapshot.

## 13. Storage publication, recovery and operational safety

Treat analytical storage as rebuildable derived state. Losing a projection may reduce performance; it must not lose authoritative heap data or produce incorrect results.

- Give segment files a format version, explicit byte order, schema fingerprint, row counts, per-column encodings, null counts, bounds and checksums. Validate lengths and arithmetic before allocating or indexing buffers.
- Publish complete generations through a documented durable protocol. Write and sync segment data before publishing a manifest that references it; include directory durability where required by the filesystem. Prove behavior for crashes at each step.
- Pin a manifest generation per reader. Compaction creates a new generation and reclaims old files only after all readers release them. Never overwrite a file still in use.
- Coordinate PostgreSQL catalog state, file publication and ingestion progress. There is no implicit atomic transaction spanning ordinary external files and PostgreSQL catalogs. Specify ordering, reconciliation and orphan cleanup.
- Recover conservatively after checksum failures, missing files, incompatible schema or interrupted publication. Disable the affected projection and rebuild it from a valid source boundary. Never silently drop damaged segments from a result.
- Make replay idempotent. Persist enough transaction/change identity to handle duplicates and advance ingestion acknowledgment only after the corresponding derived state is durable.
- Bound retained WAL, delta history, compaction IO and temporary storage. If retention is exhausted, invalidate and rebuild the projection through a documented procedure. Do not let a stalled analytical consumer fill the primary's disk.
- Document backup/restore and replica behavior. Initially exclude projection files from authoritative backup requirements and rebuild them after restore; disable acceleration on replicas until explicitly supported. Prevent restored catalogs from accepting unrelated old files.

PostgreSQL's [logical decoding documentation](https://www.postgresql.org/docs/18/logicaldecoding-explanation.html) describes exported bootstrap snapshots, retained resources and possible duplicate delivery after crashes. Evaluate same-instance consumption in a separate spike, including connection/worker lifecycle and feedback behavior, before adopting it.

## 14. Performance strategy and competitive target

**Ambition:** outperform established analytical engines on selected, published workloads while preserving PostgreSQL transaction behavior. Beating ClickHouse across workloads is an unproven research goal, not a release claim.

The initial product hypothesis is fresh operational analytics over transactional data: time-filtered dashboards, selective aggregates and application reporting while writes continue. Evaluate both query speed and the total resources needed to maintain the required freshness. This is a proposed focus, not evidence that SyntraDB already wins.

### 14.1 Optimize in measured steps

1. **Avoid reading data:** project requested columns, prune segments and row groups, then evaluate workload-specific sort keys. Include sorting and maintenance costs.
2. **Avoid decoding unnecessary values:** use selection vectors and late materialization; investigate predicates on dictionary codes where ordering and collation permit.
3. **Keep operators in batches:** run supported filter and aggregate pipelines inside the Rust engine, converting final output at the PostgreSQL boundary. A columnar scan feeding only tuple-at-a-time operators is an intermediate milestone.
4. **Choose encodings per column or block:** compare plain, dictionary, run-length and integer delta/bit-packed forms on representative data. Include decoding CPU and update amplification.
5. **Bound intermediate state:** implement aggregation memory limits. Until spill exists, reject plans whose state cannot be bounded; enforce a controlled resource error if estimates fail during execution. Do not retry after partial output.
6. **Add parallelism and SIMD after profiling:** keep scalar reference kernels, runtime CPU feature checks and portable fallbacks. Measure synchronization and allocation overhead before adding threads.
7. **Protect transactions:** reserve explicit memory, IO and worker budgets for scans and maintenance. Reduce analytical concurrency when OLTP latency exceeds its configured budget.

ClickHouse's [MergeTree documentation](https://clickhouse.com/docs/reference/engines/table-engines/mergetree-family/mergetree) describes sorted parts, granules and sparse indexes. Use it as a baseline to study and benchmark, without copying implementation code or assuming columnar storage alone is a competitive advantage.

### 14.2 Benchmark rules and provisional gates

All numbers below are **proposed engineering targets**, not achieved results. Revise them openly after baseline measurements.

| Gate | Initial target |
|---|---|
| Correctness | No known result mismatch or visibility violation in the declared supported scope |
| Enabled but idle | At most 5% OLTP throughput loss versus vanilla PostgreSQL under the declared load |
| Mixed workload | At most 10% OLTP p99 latency increase at the same offered transactional load; report throughput and errors too |
| Analytical prototype | At least 2x lower median latency than vanilla PostgreSQL on a predeclared supported scan/aggregate suite; publish every query |
| ClickHouse comparison | Report per-query wins and losses; no general superiority claim based on selected winners |
| Mutable HTAP | Publish freshness lag, fallback rate, write amplification, recovery time and resource costs; set workload-specific limits before the run |

Use identical hardware budgets, dataset contents and documented tuning. Include cold and warm cache runs, data larger than memory, selective and full scans, skew, high-cardinality groups and concurrent updates/deletes. Publish versions, seeds, SQL, configuration, repetitions and latency distributions. Keep isolated scan benchmarks separate from mixed workloads.

For strict mutable-data comparisons, define equivalent result and freshness requirements. Charge a PostgreSQL-plus-ClickHouse setup for ingestion and supporting resources when comparing complete systems. Also publish standalone ClickHouse OLAP results on a fixed dataset. If semantics differ, label the difference instead of ranking incomparable results.

## 15. Concrete next implementation milestone

**Implementation checkpoint:** the Rust core, streaming executor, immutable single-column int4 segments, all-row/range scan SQL adapters, and pass-through planner hook are present. Encoded segments can be saved as ordinary PostgreSQL `bytea` values. See [README.md](README.md) for setup and checks, and [the version 1 format contract](docs/segment-format.md) for layout and limits. The capture API reads locked sources directly and returns snapshot provenance. A managed fixed-copy catalog now provides owner isolation and atomic publication, refresh and delete. See [managed projections](docs/managed-projections.md). It does not establish live-scan eligibility. The baseline harness compares plain PostgreSQL, enabled heap execution and explicit segment scans on fixed data. See [the projection contract](docs/projection-lifecycle.md) and [benchmark methodology](benchmarks/README.md). There is no `CustomPath`, `CustomScan`, external-file manifest, compaction or synchronization yet. An assertion-disabled PostgreSQL build and paced concurrent transfer/analytics baseline are now available. See [the mixed workload contract](benchmarks/production-and-mixed.md) and [the initial optimized results](benchmarks/production-baseline-2026-10-27.md). The initial run preserved invariants but included client scheduling outliers; it does not establish the tail-latency target. Ten-second mixed phases with correlated request traces are now implemented; see [the timing contract](benchmarks/dispatch-diagnostics.md) and [longer-run findings](benchmarks/dispatch-findings-2026-10-27.md). The longer run completed 180,000 transfers without missed arrivals or invariant failures, but retained post-dispatch stalls in both native and enabled cases. Client traces alone cannot attribute those stalls. Server wait sampling now correlates several spikes with WAL initialization and other storage waits. See [wait findings](benchmarks/server-wait-findings-2026-10-27.md). Process crash, refresh isolation and native dump/restore checks succeeded; see [recovery checks](docs/recovery.md). Phase 0 still needs broader workloads, sustained resource tests and release upgrade checks. Full byte and metadata validation precedes pruning, so current scan counters do not prove physical IO savings. An admin-installed Rust statement trigger now tracks transactional source mutations. Its private counter passes initial rollback, replica-mode and ownership tests. See [source tracking](docs/source-tracking.md). DDL epoch guards now cover owner disable/write/enable gaps and source-drop cleanup. The native locked storage identity detects committed rewrites under an old snapshot. The tracker now uses sixteen transactional counter shards; measured costs and tail improvements are in [the counter comparison](benchmarks/counter-shards-2026-10-27.md). Explicit trusted publication and live eligibility now have a bounded int4 API; see Step 7. Automatic refresh and planner substitution remain unimplemented.

### Step 1: Resolve the adapter risk

Build the smallest Rust extension that loads, runs one SQL function and installs a no-op custom-path/custom-scan experiment. Confirm error handling, query cancellation, prepared-plan behavior and cleanup. Pin a supported PostgreSQL release only after this works. Record unsupported hook requirements rather than committing to a fork prematurely.

### Step 2: Freeze the narrow semantic scope

Start with nullable `boolean`, `int4` and `int8` columns; comparisons, `IS NULL`, conjunctions, projection, `COUNT`, `MIN` and `MAX`. Define output types and SQL three-valued logic. Add `SUM(int4)` with checked `int8` accumulation; defer `SUM(int8)` until PostgreSQL-compatible `numeric` results are implemented. Defer text/collations, floating-point edge cases, general `numeric`, joins, windows and user-defined functions.

Add bounded `GROUP BY` after the scan/aggregate path passes result comparisons. Unsupported operators must produce a clear capability rejection or heap plan, never an approximate answer.

### Step 3: Build one complete analytical path

Implement one versioned segment format with plain encoding, null bits, zone maps and checksums. Load a consistent immutable projection, scan it in bounded batches and compute supported aggregates. Connect an explicit experimental SQL entry point before attempting automatic planner selection. Expose rows scanned, bytes read, rows pruned, batch counts and peak accounted memory.

### Step 4: Prove behavior under failures

Compare SQL results with PostgreSQL at the same snapshot. Cover empty/all-null data, integer boundaries, overflow, unsupported expressions and malformed segments. Exercise cancellation, backend termination, interrupted publication and restart. Before mutable acceleration, add concurrent commit/abort, read-your-writes, long snapshots, update/delete, DDL and duplicate-replay cases.

Use Rust unit/property tests for codecs and kernels, fuzzing for untrusted file metadata, and PostgreSQL integration tests for visibility and lifecycle behavior. Keep concurrency tests deterministic where possible. Record seeds and failure schedules for reproduction.

### Step 5: Decide from results

Publish the supported SQL matrix and baseline comparison. Profile IO, decoding, tuple conversion, aggregate state and maintenance separately. Expand operator support only after the existing path is correct and its bottleneck is measured. Do not start distribution, broad automatic routing or a fork during this milestone.

### Step 6: Fresh Rust heap aggregates

The fresh all-row and inclusive-range entry points now stream a checked int4 source directly into the Rust kernel. They avoid a temporary segment, retain source read locks until transaction end, and use the active PostgreSQL snapshot. Native heap scan keys filter ranges before tuple values cross into Rust. This is an explicit SQL operator over transactional data; it does not require a trusted cache or change replay.

Tests cover empty and NULL input, integer endpoints, own writes, modifying CTEs and two-session snapshots. Native heap page checks preserve cancellation when no rows match. Serializable execution remains outside the custom scope until predicate-lock and conflict tests establish it. See the [fresh heap contract](docs/fresh-heap-aggregates.md) and [measured results](benchmarks/fresh-heap-2026-10-27.md).

The sixteen-shard tracker remains preparation for maintained projections. Its scalar total is diagnostic; future cache bindings need the complete vector, epoch, locked storage identity and matching visibility rules. A current guard configuration or matching count must never authorize reuse by itself.

### Step 7: Explicit trusted live reuse

The live int4 API now validates a private, admin-published segment against the
caller's snapshot. It compares every counter, the registration epoch and current
storage identity. Failed checks select a fresh Rust heap scan. An assigned caller
transaction ID also forces heap execution, covering writes inside row triggers.
See the [trusted live contract](docs/trusted-live-projections.md).

Publication uses a writable read committed transaction and nonblocking admission.
The initial catalog allows 64 columns, each within the existing segment limits.
Ordinary readers require source privileges and cannot publish or edit bindings.
Logical restore clears eligibility; administrators must rebind and republish.
Automatic refresh, planner routing and delta replay remain unfinished.

Next, test many readers, writers and maintenance tasks at once. Then add bounded
change replay with limits for memory, WAL and retained versions. Keep heap
fallback until each new case passes data and crash checks. Measure write cost
alongside scan speed before adding automatic routing.

### Step 8: Concurrent visibility stress

The opt-in harness now runs eight client sessions against one tracked source.
Writers commit and roll back; ordinary readers use read committed and repeatable
read. Competing publishers run alongside VACUUM, REINDEX and source DDL. Every
completed live result must match native SQL in the same statement snapshot.
The report records progress, rejection reasons, latency, allocated storage and WAL.
See the [stress contract](docs/trusted-concurrency-stress.md) for bounds and checks.

The next replay milestone needs stable row identity, a tested base/feed boundary
and snapshot-aware changes. Publish progress atomically and bound retained data.
Keep heap fallback for gaps and old snapshots. Test rollback, deletes, key changes
and crash replay before adding refresh automation.

### Step 9: Refresh cost and bounded replay primitives

The refresh probe now separates capture, tracked writes, changed publication and
unchanged publication. Root SELECT wrappers include AFTER STATEMENT trigger WAL;
utility commands use separately labelled node intervals. Normal request timing
stays separate from EXPLAIN instrumentation. See the
[refresh cost contract](docs/refresh-costs.md).

Publication can now return a fully validated current binding without replacing
its row. Assigned-XID transactions rebuild, protecting nested row-trigger reads.
Damaged bytes also rebuild. This preserves the existing snapshot contract while
avoiding redundant publication work.

The storage crate also has a bounded keyed-int4 replay kernel. It verifies prior
values, applies inserts/updates/deletes atomically, preserves per-key event order
and rejects gaps or conflicting retries. Row, batch and owned-buffer limits apply
before publication of new state. See the
[kernel contract](docs/keyed-replay-kernel.md).

The next integration step is a private transactional change feed for supported
primary-key sources. Prove the base/feed cut with transactions already in flight.
Then define snapshot filtering, bounded retention and atomic durable checkpoints.
No SQL path may treat the kernel's sequence as proof of PostgreSQL visibility.
Keep source DDL rejection and heap fallback until crash, restore and old-reader
tests cover the integrated path.

### Step 10: Transactional keyed capture

Opt-in capture now stores a bounded keyed base and transactional row events.
Seeding waits for an exclusive source lock and then pins a fresh snapshot. It
includes writers that committed during that wait. Source changes, event rows and
cursor reservations commit or roll back together. See the
[capture contract](docs/transactional-capture.md).

The initial scope is one nondeferrable int4 primary key and nullable int4 value.
0.3.0 also accepts int8 keys and values in any combination, with `numeric` sums
for int8 values; see [the int8 design](docs/int8-design.md).
0.4.0 also accepts int2 keys and values, stored at 4 bytes, and stores the net
change to each checkpoint partition as a bounded delta
and folds it into a new base past a fixed size; see
[partition deltas](docs/chunked-checkpoints.md#partition-deltas-040).
Other triggers, foreign-key triggers, inheritance and TRUNCATE remain unsupported.
Capacity is 16 sources, 100,000 base rows and at most 4,096 retained events per
source. A full log rolls back the source write. Admin reseeding and retirement
are explicit. The head cursor serializes captured writers per source.

Committed-session checks reconstruct rows through the replay kernel at the same
snapshot as native SQL. They test concurrent commits whose XID order differs
from event order. Process-crash and logical-restore checks cover the capture log.
This does not establish a durable materialized checkpoint or SQL replay routing.

Next, persist materialized state and applied progress atomically. Define which
checkpoint and remaining event prefix are valid for each reader snapshot. Reject
gaps and unsupported DDL. Test crash during apply and retained old readers before
adding bounded pruning or automatic refresh.

### Step 11: Durable replay and bounded pruning

The [checkpoint API](docs/durable-checkpoints.md) now commits keyed state,
columnar bytes and applied event progress together. Apply rejects gaps and bad
before values. Repeated unchanged maintenance validates without writing.
Explicit pruning deletes only the durable applied prefix and frees feed capacity.
Event ordinals can continue beyond 4,096 while retained events stay bounded.

The explicit int4 summary checks a checkpoint and head in the caller's source
snapshot. Matching progress permits reuse; lag, assigned XIDs, invalid guards or
corrupt segments select the heap. Old readers retain their visible MVCC versions.
Tests cover rollback, concurrent producers, old snapshots, crash retry and logical
restore omission. Automatic maintenance and planner routing remain unfinished.

Next, sustain concurrent capture, apply, prune and reads while measuring OLTP
latency, maintenance WAL and storage retention. The current apply still rewrites
both complete blobs. Use those measurements to design chunked updates before
expanding types, query operators or automatic routing.

### Step 12: Concurrent checkpoint maintenance

The [checkpoint stress harness](docs/checkpoint-concurrency-stress.md) now runs
writers, ordinary readers, competing appliers, a pruner and a keyed-row oracle.
A separate reader retains its initial snapshot throughout. The 60-second run
passed 50,895 aggregate checks and 9,220 keyed reconstructions. Independent
pruning deleted events, and final catch-up emptied the retained feed at ordinal
41,280. Process-crash and logical-restore checks also passed.

The [findings](benchmarks/checkpoint-stress-2026-10-27.md) record 414 MB of node WAL,
344 MB of allocated checkpoint storage and about 93% heap fallback during writes.
These results establish one bounded local correctness run. They do not establish
production readiness or a cross-database performance advantage.

Next, replace whole-source checkpoint rewrites with bounded chunk updates and an
atomic manifest. Preserve old snapshot references and test cross-chunk key moves,
skew, deletes and crash rollback. Then evaluate snapshot-safe replay of a bounded
reader tail. Measure maintenance cost and OLTP latency before adding a worker or
expanding planner routing.

### Step 13: Partitioned checkpoint publication

The [partitioned path](docs/chunked-checkpoints.md) stores 64 key-hash partitions
and a fixed manifest. Apply decodes and rewrites affected partitions, then
publishes their references and event cursor in the same PostgreSQL transaction.
Old readers retain matching MVCC versions. Pruning validates the complete state
before removing the applied prefix. Cross-partition failure, inherited corruption,
initial pending changes and deliberately skewed keys now have tests.

The [cost probe](benchmarks/chunked-checkpoints-2026-10-27.md) reduced median
two-event apply from 0.812 ms to 0.180 ms and backend WAL by about 93% on 10,000
rows. Read and full-validation latency increased. A separate 60-second run passed
59,381 aggregate checks and 8,465 keyed reconstructions, including a retained old
snapshot and independent pruning. Crash and logical restore also passed.

Next, replay a bounded pending tail for readers at their exact source snapshot.
Operate on affected partitions without persisting reader progress. Preserve
before-image checks and exact extrema after deletes. Fall back for gaps and
limits. Measure the combined read and maintenance cost before adding automation,
new operators or planner routing. Hash skew and migration support remain open.

### Step 14: Bounded reader-tail replay

The [reader path](docs/reader-tail-replay.md) now replays a visible pending suffix
in memory. Limits are 256 events, 65,536 affected-row work units and 8 MiB of
encoded partition inputs. It validates before images and recomputes exact extrema
after deletes. No reader publishes progress or prunes events. Replay is opt-in through the
three-argument summary overload; default calls preserve heap fallback on lag.
Own-XID, permission,
history, integrity and resource guards retain heap execution where required.

Native event scans now use epoch and ordinal bounds. Admin pruning deletes only
the interval after its old pruning cursor through the durable applied cursor.
Eight PostgreSQL tests and fourteen committed scenarios cover the new path. A
60-second run passed 30,205 aggregate checks and 6,187 keyed reconstructions, with
no ordinary-reader heap fallback. Old snapshots, crash and restore also passed.

The [findings](benchmarks/reader-tail-2026-10-27.md) show warm two-event replay at
0.101 ms versus native SQL's 0.324 ms. Concurrent reader cycle p99 still regressed
against the earlier heap-fallback run. This remains a performance gate. Profile
catalog lookup and retained versions, then measure paced load and calibrate reader
admission. Preserve the bounds and heap path before increasing scope or adding
automatic workers and planner routing.

### Step 15: Exact revision lookup and retained-version profiling

The [controlled probe](docs/catalog-lookup-probe.md) pins an old snapshot, pauses
writers at fixed retention depths and checks native answers. Chunk loads now use
64 exact revision probes through one native scan and snapshot. Index attribute
mapping, missing-index rejection and strict probe options have regression tests.

At 1,024 churn cycles, chunk entries fell from 836 to 64 per request. The
[results](benchmarks/catalog-lookups-2026-10-27.md) also retain fresh-read overhead,
higher publication cost and remaining head/manifest version growth. A 60-second
run passed 41,306 aggregate checks and 5,601 keyed reconstructions; reader cycle
p99 improved against the prior opt-in run, while writer event throughput fell.
Crash, restore and old snapshots passed. All 222 tests pass.

Next, measure paced load, isolate head/manifest costs and calibrate replay
admission across retention depth and skew. Preserve OLTP throughput, native heap
fallback and opt-in replay. Production upgrades, long soaks, wider SQL support
and fair cross-engine comparisons remain release gates.

### Step 16: Paced checkpoint load and explicit replay comparison

The [paced harness](docs/checkpoint-paced-load.md) adds finite absolute arrival
schedules, missed-arrival accounting and scheduled latency to the existing
correctness stress. It drops overdue slots without catch-up bursts and keeps
all native oracles, rollback checks, retained snapshots and progress gates.
The benchmark can explicitly choose reader replay; public defaults are unchanged.

Six 30-second runs compared replay on/off at 30, 100 and 300 cycles per second
per worker. They passed 115,346 aggregate checks and 16,427 keyed reconstructions.
All drains and retained snapshots passed; the 300/off build also passed crash
and restore. Four new Cargo tests bring the total to 226. Mutations verify the
schedule, defaults and option bounds.

The [findings](benchmarks/checkpoint-paced-2026-10-27.md) show zero missed arrivals
with replay off at 30 cycles/second. Replay on regressed that pair and missed work
at every tested rate. At 300 it improved some reader tails and completion counts,
but both modes were overloaded. Keep replay opt-in. Next, attribute stalls,
repeat matched trials and require both writer progress and reader latency gains
before selecting admission thresholds. These local runs establish no broad
performance or production-capacity claim.

### Step 17: Reverse metadata traversal with matched load trials

Head, manifest and current capture configuration reads now scan existing metadata
primary-key indexes backward. PostgreSQL MVCC still selects the visible row under
the exact required snapshot. Tuple order is a locality hint, not version proof.
Missing indexes retain heap scanning. No index or stored format was added.

The [controlled findings](benchmarks/metadata-traversal-2026-10-27.md) reduce head
entries from 152 to two and manifest entries from 12 to one per request at 1,024
churn cycles. Checkpoint median fell from 0.201 to 0.144 ms. Fresh medians stayed
similar. Old answers, reseeding, crash and restore checks passed.

Eight matched paced runs passed 112,555 aggregate checks and 14,337 keyed checks.
Both candidate runs at 30 cycles/second completed every arrival. At 100, writer
event counts and repeatable-read p99 improved against corresponding baseline
trials, while read-committed results were mixed and both builds missed work.
The full suite has 227 tests. A direction mutation failed the new scan test.

Keep replay opt-in. Next, time historical-reader traversal and attribute remaining
reader and maintenance stalls under page reuse and longer retention. This remains
an HTAP prototype with production and wider SQL gates open.

### Step 18: Historical-reader costs and maintenance profiling

The [expanded probe](docs/catalog-lookup-probe.md) now measures historical reads
inside the retained transaction, plus transfer/apply/prune cycles and separate
tail apply, prune and commit times. Native oracles check every aggregate and
preserved source snapshot. A transaction-ending mutation failed that oracle.

The [findings](benchmarks/historical-readers-2026-10-27.md) show about a 5% old-read
penalty from unconditional reverse traversal at 1,024 cycles. A snapshot/XID-based
candidate reduced old-read work, but was removed after mixed trials failed to
show reliable writer behavior. No heuristic was added to the retained runtime.
Its source archives and passing candidate checks remain available for review.

Six controlled runs passed 2,604 measured requests, old snapshots, crash and
restore. Four mixed runs passed 103,896 aggregate checks and 11,643 keyed checks.
The retained build passes 227 tests. The final checkpoint-specific cost probe
measured unchanged apply at 0.211 ms, two-event apply at 0.232 ms and two-event
prune at 0.295 ms. The earlier report now corrects a trusted-publication versus
checkpoint-apply attribution error.

Next, profile repeated metadata and payload validation. Evaluate a bounded atomic
apply/prune operation that can reuse verified state while preserving corruption,
visibility, rollback and permission checks. Keep replay opt-in and test matched
writer progress before retaining performance changes.

### Step 19: Atomic checkpoint apply and prune

The explicit [combined API](docs/atomic-checkpoint-maintenance.md) applies a
captured prefix and prunes through its applied cursor in one transaction. It
returns the applied ordinal and deleted-event count. It shares configuration,
head and unchanged-checkpoint validation inside one call. Changed publication
still verifies actual stored bytes under a fresh snapshot before pruning.
Concurrent producer events beyond the initial cut remain pending.

The [results](benchmarks/atomic-maintenance-2026-10-27.md) reduce unchanged
maintenance from 0.398 to 0.214 ms and from 0.483 to 0.246 ms in two probes.
Changed-path gains are limited, and mixed reader tails vary. No automatic worker
or reader default was changed. Fresh installation adds the SQL function and ACL;
existing databases need an explicit migration. The stored format is unchanged.

Six PostgreSQL tests and two Cargo tests bring the total to 235. Five mutations
failed focused checks. Committed scenarios verify old snapshots, no-XID/no-rewrite
retries, permissions, isolation restrictions and an observed producer commit during
paused publication. Trigger-induced stored corruption and prune errors roll back
the full operation. Crash tests exercise combined commit, abort and retry.

Seven timing reports cover two cost probes, four matched paced runs and saturated
60-second combined stress. Worker runs passed 219,343 aggregate checks and 19,314
keyed checks, with zero unexpected errors or invariant failures. Every retained
snapshot, progress gate and final drain passed. Logical-restore gates also passed.

Next, evaluate resource budgets and longer retained histories while preserving
full integrity checks. Keep performance claims scoped to measured workloads and
validate writer progress before changing maintenance defaults.

### Step 20: Bounded chunk input, soak and operator signals

Checkpoint validation, reader summaries, replay and changed apply now
[stream chunk input](docs/resource-bounds.md) one partition at a time. Each stored
state or segment is length-checked against its manifest row count before it is
detoasted or copied, so padded or truncated values fail as corrupt without being
read. `syntra_checkpoint_input_bytes()` reports the latest pass.

A [20-minute paced soak](docs/checkpoint-soak.md) with a retained snapshot,
independent vacuum and explicit limits found a real limit: while an old snapshot is
pinned, dead catalog versions cannot be cleaned, and writer throughput fell about 70% over ten minutes at roughly 55 writes per
second. Storage and memory bounds held and everything recovered when the snapshot
ended. The run fails its limit and is [recorded as a failure](benchmarks/checkpoint-soak-2026-10-27.md).

Two operator signals follow. `syntra_cleanup_horizon_age()` reports how far back
the oldest cleanup blocker is; the [policy](docs/resource-bounds.md#snapshot-age-policy)
sets alert and page thresholds. `syntra_capture_headroom(source)` lets a producer
see free event capacity before writers hit the hard error. It is advisory, counts
committed events only, and works for any role with a table privilege. Fresh
installation adds both functions and the earlier input-bytes function; existing
databases need an explicit migration. The stored format is unchanged.

Not done: finding the cause of the held-snapshot slowdown (a head-row redesign is
[on hold](docs/capture-v2-design.md) after a model failed to reproduce it), background maintenance, physical restore,
upgrades, monitoring integration and wider SQL types.

### Step 21: Held-snapshot cost and the first upgrade (0.2.0)

`EXPLAIN` on the real system measured the cause of the Step 20 slowdown. A key
lookup of the head row stepped over every version written since the old snapshot
began, 5 ms at window 5 of a diagnostic soak. Each captured row did three.

Release 0.2.0 removes the per-write head update without changing event order.
Writers read their configuration from a never-updated `capture_bindings` row, lock
the head row, and append one insert-only `capture_marks` row per trigger call. The
pruner folds marks into the head and deletes them, starting above its last cut.
See [transactional capture](docs/transactional-capture.md#base-boundary-and-write-contract)
and the [measured fixes](benchmarks/checkpoint-soak-2026-10-27.md#fixes-measured-2026-10-28).

The [20-minute held-snapshot soak](benchmarks/checkpoint-soak-2026-10-27.md#20-minute-rerun-on-020-2026-10-28)
now passes every limit with no missed writer arrivals, where 0.1.0 missed 15.5%.
A repeat run passed the same way. Both ran on one laptop at 30 cycles per second. At 60 cycles per second
writers still slowed at snapshot ages above about 41,000 while readers ran.

The first upgrade script, 0.1.0 to 0.2.0, carries live feeds and unapplied
history over without a reseed. `scripts/upgrade-check.sh` upgrades a populated
0.1.0 database and compares its catalog with a fresh install
([upgrades](docs/upgrades.md)). `syntra_capture_lag(source)` reports how far the
checkpoint trails the feed, and `syntra_capture_status()` lists lag and headroom
for every source the caller can inspect. `scripts/physical-restore-check.sh` restores a base
backup, taken while captured writes commit, with pending capture history to a named restore point and to the end of
the WAL archive ([recovery](docs/recovery.md#physical-backup-and-point-in-time-restore)).

An opt-in [background worker](docs/background-maintenance.md), enabled by
`shared_preload_libraries` and `syntradb.maintenance_database`, applies and prunes
each captured source of up to 8 listed databases, one worker per database, each source in its own transaction. It skips a busy
maintenance lock and logs and skips a failing source. `scripts/maintenance-check.sh`
checks both, plus draining and a clean stop. The default is unchanged: no worker.
`scripts/maintenance-load-check.sh` makes the worker the only applier for 200
paced transactions per second. In one 600-second run with a snapshot held to age
54,775, pending events never passed 203 and no transaction failed.

`scripts/replica-check.sh` reads checkpoints on a streaming standby while the
primary takes 200 captured writes per second, keeps the worker off it, stops the
primary in immediate mode under that load and promotes the standby; the worker
then drains new writes there.

`scripts/upgrade-scale-check.sh` timed the update at 118 ms for 8 sources and
164–183 ms for 16, each with 100,000 rows and thousands of pending events.

Not done: soaks on other hardware and at higher rates, restore and upgrades under
load, synchronous and cascading replicas, the worker under the full soak workload, integration with a monitoring system and wider SQL
types.

---

**Guiding principle:** Build **SyntraDB as one logical PostgreSQL-compatible HTAP database**. Preserve OLTP correctness and latency. Add measured analytical gains through a **custom-built columnar engine**. Begin with an extension and require evidence before choosing a fork.
