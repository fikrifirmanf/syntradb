# Production readiness gates

SyntraDB is a prototype. The current implementation is not a production HTAP
release. Narrow scan wins and successful microbenchmarks do not establish broad
OLTP, OLAP or HTAP superiority.

Work proceeds through concrete gates. Passing a later benchmark cannot waive an
earlier correctness or recovery requirement.

| Gate | Evidence | Work still needed |
|---|---|---|
| Build | Pinned Rust 1.99.0, PostgreSQL 18.6 and pgrx 0.19.3. [0.12.0 checks](verification-0.12.0.md) pass 274 PostgreSQL tests. [Pilot archives](pilot-packages.md) pass fresh installation checks on macOS arm64 and Ubuntu 24.04 x86_64, including installation from extracted files. | Distribution formats, deployment rehearsals and a supported binary compatibility matrix. |
| SQL | Capture supports integer keys and integer or fixed-scale numeric values. Explicit [heap grouping](heap-grouped-aggregates.md) matches native aggregates in all nine integer layouts. Fresh [text and UUID grouping](text-uuid-grouping.md) adds bounded keys over integer measures; text requires UTF8 and deterministic source collations. Fresh [time-window aggregates](temporal-aggregates.md) support date and timestamp filters over integer and fixed-scale numeric values. | Broader accelerated types and operators, multiple grouping keys and planner integration. Native PostgreSQL remains the normal SQL executor. |
| Visibility | Heap and checkpoint tests cover statement snapshots, repeatable read, own writes, retained readers and cancellation. [0.9.0 fresh SSI checks](verification-0.9.0.md) pass locally and on native Linux, including conflicts and recovery after read-time aborts. | Cached-state SSI needs separate proof. Broader type and concurrency stress remains open. |
| Access | Source ACL, owner, RLS rejection and DDL tests pass. Public catalog access is restricted. | Broader deployment roles and security review of unsafe boundaries. |
| Integrity | Checksums and malformed-input tests pass. Parser fuzzing completed an hour and grouping differential fuzzing completed five minutes. | Longer corpus runs and a broader stored-format compatibility matrix. |
| Recovery | The complete [21-case Linux lifecycle suite](verification-2026-10-31.md) passes on 0.7.0. The 0.8.0 through 0.10.0 suites also pass, along with the [33-case 0.11.0 suite](verification-0.11.0.md) and the 36-case 0.12.0 suite ([verification record](verification-0.12.0.md)). | Larger restores, real network partitions, power-loss and multi-machine evidence remain open. |
| Load | The first [two-hour Linux soak](../benchmarks/checkpoint-soak-linux-2026-10-31.md) failed offered-load limits. The [profiled repeat](../benchmarks/profiled-soak-linux-2026-10-31.md) also failed: writers missed about 21.7%, repeatable read 27.8%. Final correctness checks passed. Stacks show substantial native source-index and verification-scan costs. | A [matched Linux control at 0.12.0](../benchmarks/hold-control-linux-2026-11-01.md) shows plain PostgreSQL also collapses under the held snapshot (200 to 58 transactions per second), so the absolute limit mostly measures PostgreSQL. Captured is far slower than plain between horizon ages of about 40,000 and 110,000, then trails by 10% to 25%. The throughput gate stays open; the gate needs a native-relative form and the middle band needs a cause. A two-hour [soak at ca5a395](../benchmarks/soak-linux-ca5a395-2026-11-02.md), after kept plans and commit-time batching, still fails the offered-load limit but improved: writers missed 9.7% (was about 21.7%) and repeatable read 15.6% (was 27.8%); read committed missed none, and no correctness check failed. |
| Mutable HTAP | Transactional capture, durable replay and explicit snapshot-safe checkpoint reuse have correctness and recovery checks. | Replay cost admission, adaptive partitions, broader scale and a planner route with a proven fallback. |
| Operations | Local scripts, feed-status SQL, an opt-in maintenance worker, [pilot packages](pilot-packages.md) and the [pilot runbook](operations.md) exist. Worker tests cover source lock isolation and shutdown. | Deployment automation, monitoring integration and rehearsed operational procedures. |
| Speed | The [memory-controlled Linux grouping run](../benchmarks/integer-grouping-memory-linux-2026-10-31.md) preserves raw samples and plans. Native parallel plans win at smaller group counts; native in-memory plans win the large-group cases in that run. | Repeat matched-host controls and broader workloads. Keep native execution as the normal SQL route. |
| Release | GPL-3.0-only license, code of conduct, [security policy](../SECURITY.md), [contributing guide](../CONTRIBUTING.md) and [changelog](../CHANGELOG.md) adopted. 0.12.0 is tagged, but there is no production release. [Dependency checks](dependency-advisories.md) retain one unmaintained pgrx dependency warning. | A supported deployment matrix, release packages, a support policy and resolution of the technical gates above. |


Managed fixed copies now pass initial lifecycle, recovery and owner checks.
The explicit live API now checks snapshot bindings and falls back to the heap.
An initial 60-second run passed data checks while writers, readers and maintenance
ran together. The combined workload generated 4.56 GB of node WAL. See the
[stress findings](../benchmarks/trusted-stress-2026-10-27.md).
The [refresh probe](refresh-costs.md) now measures tracking and publication costs.
The [keyed replay kernel](keyed-replay-kernel.md) passes atomicity and limit tests.
The [capture layer](transactional-capture.md) now passes its first snapshot-cut
and recovery checks. [Durable checkpoints](durable-checkpoints.md) now add atomic
applied state, explicit snapshot-safe summaries and bounded event pruning.
The [checkpoint stress run](../benchmarks/checkpoint-stress-2026-10-27.md) now
passes concurrent apply, pruning, keyed oracles and retained readers. It also
exposes full-blob WAL, allocated storage growth and frequent heap fallback.
[Partitioned checkpoints](chunked-checkpoints.md) now reduce changed-apply rewrites
and pass crash, restore, rollback and retained-reader tests. The
[measured costs](../benchmarks/chunked-checkpoints-2026-10-27.md) also show slower
full validation and continued lag fallback. [Bounded reader replay](reader-tail-replay.md)
now passes snapshot and integrity checks and removes fallback in an opt-in stress run.
Its [measured concurrent tails](../benchmarks/reader-tail-2026-10-27.md) still regress,
so default checkpoint calls retain heap fallback on lag.
[Exact chunk revision lookup](catalog-lookup-probe.md) now bounds chunk index visits.
The [new measurements](../benchmarks/catalog-lookups-2026-10-27.md) improve concurrent
reader tails but show higher fresh-read and publication costs. Head and manifest
version lookup grew with retained history in the forward-scan build.
[Reverse metadata traversal](../benchmarks/metadata-traversal-2026-10-27.md) now
reduces that work for current readers in the controlled probe. Matched paced
trials improve writer progress and repeatable-read tails, with mixed
read-committed results. [Historical-reader timing](../benchmarks/historical-readers-2026-10-27.md)
now quantifies the reverse-scan tradeoff. A snapshot-aware hint improved isolated
old reads but was removed after mixed-load trials. Page reuse and longer
histories still need broader testing. The
[paced comparisons](../benchmarks/checkpoint-paced-2026-10-27.md) now expose missed
arrivals at fixed offered rates. Replay helps some overloaded readers but loses
work at all three tested rates; default heap fallback remains appropriate. Next,
profile repeated maintenance validation and evaluate atomic apply/prune reuse
without weakening integrity. Repeat matched trials before calibrating admission. Keep native
heap execution as the default whenever eligibility is unproven. Avoid expanding
the optimizer's scope until the storage and visibility gates hold.

Server wait sampling closes the current measurement question. It should not turn
into repeated benchmark tuning while core database requirements remain unfinished.

[Atomic apply/prune](atomic-checkpoint-maintenance.md) now reuses unchanged-state
validation inside one explicit administrative call. Changed publications still
re-read and verify stored bytes before pruning. The
[measurements](../benchmarks/atomic-maintenance-2026-10-27.md) reduce unchanged
maintenance cost in two probes, with little changed-path gain and variable mixed
tails. Next, test resource limits and longer retained snapshots. This does not
change reader replay admission or enable automatic maintenance.

The native Linux two-hour 0.7.0 soak failed its offered-load limits: writers missed about 17.1% of cycles and the repeatable-read worker missed 23.5%. Both recovered after the retained snapshot ended. See the [findings and versioned data](../benchmarks/checkpoint-soak-linux-2026-10-31.md). This leaves the load gate open.

The complete native Linux [21-case lifecycle run](https://github.com/fikrifirmanf/syntradb/actions/runs/41168092491) passed on `d23f65b`. It covers the listed recovery, replication and version-lifecycle workloads. It does not override the failed two-hour offered-load result.
