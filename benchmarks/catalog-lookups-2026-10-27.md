# Catalog lookup results on 27 October 2026

Exact chunk revision probes bound chunk index visits under retained snapshots.
They also add work to fresh reads and checkpoint writes. Reader-tail replay
remains opt-in, and SyntraDB remains an HTAP prototype.

The [probe contract](../docs/catalog-lookup-probe.md) describes the paused-writer,
10,000-row workload. The baseline has seven samples per case; the new run has
31. These are sequential local runs, without paired trials or confidence bounds.
Times below are median prepared request milliseconds.

| Retained churn cycles | Checkpoint before | Checkpoint after | Two-event replay before | Two-event replay after | Chunk entries before / after |
|---|---:|---:|---:|---:|---:|
| 0 | 0.102375 | 0.132500 | 0.108666 | 0.143333 | 64 / 64 |
| 64 | 0.113459 | 0.137125 | 0.114375 | 0.141958 | 114 / 64 |
| 256 | 0.130417 | 0.148334 | 0.150375 | 0.156667 | 259 / 64 |
| 1,024 | 0.221792 | 0.209667 | 0.235709 | 0.209209 | 836 / 64 |
| After release and VACUUM | 0.096500 | 0.126875 | 0.102333 | 0.139917 | 64 / 64 |

At 1,024 cycles, chunk heap buffer accesses fell from 787 to 64 per request.
Fresh reads increased from 12 to 64 accesses. The new run still visits 152 head
entries and 12 manifest entries at that depth, versus two and one after VACUUM.
Thus chunk lookup is bounded, but retained metadata still raises total latency.
Old-snapshot answers and native aggregate checks passed in both runs.

The separate 51-sample checkpoint cost run measured two-event apply at
0.386584 ms and 9,747 statement WAL bytes (medians), versus 0.187625 ms and 9,443
bytes in the prior reader-tail run. Unchanged apply measured 0.356208 ms with
zero statement WAL. Two-event prune measured 0.484250 ms and 239 bytes, versus
0.671542 ms and 245 bytes before. These sequential local runs do not attribute
all differences to the new index.

Captured transfer measured 0.244209 ms and 965 bytes, versus native transfer at
0.050833 ms and 142 bytes. These are separate statements, not end-to-end
transaction costs. An earlier version of this report mixed the trusted-copy
`publication_changed` case with checkpoint apply. The numbers above use
`refresh_costs.checkpoint_costs.cases`; trusted-copy publication is a separate API.

The new 60-second saturated mixed run passed 41,306 aggregate checks and 5,601
keyed reconstructions, with zero invariant failures or unexpected errors. Writers
emitted 30,888 events. Final catch-up emptied the feed, the retained snapshot
stayed valid, and crash and logical-restore gates passed. All 41,124 ordinary
reader checks used checkpoint or explicit replay routes.

| Mixed-run measure | Prior reader-tail run | Exact revision lookup run |
|---|---:|---:|
| Read-committed cycle p99, ms | 9.127 | 5.223 |
| Repeatable-read nine-check cycle p99, ms | 76.031 | 46.687 |
| Emitted events | 36,204 | 30,888 |
| Keyed checks | 6,187 | 5,601 |
| Node WAL bytes, excluding final drain/VACUUM | 120,545,600 | 113,880,104 |

Reader tails improved in this run while writer event throughput fell. The earlier
heap-fallback run still had lower reader cycle p99 (4.227 and 39.455 ms). There is
no basis here for enabling replay by default or claiming broad speed superiority.
Allocated checkpoint storage after drain was 57,008,128 bytes. VACUUM did not
shrink the allocated files; this is not a memory or disk reclamation result.

A trial with newest-first head and manifest indexes failed the short stress
progress gate. Those indexes were removed and the gate stayed unchanged. Their
extra write cost needs evidence before reconsideration.

Next, measure paced offered load and isolate head/manifest lookup costs without
sacrificing write throughput. Then calibrate replay admission against native heap
execution across retention depth and skew. Keep the existing correctness bounds,
XID guard, fallback and opt-in API while those gates remain open.

Raw reports: [before](results/catalog-probe-before-2026-10-27.json),
[after](results/catalog-probe-after-2026-10-27.json),
[costs](results/revision-lookup-costs-2026-10-27.json),
[mixed stress](results/revision-lookup-stress-60s-2026-10-27.json).
The baseline source fingerprint is
`3a866ec5efe3884a967368e713130c858dd489ff416cff06102ca7bc4d43385d`;
the three final reports use
`bcc0554abe55654f49c33b27e041f465a7228be6a6bdf2f3f0c279e8f7e0c973`.
Each report has a matching archive and hash manifest in
[source-variants](results/source-variants/). Both binaries report matching inputs.
