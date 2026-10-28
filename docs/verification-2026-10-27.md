# Verification on 27 October 2026

All 249 tests pass. These include 68 engine tests, 47 harness tests, and 134 tests
inside PostgreSQL. Workspace Clippy and compile checks
pass. Backend, test and copy audits report no errors in the latest changes.

| Area | Coverage location |
|---|---|
| Aggregate semantics | Core and executor Cargo tests |
| Encoding and corruption | Storage integration tests and independent binary fixture |
| Capture and source identity | PostgreSQL `source_tests.rs` |
| Named copies and ownership | PostgreSQL `projection_tests.rs` |
| Transactional counter shards | PostgreSQL `tracking_tests.rs` |
| DDL guards and rollback | PostgreSQL `ddl_tests.rs` |
| Trusted publication, capacity and fallback | PostgreSQL `live_tests.rs` |
| Committed trusted reuse and invalidation | Executable `syntra-bench/src/trusted_checks.rs` |
| Concurrent readers, writers and maintenance | Executable `syntra-bench/src/trusted_stress.rs` |
| Stress duration limits | Cargo `trusted_stress_options.rs` |
| Atomic keyed replay and bounds | Cargo `keyed_replay.rs` |
| Durable keyed format and event cursors | Cargo `keyed_checkpoint.rs` and independent binary fixture |
| Partition manifests, routing and payload digests | Cargo `chunk_manifest.rs` and independent Python fixtures |
| Atomic checkpoint apply and bounded pruning | PostgreSQL `checkpoint_tests.rs` |
| Checkpoint visibility, concurrency and crash retry | Executable `syntra-bench/src/checkpoint_checks.rs` |
| Public read-only tail replay, fallback and permissions | Executable `syntra-bench/src/tail_checks.rs` |
| Concurrent capture, apply, pruning and keyed snapshot oracle | Executable `syntra-bench/src/checkpoint_stress.rs` |
| Checkpoint stress duration and opt-in bounds | Cargo `checkpoint_stress_options.rs` |
| WAL accounting and opt-in probes | Cargo `wal_usage.rs` |
| Transactional capture and retirement | PostgreSQL `capture_tests.rs` |
| Committed capture cut and reconstruction | Executable `syntra-bench/src/capture_checks.rs` |
| Load accounting and diagnostics | Benchmark Cargo integration tests |
| Build provenance | Independent SHA-256 fixture and fingerprint tests |
| Two-session visibility and recovery | Executable checks in `syntra-bench/src/checks.rs` |

The filename-based missing-test tool expects names such as `tracking.test.rs`.
This Rust workspace uses Cargo integration files and pgrx `pg_test` modules.
Those tests execute against the implementation; adding duplicate filename stubs
would not improve coverage. The external checks run against disposable local
PostgreSQL clusters rather than mocked storage.

New tests were checked with temporary implementation or expectation changes.
The changed versions failed before restoration. Mutation runs covered projection
ownership and lifecycle, counter guards, DDL handling, source identity, writer
counts, cancellation, wait samples, and build fingerprints.

Process-crash and native dump/restore checks also succeeded. They cover committed
and uncommitted publication, counter recovery, restore rebind, an old-snapshot
TRUNCATE counterexample, native storage identity, read-only REINDEX, and extension
removal. See [recovery checks](recovery.md) for commands and scope.

This checkpoint does not establish production readiness. Sustained live-cache stress,
sustained replay load, write scaling, release upgrades and broader workload tests remain open. The
[readiness gates](readiness.md) keep those requirements explicit.

Fresh heap tests also cover native range keys, modifying CTE snapshots, and
transaction lock retention with generic prepared plans. See the
[fresh heap contract](fresh-heap-aggregates.md) for scope.

The live checks cover twenty cases with real commits and separate sessions.
They run before timing starts. Crash tests retain committed trusted bytes and
discard an aborted refresh. Logical restore clears trusted bindings. See the
[trusted contract](trusted-live-projections.md) for conservative fallback rules.

Disabling the full-vector comparison made the committed-write gate fail before
timing. The correct comparison was restored before the final measured run.

The stress oracle also rejected a build that bypassed version matching only on
its own source. Earlier checks passed, then a concurrent repeatable-read check
detected stale data. Both option-boundary tests failed under changed limits and
passed after restoration. See the [stress contract](trusted-concurrency-stress.md)
for commands and resource accounting.

The 60-second stress run passed 173,274 aggregate checks. Both publishers and all
four maintenance operations made progress. No unexpected error or invariant
failure occurred. This is one local run, with no comparative throughput claim.
The [findings](../benchmarks/trusted-stress-2026-10-27.md) retain WAL and storage
costs alongside the passing checks.

The keyed replay tests compare 200 fixed-seed batches with an independent map.
Mutations to prior-value checks, retry identity, memory limits and cancellation
all failed. WAL counter and opt-in mutations failed too. The live checks now cover
twenty committed-session cases, including unchanged publication without an XID.
An unsafe publication shortcut failed inside a row trigger. Removing the shortcut
failed the unchanged-publication check. Original code was restored after each run.

The optimized build passed another 60-second run with 174,557 aggregate checks,
plus the crash and logical-restore gates. The separate clean-cluster cost probe
recorded zero WAL bytes in all 51 unchanged-publication samples. See the
[cost findings](../benchmarks/refresh-costs-2026-10-27.md) for limits and raw reports.

Capture checks compare logged bases and visible prefixes with native keyed rows
through the Rust replay kernel. They cover a writer blocked by seeding, old
snapshots, reversed numeric XID order, savepoints, DDL gaps and retirement.
Changing seed or configuration reads to old snapshots made the checks fail.
Bypassing event limits, retirement or trigger-scope checks failed their tests too.
The assertion-enabled server also caught an unpinned metadata snapshot during
development. The corrected code pins it through both catalog scans.
See the [capture contract](transactional-capture.md) for limits and recovery scope.

The final capture run passed eleven committed-session cases and process-crash
and restore checks. Write-cost probes also verify emitted event counts and source
values. The [capture findings](../benchmarks/transactional-capture-2026-10-27.md)
retain those costs alongside the correctness evidence.

Durable checkpoints add ten codec/event-range tests and nine PostgreSQL tests.
They cover an empty checkpoint, NULL values, key moves, prefix retention, byte
corruption, rollback and 4,100 captured changes with periodic pruning. Temporary
mutations to CRC checks, restored retry identity, event advancement and the prune
boundary each failed the focused test. All original implementations were restored.

Thirteen committed-session checkpoint scenarios compare all five aggregates with
native SQL in the same snapshot. They check old checkpoint readers, snapshots
between source commit and apply, uncommitted publication, no-op behavior, producers
during publication, competing maintenance, ACLs and DDL gaps. Removing the applied
cursor comparison failed the committed-write gate before benchmark timing.

A process crash preserved the committed checkpoint and source change, while
rolling back an uncommitted apply/prune transaction. Retrying applied the pending
suffix once. Logical dump/restore omitted a populated checkpoint along with the
capture feed. Separate 51-sample probes measure checkpoint reads, two-event apply,
unchanged apply and pruning. The [checkpoint findings](../benchmarks/durable-checkpoint-2026-10-27.md)
retain raw samples and the matching source archive. These tests cover process
crashes, rather than physical restore, power loss or replica failover.

The checkpoint stress run passed for 60 seconds with eight workers, plus an old
snapshot held throughout. It checked 50,895 worker aggregates and reconstructed
9,220 keyed states against native source rows. Both writers and both appliers
committed and rolled back. The independent pruner deleted 11,826 events. Final
catch-up reached ordinal 41,280 and left no retained events. A targeted stale-read
mutation failed inside this stress phase after earlier gates had passed. Inverting
the keyed oracle also failed. Both CLI tests failed under changed defaults or
limits. Original implementations were restored before the final run.

The run measured 414 MB of node WAL and 344 MB of allocated checkpoint storage.
About 93% of ordinary reader checks used heap fallback. These costs keep chunked
updates and bounded reader-tail replay on the implementation roadmap. See the
[checkpoint stress findings](../benchmarks/checkpoint-stress-2026-10-27.md) for raw
samples, worker latencies, source inputs and the exact workload scope.

Partitioned checkpoints add seven manifest tests and seven PostgreSQL tests.
They cover independent binary and digest fixtures, untouched tuple versions,
partial-publication rollback, missing partitions, mismatched revisions, corrupt
inherited bytes, initial pending events and skewed keys. Disabling manifest CRC
validation and forcing all partitions dirty made the corresponding tests fail.
The original implementations were restored before the production runs.

The new 60-second run passed 59,381 worker aggregate checks and 8,465 keyed-row
reconstructions. It emitted 47,466 events and retained an old snapshot throughout.
Final catch-up emptied the feed. Process-crash and logical-restore gates also
passed. The monitor validates partition references and representation agreement
before replaying the pending prefix against native source rows.

The separate 51-sample cost probe reduced median two-event apply from 0.812 ms
to 0.180 ms and backend WAL from 134,593 to 9,443 bytes. Read, unchanged apply and
prune latency increased. The [partition findings](../benchmarks/chunked-checkpoints-2026-10-27.md)
retain both improvements and costs, with matching source archives. Snapshot-safe
reader-tail replay remains open; reads still fall back when the manifest lags.

Bounded reader replay adds eight PostgreSQL tests and fourteen committed-session
scenarios. Tests cover exact extrema, NULL/empty results, repeated keys, bad before
images, missing required events, corruption, 256/257-event admission, skewed row
budgets and byte accounting. PostgreSQL tests use the shared engine because their
harness has an XID; committed clients test the actual public XID and ACL guards.
Replay leaves persisted tuple identities and capture progress unchanged.

Indexed event reads now exclude applied history. An exclusive lower cursor and
inclusive visible-head upper cursor constrain the existing epoch/ordinal index.
Prune deletes only its newly applied interval. Event, row, byte, replay, index-bound
and pruning-cursor mutations all failed focused tests or the committed oracle.
Correct code was restored before final timing and recovery runs.

The final 60-second run passed 30,205 aggregate checks and 6,187 keyed reconstructions.
All ordinary-reader checks explicitly opted in and used checkpoint or replay
routes. The old snapshot
remained valid and final catch-up emptied the feed. Crash and logical restore also
passed. Warm two-event replay took 0.101 ms versus native SQL's 0.324 ms, but reader
cycle p99 worsened against the earlier heap-fallback run. The
[reader-tail findings](../benchmarks/reader-tail-2026-10-27.md) retain that regression,
measured read WAL, source inputs and profiling work still needed.

The default two-argument checkpoint API retains heap fallback on lag. Reader-tail
replay requires the three-argument overload with `replay_tail = true`. False also
preserves lag fallback. Committed scenarios test both default-off cases and the
explicit overload's own-XID protection. Enabling replay implicitly in the default
entry point made its compatibility check fail before timing. This keeps the
measured experimental regression out of default checkpoint behavior.

Exact revision lookup adds two PostgreSQL tests: reordered index attributes must
work across native rescans, and a missing version index must fail closed. Two
Cargo tests cover probe opt-in defaults and strict boolean parsing. Removing key
sorting or changing either option rule made the corresponding test fail. Correct
code was restored before the full check and timed runs.

The executable catalog probe checks native answers, route, no-XID reads, retained
snapshots and backend-local counter bounds. The filename-based coverage tool does
not recognize these executable or pgrx checks. The final full script passed all
222 tests, workspace Clippy and compile checks. The default Cargo test tool and
backend compile tool also passed. The
[catalog findings](../benchmarks/catalog-lookups-2026-10-27.md) retain fresh-read
and write costs alongside the lookup improvement, crash and restore results.

Paced checkpoint load adds four Cargo tests in `checkpoint_pacing.rs`. They check
absolute slot selection, overdue drops, finite schedule end, large elapsed times,
rate bounds, disabled stress and strict replay options. Scheduler, default and
rate-bound mutations failed these tests. Correct code was restored before the
full script passed 226 tests, Clippy and compilation. The filename-based coverage
tool expects `.test.rs` names; these Cargo tests and the executable stress oracles
cover the changed logic instead. Backend and test audits also passed.

Six paced runs at 30, 100 and 300 cycles per second per worker passed 115,346
aggregate checks and 16,427 keyed reconstructions. Old snapshots and final drain
passed in every run. The 300/off run also passed crash and logical restore.
Report hashes, source archives and both binary fingerprints agree. Independent
report checks reconcile attempted, missed and planned work and histogram counts.
See the [paced findings](../benchmarks/checkpoint-paced-2026-10-27.md) for the
measured replay regressions and overload behavior.

Reverse metadata traversal adds a PostgreSQL test for descending index order,
end-of-scan and missing-index heap fallback. Changing its traversal direction to
forward made that test fail. Restoring the implementation passed the focused
check. The full script passed 227 tests, Clippy and compilation. Existing committed
checks cover old checkpoint snapshots, reseeding and source changes. The coverage
tool's expected `.test.rs` filenames do not match these pgrx tests.

Two controlled metadata probes passed native and retained-snapshot checks, plus
crash and logical restore. Eight matched paced trials passed 112,555 aggregate
checks and 14,337 keyed reconstructions. Every progress gate and final drain
passed. All ten reports have verified hashes, matching binary fingerprints and
source archives. Candidate archives match final source bytes. The
[metadata findings](../benchmarks/metadata-traversal-2026-10-27.md) retain mixed
read-committed tails, missed work and historical-reader timing still needed.

The expanded catalog probe times historical readers without ending their initial
transaction. A transaction-ending mutation failed its committed native oracle.
It also checks two-event settlement and records churn and apply/prune/commit
costs. The filename-based tool expects `catalog_probe.test.rs`; this executable
multi-session oracle covers transaction semantics that a mock unit test cannot.

Six controlled runs passed 2,604 measured requests, retained snapshots, crash and
restore. Four mixed runs passed 103,896 aggregate checks and 11,643 keyed checks.
All progress gates and final drains passed. The snapshot-direction candidate
passed its own 228-test build and boundary mutations but was removed after its
mixed results. Candidate source and measurements remain archived. The retained
source passed the final full script's 227 tests, Clippy and compilation. Eleven
report/archive pairs have verified hashes and matching binary fingerprints.
See the [historical findings](../benchmarks/historical-readers-2026-10-27.md) for
maintenance costs, the rejected heuristic and the earlier cost attribution fix.

Atomic apply/prune adds six PostgreSQL tests for pending initial seed, key moves,
empty state, retry tuple identity, clean-partition damage, bytes changed by a
publication trigger, prune failure rollback, no-op integrity and runtime ACLs.
Two Cargo tests cover its benchmark flag's explicit default and strict parsing.
Removing stored-byte validation, removing no-op validation and skipping prune
made focused PostgreSQL tests fail. Changing the CLI default or parser also
failed its focused Cargo tests. Correct code was restored before the full check.

Private-catalog trigger fixtures cannot safely run in parallel with unrelated
catalog DDL cleanup in the same test database. The pgrx wrapper now defaults those
rollback fixtures to one test thread. No assertions or tests were removed.
Committed multi-session checks and stress workers continue testing concurrency.
The full script passed 235 tests, Clippy and compilation.

Committed checkpoint checks now exercise the combined path's rollback, retry,
old readers, no-XID/no-rewrite retries and nonblocking admission. New checks reject
read-only and repeatable-read maintenance. A catalog trigger pauses publication
at an observed advisory-lock wait while a producer commits a later event. The
combined call must preserve that event and its before image, then consume it
exactly once on retry. The old reader must keep its original answer. This uses
an observed database condition and a deadline rather than a fixed delay.
Crash checks now commit, abort and retry the combined operation.

The filename-based coverage tool expects `.test.rs` names. The pgrx tests,
`combined_maintenance_options.rs`, committed oracles and cost/stress assertions
cover these changes. They check transaction behavior against real PostgreSQL
rather than mocking snapshots or catalog persistence.

Seven timing reports have matching binary fingerprints and verified archives of
all 94 recorded inputs. A final test-only DELETE predicate change is recorded
in an extra verification report; runtime and benchmark source bytes are unchanged. The five worker runs passed
219,343 aggregate checks and 19,314 keyed reconstructions. Every progress gate,
retained snapshot and final drain passed. Independent report checks reconcile
writer commits, emitted and pruned events, offered arrivals and empty feeds.
The cost and saturated reports additionally passed combined commit/abort/retry
crash checks and existing logical-restore gates. All seven passed fifteen
committed checkpoint scenarios. The
[results](../benchmarks/atomic-maintenance-2026-10-27.md) retain both the unchanged
validation saving and the mixed workload's variable tails.

The last full check passed all 235 tests after tightening the empty-state test's
DELETE predicate to its two known keys. Final three-sample production and
recovery verification also passed. Backend audit has zero errors; its unwrap
warnings are test assertion failures in pgrx fixtures, as permitted for tests.
All eight report/archive pairs are verified. The final archive matches the
workspace; earlier timing variants preserve their original test source.

## Resource bounds, soak and producer headroom

The last full script passed 249 tests, Clippy and compilation.

New coverage: exact encoded lengths at 11 row-count boundaries and at the
`MAX_ROWS` limit; streamed chunk passes on a compressed 20,000-row checkpoint
(chunk count, bytes fetched, per-partition peak); padded and truncated stored
chunks rejected before they are read, with publication rolled back; soak option
parsing and limits; the cleanup horizon age rising by exactly the number of
transactions burned; and producer headroom counting down to zero, recovering after
prune, working for an INSERT-only role and rejecting a role with no table
privilege.

Mutations that failed a focused test: plain `get` instead of the exact-length read,
reader summaries also fetching states, summing instead of taking the peak,
`>=` on the soak catalog limit, a doubled RSS limit, floor division in the encoded
length, and a headroom permission check changed to never match.

Soak results: a 240-second paced smoke run passed. The 20-minute soak failed its
5% missed-arrival limit for writers (15.5%) while an old snapshot was held, with the
cause not yet established; a diagnostic run at twice the rate failed the same way. Neither is
a pass. See the [results](../benchmarks/checkpoint-soak-2026-10-27.md) and the
[snapshot age policy](resource-bounds.md#snapshot-age-policy). Backend, test and copy audits passed on the files changed in this work.

## Hot head row and upgrade path (28 October)

The last full script passed 255 tests: 68 engine, 48 harness and 139 inside
PostgreSQL. Clippy and compilation pass.

New coverage:

- the binding row copies the head's configuration and leaves with the feed;
- a tampered binding makes writers fail closed;
- a sequence of updates, a key move, an insert, a delete and a rolled-back write
  leaves the head row at the same `ctid` and appends exactly the expected marks
  (`1:2,2:2,4:2,5:3,6:2,7:2`), with headroom matching an SQL oracle;
- deleting the newest mark makes reader replay report a history gap;
- pruning every mark folds the live row count into the head, and the next read
  is exact.

Mutations that failed a focused test: reading configuration from the head again,
adding a no-op head update to every write, removing the check for events past the
newest mark, and folding the head's old live row count instead of the mark's.

`bash scripts/upgrade-check.sh` passed: a 0.1.0 database with applied and
unapplied capture history upgraded to 0.2.0, replayed that history exactly, kept
working, and matched a fresh 0.2.0 install on 462 catalog entries. Omitting one
index from the upgrade script made it fail. See [upgrades](upgrades.md).

The recovery checks and the 60-second checkpoint stress passed on this tree with
no invariant failures or unexpected errors. Their reports went to scratch files,
not to the archived results.

The held-snapshot soak still fails its writer limit: 11.7% missed, down from
26.4% in the same diagnostic. See the
[measured fixes](../benchmarks/checkpoint-soak-2026-10-27.md#fixes-measured-2026-10-28).

## Mark-delete bound, lag signal and physical restore (28 October, later)

The pruner's delete of folded marks now starts above its previous cut. A
no-readers ablation before the change showed the pruner collapsing first; with it
writers stayed flat to snapshot age 51,052. The change returns identical results,
so no test can tell the two statements apart; the soaks are its evidence.

`syntra_capture_lag(source)` is new in 0.2.0 and in the upgrade script. Focused
tests check its counts across writes, apply and prune (`0,-,0,-` through
`3,3,0,2`), access for a write-only role, and rejection of a role without table
privileges and of an uncaptured table. The upgrade check now also asserts lag
before and after the drain, and matched a fresh install on 464 catalog entries.

`bash scripts/check.sh` passed with 143 PostgreSQL tests.

`bash scripts/physical-restore-check.sh` passed: one base backup, taken with
capture history pending, restored to a named restore point and to the end of the
archive, matched the primary at each point and kept capturing and draining. A
copy that made `capture_marks` unlogged failed it.

The 20-minute held-snapshot soak with recovery checks passed every limit on 0.2.0
with no missed writer arrivals (`benchmarks/results/checkpoint-soak-20min-2026-10-28.json`).
A repeat on the later workspace (lag function and worker added) passed the same way
(`benchmarks/results/checkpoint-soak-20min-repeat-2026-10-28.json`).
Two 720-second diagnostics at twice the rate also passed, with 3.8% and 0.8% of
writer arrivals missed. See the
[rerun](../benchmarks/checkpoint-soak-2026-10-27.md#20-minute-rerun-on-020-2026-10-28).

## Maintenance worker (28 October, later)

The first build of the opt-in worker defined its settings in every backend.
PostgreSQL refuses postmaster settings after startup, so every PostgreSQL test
failed at library load with `cannot create PGC_POSTMASTER variables after
startup`. The worker check had passed because it preloads the library. The
settings are now defined only during preload. After the fix `bash scripts/check.sh`
passed with 143 PostgreSQL tests, and the upgrade check still matched a fresh
install on 464 catalog entries.

`bash scripts/maintenance-check.sh` passed: no worker without a maintenance
database; with one, the worker created the first checkpoint and drained writes
with nobody calling apply, skipped a held maintenance lock, kept draining beside a
source with a tampered binding while warning about it, and stopped cleanly.
Removing its per-source error handler made the check fail.

## Writes during the upgrade (28 October, later)

`docs/upgrades.md` said that capture writes arriving during `ALTER EXTENSION
UPDATE` wait for it. A check that holds the update transaction open for two seconds
and writes in the middle showed otherwise: the write failed with `capture
configuration is missing or changed` and its source change rolled back. Writers
take only a row-share lock on `capture_heads`, which the update's lock does not
block, and then find no bindings. Failing closed is safe, so the docs and the
script comment now say so. The upgrade check asserts this behaviour and still
matches a fresh install on 464 catalog entries.

`bash scripts/physical-restore-check.sh` now takes its base backup while a session
commits captured updates, applying and pruning as it goes. The recorded runs
committed 2,537 and 1,862 events during the backup and both restores matched.

## Maintenance worker under load (28 October, later)

`bash scripts/maintenance-load-check.sh 600 200` made the worker the only applier
for four pgbench clients at 200 transactions per second, with an old snapshot held
for the first 300 seconds. It passed: no failed transactions, at most 203 pending
events, horizon age 54,775 before release, and a drained checkpoint read that
matched the heap. With the interval set to one hour, the feed filled and the
check failed. Logs: `benchmarks/results/maintenance-load-600s-2026-10-28*.log`.

## Capture status (28 October, later)

`syntra_capture_status()` lists lag and headroom for every captured source the
caller can inspect. A source whose read fails keeps its row, with the error.
Three new PostgreSQL tests cover the counts, a role that can see one of two
sources, and a checkpoint corrupted to fail as `invalid_checkpoint` beside a
healthy source. Listing every source regardless of privileges made the second
test fail; removing the per-source exception handler made the third fail. The
function is in the upgrade script. The upgrade check asserts its row for the
upgraded feed and matches a fresh install on 490 catalog entries; the plpgsql
body spans several lines, so the count grew by more than the two functions.
`scripts/check.sh` passes with 146 PostgreSQL tests.

## Upgrade duration (28 October, later)

`scripts/upgrade-scale-check.sh` upgraded 0.1.0 databases with 8 and 16 captured
sources, each with 100,000 rows and 3,000 or 4,000 unapplied events.
`ALTER EXTENSION UPDATE` took 118 ms for 8 sources and 164, 166 and 183 ms in
three runs with 16. Every source kept its pending count and drained to a matching
checkpoint. Results: `benchmarks/results/upgrade-scale-2026-10-28.log`.

## Maintenance in several databases (28 October, later)

`syntradb.maintenance_database` now takes a comma-separated list of up to 8
databases, one worker each; a single name behaves as before. A malformed list
stops the server from starting. A PostgreSQL test covers the parser's splitting,
trimming and each rejection. `scripts/maintenance-check.sh` now also runs workers
for two databases, drains a feed in each, and checks that `'feeds,feeds'` is
refused with its message in the log. Pointing the second worker at the first
database made the check fail.

## Streaming standby (28 October, later)

`scripts/replica-check.sh` passed on the first run: checkpoint and replay reads on
a streaming standby matched its heap, maintenance and writes were refused there,
no worker ran on it, and after promotion the worker drained new writes. Starting
the worker at consistent state made the check fail. See
[recovery](recovery.md#streaming-standby-and-failover).

The check now also reads on the standby for 15 seconds while the primary takes
200 captured updates per second, then stops the primary in immediate mode under
that load and promotes. Two runs: 2,917 and 2,971 standby reads, all matching the
standby's heap; the promoted node drained new writes both times.

## int8 capture, 0.3.0 (28 October, later)

The workspace is now 0.3.0: capture feeds accept `int4` or `int8` keys and values
([design](int8-design.md)). `scripts/check.sh` passes with 155 PostgreSQL tests.
A change that truncated values to 32 bits failed three of the `int8` tests.

- **Upgrade.** `scripts/upgrade-check.sh` passes from 0.1.0 and from 0.2.0 with
  catalogs identical to a fresh install (498 entries). A write sent while the
  0.2.0 update is open waits and is then captured; from 0.1.0 it fails closed.
  The first version of the check expected only the fail-closed result and failed
  on 0.2.0 until it was corrected. The 0.3.0 update rewrites two capture tables:
  1.3 to 2.5 s at 16 sources of 100,000 rows, against about 160 ms for 0.2.0.
  See [upgrades](upgrades.md#020-to-030).
- **Recovery.** `physical-restore-check.sh int8` and `replica-check.sh int8`
  pass, as do the `int4` modes. See [recovery](recovery.md).
- **Benchmark harness.** The stress and soak workload read captured events as
  `int32` and failed against 0.3.0's `bigint` columns, as did the harness's own
  capture checks. Both are fixed; the workload takes `--checkpoint-column-type`.
- **Soak.** Three 20-minute runs passed every limit: `int4` on 0.3.0 and `int8`
  twice. Missed writer arrivals were 0.05%, 0.18% and 1.7%. The 1.7% run and a
  dip at the midpoint in both `int8` runs are unexplained. See
  [the soak](checkpoint-soak.md#layouts-and-recorded-runs).
- **Growth to the row cap.** `capture-growth-check.sh 1000000 4000` grew one
  `int4` source from 100,000 to 1,000,000 rows. Checkpoint reads matched the heap
  before and after a crash restart. Apply time rose from 17 ms to 144 ms, WAL was
  1,817 MB, and the 1,000,000-row summary read took 8 ms. Single writer, inserts
  only. See [resource bounds](resource-bounds.md#growth-past-the-seed-cap).
- **Churn at 1,000,000 rows.** Three 120 s runs of four writers plus an apply
  loop passed: the checkpoint stayed exact after the drain and after a crash.
  Committed transactions ranged from 2,548 to 9,344 and full-log refusals from 12
  to 2,146; the cause of that variance is not known. The chunk table reached 658 MB
  to 2 GB but held 8.5 to 9.2 MB after `VACUUM FULL`. Two earlier attempts failed
  (a script bug, and a `55P03` later traced to autovacuum; see the next item). See
  [resource bounds](resource-bounds.md#churn-at-1000000-rows).
- **Admission and autovacuum.** Autovacuum on an admission catalog made checkpoint
  maintenance, capture setup and trusted publication fail as busy (`55P03`); it
  occurred twice at 1,000,000 rows. Admission now retries for up to one second.
  `admission-vacuum-check.sh` reproduced the failure before the change, passes
  after it, and catches the bug at all three sites when the retry is removed.
  `scripts/check.sh` passes (155 PostgreSQL tests). See
  [resource bounds](resource-bounds.md#admission-and-vacuum).
- **Chunk table growth.** Two 600 s churn runs at 1,000,000 rows passed every
  correctness check, but the chunk table did not level off: 465 MB to 3.9 GB with
  default autovacuum, 776 MB to 1.4 GB with no cost delay, against 12.4 MB live.
  Each apply rewrites the touched partitions in full; WAL was about 0.65 MB per
  transaction. See [resource bounds](resource-bounds.md#chunk-table-growth-under-sustained-churn).
- **Partition deltas (0.4.0).** Apply now writes a per-partition delta and folds
  it into a new base past a fixed bound. `scripts/check.sh` passes (160
  PostgreSQL tests). The upgrade check passes from 0.1.0, 0.2.0 and 0.3.0, and
  the scale and under-load upgrade checks from 0.3.0. The same 600 s churn with
  default autovacuum passed every correctness check with 63,251 transactions,
  none failed on a full log, a chunk table of 83 to 198 MB and at most 0.10 MB of
  WAL per transaction (one run). A 1,800 s run passed with 211,297
  transactions, and the table stayed between 92 and 236 MB with no rising trend.
  See [the design](checkpoint-delta-design.md) and
  [resource bounds](resource-bounds.md#chunk-table-growth-under-sustained-churn).
- **Soak on 0.4.0.** The first 20-minute `int4` soak stopped in the bench keyed
  monitor, which did not merge deltas; the monitor now does. The next run passed
  but exposed that the admission retry also waited for concurrent maintenance
  callers, so applies committed 4.4 times as often. The retry now waits only for
  vacuum, and `admission-vacuum-check.sh` checks that a concurrent caller is
  refused within 200 ms. With that change two soaks passed with 1,148 and 27
  missed arrivals per writer, against 632 and 22 for a control of the 0.3.0
  baseline run alongside each; the slow pair slowed in both trees. See
  [the soak](checkpoint-soak.md#040-and-a-control-2026-10-28).
- **int2 columns.** Capture accepts `int2` keys and values, stored at 4 bytes with
  no catalog or format change. `scripts/check.sh` passes (166 PostgreSQL tests,
  six of them new), and the upgrade check passes from 0.1.0, 0.2.0 and 0.3.0.
  Three planted bugs each failed tests. See
  [int2 columns](int8-design.md#int2-columns-040).
- **Restore with many writers and feeds.** `physical-restore-check.sh` takes a
  third argument for the number of feeds and allows up to 64 writers. Runs with
  64 writers on one feed (two), 32 writers on 8 feeds, and 64 writers on 16 feeds
  (one `int4`, two `int8`) passed, with up to 66,823 events committed during the
  backup. See [recovery](recovery.md).
- **Downgrade to 0.3.0.** `syntra_fold_capture` folds every partition delta,
  and `syntradb--0.4.0--0.3.0.sql` refuses while any remain. Three new
  PostgreSQL tests pass (169 in all), and a fold that skipped its partitions
  failed two of them. `downgrade-check.sh` passed twice, going 0.3.0 to 0.4.0
  to 0.3.0 to 0.4.0 with captured history. The upgrade checks pass from 0.1.0,
  0.2.0 and 0.3.0. See [upgrades](upgrades.md#040-to-030-downgrade).
- **int2 restore and standby.** The physical restore check passed with `int2`
  feeds at 1, 16 and 64 writers, and the standby check passed twice with
  `int2`. The soak is not run with `int2`, because it needs 1,000,000 keys.
  See [recovery](recovery.md).
- **Quorum commit.** `quorum-check.sh` passed twice with `ANY 2` of three
  standbys. An `ANY 1` control with two stalled links lost acknowledged writes,
  and the check caught it both times. See
  [recovery](recovery.md#quorum-synchronous-commit).
- **numeric value columns.** Capture accepts `numeric(p,s)` values with
  p ≤ 18, stored as v × 10^s in the 8-byte layout, so formats are unchanged.
  Fifteen new tests pass (184 PostgreSQL tests in all), and three planted
  conversion bugs failed them. The upgrade checks pass with 504 catalog
  entries, and the downgrade refuses while a `numeric` feed exists. The
  physical restore check passed at 1, 16 and 64 writers and the standby check
  twice with `numeric(12,2)`. Eight of nine 20-minute `numeric(18,2)` soaks
  passed; the first stopped with an unrecorded pruner error, and the harness
  now keeps the error details and server log. Writer tail latency varied
  between runs; the runs do not show a `numeric` cost. The synchronous,
  cascading and failback check and the quorum check each passed twice with
  `numeric`. See
  [numeric columns](numeric-design.md).
- **Linux build.** In an Ubuntu 24.04 arm64 container, a non-root user ran
  `scripts/setup.sh` and `scripts/check.sh` against commit `afc0afb`. All 184
  PostgreSQL tests passed. The first two attempts failed: `cargo-pgrx` needs
  `libssl-dev`, which CI now installs, and clippy rejected seven `c_char`
  casts that are no-ops on arm64 Linux, where `c_char` is unsigned. Those
  casts now go through one helper. The CI workflow has not run, because the
  repository has no remote. The same check then passed at `52fa5ed` in an
  x86_64 Ubuntu 24.04 container, emulated by Docker Desktop on the arm64 Mac,
  with all 184 PostgreSQL tests
  (`benchmarks/results/linux-x86_64-emulated-check-2026-10-29.log`). It has
  not run on x86_64 hardware. The log is
  `benchmarks/results/linux-arm64-check-2026-10-29.log`.
- **Resealed decoder edits.** `crates/syntra-storage/tests/resealed_mutations.rs`
  makes 20,000 seeded edits of one to four bytes to each checkpoint, delta and
  manifest fixture, then rewrites the CRC so the parser sees the damage. Each
  decoder must refuse the bytes or accept only bytes it encodes back exactly.
  All six fixtures passed, with 8,026 to 15,547 of each 20,000 refused. Letting
  the delta decoder accept a nonzero value under an absent or NULL tag failed
  the test. Segments are not covered; they keep their own arbitrary-bytes test.
