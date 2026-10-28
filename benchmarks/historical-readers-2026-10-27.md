# Historical reader costs on 27 October 2026

Unconditional reverse traversal makes historical reads slower in this controlled
probe while improving current reads. A snapshot-aware candidate reduced old-reader
lookup work, but mixed trials did not justify its runtime cost. The candidate
was removed. The expanded probe and all experiment inputs remain available.

The [probe contract](../docs/catalog-lookup-probe.md) now samples both current and
historical readers while writers pause. Each run has 31 measured requests plus
one warmup per case. Historical samples keep their initial repeatable-read
transaction open. Every answer matches native SQL at that snapshot, with no
assigned XID. A separate check compares the native answer with the original
fixture after each stage. Ending the historical transaction made this oracle fail.

Forward and unconditional reverse builds ran in forward/reverse/reverse/forward
order. Two snapshot-aware runs followed. All runs used fresh disposable clusters,
10,000 source rows and the same updated benchmark code. There were no compiler or
test jobs alongside timed phases; the host was not isolated.

The archived hint kept backward traversal if the latest head is already represented by
the source snapshot, or is fewer than 128 XIDs beyond its upper horizon. An older
in-progress XID in the opposite modular half-range also keeps backward traversal,
since this hint cannot infer its age. Other older snapshots used forward scans.
Invalid inputs used forward traversal. The threshold is a locality heuristic,
not a time limit or bound on visited versions. Page reuse and transaction shapes
can change its benefit. All epoch, binding, permission and integrity guards remain.

At 1,024 churn cycles, median prepared request milliseconds and per-request
historical index entries were:

| Traversal | Trial | Current checkpoint, ms | Historical checkpoint, ms | Historical head entries | Historical manifest entries |
|---|---:|---:|---:|---:|---:|
| forward | 1 | 0.209167 | 0.168250 | 77 | 1 |
| forward | 2 | 0.198417 | 0.165625 | 77 | 1 |
| reverse | 1 | 0.138833 | 0.177292 | 77 | 12 |
| reverse | 2 | 0.140083 | 0.173166 | 77 | 12 |
| snapshot | 1 | 0.136125 | 0.137791 | 2 | 1 |
| snapshot | 2 | 0.136209 | 0.137541 | 2 | 1 |

Unconditional reverse traversal shifted visits from current head lookup to the
historical head lookup, and added historical manifest visits. The candidate
reduced both historical counters to two and one in this fixture, but that does
not establish a work bound across page reuse or other transaction histories.

Churn cycles include transfer, apply, prune and implicit commit. The table below
uses the final interval of 768 cycles. Settlement columns each time one separate
statement in the same explicit transaction for one two-event tail. They are
single observations per stage, not latency distributions. All times are ms.

| Traversal | Trial | Cycle median | Cycle max | Settle apply | Settle prune | Settle commit |
|---|---:|---:|---:|---:|---:|---:|
| forward | 1 | 1.051458 | 1.353166 | 0.526958 | 0.394042 | 0.050459 |
| forward | 2 | 1.075083 | 1.509625 | 0.685416 | 0.393542 | 0.057458 |
| reverse | 1 | 0.980770 | 1.454458 | 0.470875 | 0.347375 | 0.053625 |
| reverse | 2 | 0.958813 | 1.433250 | 0.459625 | 0.341666 | 0.050542 |
| snapshot | 1 | 0.970104 | 1.421125 | 0.462625 | 0.332167 | 0.060250 |
| snapshot | 2 | 0.970271 | 1.384000 | 0.510208 | 0.344625 | 0.067041 |

Four fresh 30-second mixed runs offered 100 cycles/second per worker with replay
explicitly enabled. Order was reverse/candidate/candidate/reverse. Successful
scheduled-cycle p99 below includes dispatch delay and the native oracle. Missed
arrivals are excluded from histograms. Writer misses combine two workers; each
reader had 3,000 offered cycles. Repeatable-read cycles contain nine checks.

| Traversal | Trial | Emitted events | Writer misses | RC p99, ms | RC misses | RR p99, ms | RR misses |
|---|---:|---:|---:|---:|---:|---:|---:|
| reverse | 1 | 29,628 | 570 | 3.501 | 0 | 18.527 | 24 |
| reverse | 2 | 24,108 | 1,582 | 5.483 | 1 | 25.935 | 674 |
| snapshot | 1 | 25,764 | 1,278 | 3.929 | 0 | 24.511 | 446 |
| snapshot | 2 | 24,234 | 1,559 | 4.051 | 0 | 26.447 | 713 |

The first candidate trial emitted 25,764 events versus 29,628 in the first reverse
trial. In the second
pair, writer counts were roughly equal while repeatable-read tails remained
slightly worse. Two trials do not separate the hint's overhead from host and
workload variance. They provide no reliable mixed-load improvement to justify
retaining the heuristic. The runtime restores unconditional reverse traversal.
Replay remains opt-in.

All 2,604 timed controlled requests passed their native, route and no-XID
checks. All six controlled runs passed crash, restore and retained-snapshot
checks. The mixed runs passed 103,896 aggregate checks and 11,643 keyed
reconstructions, with zero invariant failures or unexpected errors. Every old
snapshot, progress gate and final drain passed.

The final 51-sample cost run uses the retained runtime. Checkpoint-specific
medians were:

| Operation | Request ms | Statement WAL bytes |
|---|---:|---:|
| Native summary | 0.323666 | 0 |
| Checkpoint summary | 0.131041 | 0 |
| Two-event replay | 0.130875 | 0 |
| Two-event apply | 0.231666 | 9747 |
| Unchanged apply | 0.210875 | 0 |
| Two-event prune | 0.295334 | 239 |

These timings exclude setup and separate EXPLAIN probes. WAL is the root statement's
backend counter and excludes commit records. Unchanged apply is nearly as costly
as changed apply here; pruning is also substantial. This makes repeated metadata
and payload validation a useful next profiling target. The controlled cycles do
not attribute concurrent lock, storage or scheduling stalls. Do not remove
validation to reduce these costs. Evaluate a bounded combined apply/prune path
that can reuse verified state within one atomic operation.

The earlier [catalog lookup report](catalog-lookups-2026-10-27.md) now corrects a
cost attribution error. Trusted-copy publication and checkpoint apply are
separate APIs. Checkpoint cost numbers must come from
`refresh_costs.checkpoint_costs.cases`, rather than the top-level trusted cases.

The candidate added an XID-gap test covering boundaries, in-progress XIDs and
wraparound. Predicate and threshold mutations failed that test. Its archived
build passed 228 tests. Those runtime changes and their test were removed together.
The retained build has 227 tests. Ending the historical transaction also failed
the new committed oracle. Compile, Clippy and audit checks passed.

Raw reports:

- [historical-forward-trial1-2026-10-27](results/historical-forward-trial1-2026-10-27.json)
- [historical-forward-trial2-2026-10-27](results/historical-forward-trial2-2026-10-27.json)
- [historical-maintenance-costs-2026-10-27](results/historical-maintenance-costs-2026-10-27.json)
- [historical-mixed-reverse-trial1-2026-10-27](results/historical-mixed-reverse-trial1-2026-10-27.json)
- [historical-mixed-reverse-trial2-2026-10-27](results/historical-mixed-reverse-trial2-2026-10-27.json)
- [historical-mixed-snapshot-trial1-2026-10-27](results/historical-mixed-snapshot-trial1-2026-10-27.json)
- [historical-mixed-snapshot-trial2-2026-10-27](results/historical-mixed-snapshot-trial2-2026-10-27.json)
- [historical-reverse-trial1-2026-10-27](results/historical-reverse-trial1-2026-10-27.json)
- [historical-reverse-trial2-2026-10-27](results/historical-reverse-trial2-2026-10-27.json)
- [historical-snapshot-trial1-2026-10-27](results/historical-snapshot-trial1-2026-10-27.json)
- [historical-snapshot-trial2-2026-10-27](results/historical-snapshot-trial2-2026-10-27.json)

Forward source fingerprint:
`21f7714759132864045bbbff25691fa35cd881300f67378494347eb8f421bd21`.
Unconditional reverse and final source fingerprint:
`f8987afa3547f9afc21cdc226ced102b4da8760d27dd20779d34ac6f42cb6920`.
Rejected candidate source fingerprint:
`00444a3e69df49bae7ce3a3f88cdcdda0ab0aae0893174b65685333df0e15333`.
Every report has matching extension and runner fingerprints and verified
report/archive hashes in [source-variants](results/source-variants/). Reverse
and final-cost archives match the final workspace source inputs.
