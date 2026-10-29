# Production readiness gates

SyntraDB is a prototype. The current implementation is not a production HTAP
release. Narrow scan wins and successful microbenchmarks do not establish broad
OLTP, OLAP or HTAP superiority.

Work proceeds through concrete gates. Passing a later benchmark cannot waive an
earlier correctness or recovery requirement.

| Gate | Evidence | Work still needed |
|---|---|---|
| Build | Pinned Rust and PostgreSQL builds; the full check passes on macOS arm64 (193 PostgreSQL tests at 0.7.0), and passed on 0.4.0 (184 tests) on Ubuntu 24.04 in containers on arm64 and on x86_64 under emulation | A CI run on native x86_64 hardware, packages, more supported platforms |
| SQL | Nullable int4 matches PostgreSQL; capture feeds with int2, int4 or int8 keys and int2, int4, int8 or `numeric(p,s)` (p ≤ 18) values match PostgreSQL's aggregates, including sums past the `bigint` range | Other types, `numeric` keys and precision above 18, the cause of one failed `numeric` soak (twelve of thirteen passed), and bounded grouping |
| Visibility | Fresh scans and explicit trusted reuse | Broader types and snapshot stress |
| Access | Source ACL and catalog owner tests | Broader role and DDL checks |
| Integrity | Checksums and malformed input tests | Long fuzz runs and format upgrades |
| Recovery | Process crash and logical restore checks; physical base backup during captured writes with point-in-time restore of pending capture history; tested upgrades 0.1.0 to 0.2.0, 0.2.0 to 0.3.0, 0.3.0 to 0.4.0 and from 0.1.0 and 0.2.0 straight to 0.4.0, 0.4.0 to 0.5.0 and from 0.1.0, 0.2.0 and 0.3.0 straight to 0.5.0, 0.5.0 to 0.6.0 and from 0.1.0 through 0.4.0 straight to 0.6.0, 0.6.0 to 0.7.0 and from 0.1.0 through 0.5.0 straight to 0.7.0, with live capture history, where writes before the update fail closed and writes during it fail closed or wait and are then captured; the 0.3.0 update rewrites the capture tables and took 1.3 to 2.5 s at 16 sources of 100,000 rows; one `int4` source grown from 100,000 to 1,000,000 rows by captured inserts read back exactly, with apply time rising to 144 ms, then three 120 s runs of four-writer update, delete and key-move churn at that size that stayed exact, with the checkpoint chunk table growing to 300 times its live size in 600 s of sustained random writes on 0.3.0, because each apply rewrote touched partitions in full, and up to about 18 times on 0.4.0, where an apply writes per-partition deltas, staying in that band for a 1,800 s run; restore and standby checks also pass with `int2` and `int8` feeds; a downgrade from 0.4.0 to 0.3.0 and back with captured history, after folding partition deltas (two runs); a downgrade from 0.5.0 to 0.4.0 that drops trusted projections keyed to a capture position; a downgrade from 0.6.0 to 0.5.0 that waits for a writer with queued changes and captures them; a downgrade from 0.7.0 to 0.6.0 that waits for a running apply and moves appended deltas back inline; physical restore with 64 writers spread over 16 feeds of 1,000 rows passed on 0.4.0 (two `int8` runs, one `int4`); an upgrade with four writers running and a drain loop passed from 0.1.0 and 0.2.0 with no write lost or doubled, and from 0.3.0, 0.4.0 and 0.5.0 to 0.6.0, and from 0.3.0, 0.5.0 and 0.6.0 to 0.7.0, with no write failed; streaming standby reads under load and promotion after an immediate primary stop (two runs); a synchronous standby that kept every acknowledged write across an immediate primary stop, a cascading standby that followed the new timeline, and failback of the old primary with `pg_rewind` (two runs); quorum commit with `ANY 2` of three standbys that kept every acknowledged write after losing the primary and the most advanced standby, with one link stalled (two runs; an `ANY 1` control lost writes and was caught) | Restore of large feeds or with more than 64 writers or 16 feeds, network partitions between machines, upgrades with many writers or large feeds, downgrades below 0.3.0 or under load |
| Load | Paced mixed tests, concurrent reuse stress, and the 20-minute held-snapshot soak, which passed on 0.2.0 with no missed writer arrivals (two runs, one laptop) and on 0.3.0 for `int4` (one run) and `int8` (two runs, with 0.2% and 1.7% of writer arrivals missed; see [the soak](checkpoint-soak.md#layouts-and-recorded-runs)), and on 0.4.0 for `int4` (two runs, 3.2% and 0.1% missed, against 1.8% and 0.1% for a 0.3.0 control run alongside each) ; a two-hour soak holding the snapshot for an hour failed on 0.4.0 and again on 0.5.0: writers slowed past a horizon age near 90,000 and recovered after release; on 0.6.0 writers held full rate to about 110,000 in a quiet run (11% of arrivals missed against 19%) and then slowed after pruning stalled behind long applies | Other hardware, higher rates; writer speed under holds past 90,000 (110,000 on 0.6.0), traced on 0.5.0 to the feed head-lock queue; 0.6.0 takes that lock at commit, and its remaining slowdown follows apply time under the old snapshot, which blocks pruning ([findings](../benchmarks/held-snapshot-writers-2026-10-29.md)); appliers still pay for chunk versions kept by an old snapshot |
| Mutable HTAP | Transactional capture, durable replay and explicit snapshot-safe reuse | Replay cost admission, paced load and adaptive partitions |
| Operations | Local build and benchmark scripts; SQL signals for cleanup horizon age, feed headroom and checkpoint lag, with `syntra_capture_status()` listing every source; an opt-in maintenance worker for up to 8 listed databases, which kept one feed drained at 200 paced transactions per second with an old snapshot held (one run); a pilot [runbook](operations.md) | Packages, deployment, integration with a monitoring system |
| Speed | Raw local workload measurements | Wider fair comparisons |
| Release | No public production release | License and support contract |


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
