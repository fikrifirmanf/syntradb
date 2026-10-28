# Metadata traversal results on 27 October 2026

Reverse traversal of existing metadata primary-key indexes reduces visits to
retained invisible entries for current readers in the controlled probe. It adds
no index and changes no stored format. PostgreSQL MVCC still chooses the visible
row. This traversal is a locality hint, not a promise that a physical tuple
location identifies the newest version.

The [catalog probe](../docs/catalog-lookup-probe.md) used 10,000 rows and 31
prepared samples per case. A separate old reader pinned history while writers
paused at each sample depth. Medians below are request milliseconds. Counters
are per request, including warmup; EXPLAIN is separate.

| Churn cycles | Checkpoint forward | Checkpoint reverse | Replay forward | Replay reverse | Head entries forward / reverse | Manifest entries forward / reverse |
|---|---:|---:|---:|---:|---:|---:|
| 0 | 0.134125 | 0.135750 | 0.142333 | 0.139375 | 2 / 2 | 1 / 1 |
| 64 | 0.141083 | 0.141792 | 0.148125 | 0.150333 | 10 / 2 | 1 / 1 |
| 256 | 0.150167 | 0.140000 | 0.156083 | 0.150042 | 38 / 2 | 3 / 1 |
| 1,024 | 0.200833 | 0.143500 | 0.212291 | 0.150250 | 152 / 2 | 12 / 1 |
| After release and VACUUM | 0.135000 | 0.133750 | 0.140625 | 0.138417 | 2 / 2 | 1 / 1 |

Chunk visits remained 64 per request in both builds. Fresh medians were similar.
The old snapshot retained its correct answer, and both controlled runs passed
process-crash and logical-restore gates. Existing checks cover reseeding and
source changes. One PostgreSQL test checks reverse order, end-of-scan and heap
fallback when the index is missing. A forward-direction mutation made it fail.
The full script passed 227 tests, Clippy and compilation.

Reverse traversal can shift work to historical snapshots, which may visit newer
invisible entries first. This probe checks the old reader's answer, but does not
time its historical queries. Page reuse can also change which traversal does
less work. There is no constant-time metadata lookup guarantee.

Eight fresh 30-second paced runs compared the archived forward build with the
candidate. Replay was explicitly on in every run. Each worker received 900 or
3,000 arrivals. Order was forward/reverse/reverse/forward at 30 cycles/second,
then reverse/forward/forward/reverse at 100. There were no concurrent compiler
or test jobs during timed phases. The host was not isolated.

The following are successful scheduled-cycle p99 milliseconds. Writer misses
combine both workers; reader misses count cycles for one worker. Repeatable-read
cycles include nine native-checked summaries. Dropped arrivals are excluded from
histograms and must be read alongside them. Expected lock rejections have their
own histograms in each report.

| Cycles/s per worker | Traversal | Trial | Emitted events | Writer misses | RC p99, ms | RC misses | RR p99, ms | RR misses |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| 30 | forward | 1 | 9,822 | 1 | 4.291 | 0 | 15.991 | 1 |
| 30 | forward | 2 | 9,828 | 0 | 4.795 | 0 | 15.543 | 0 |
| 30 | reverse | 1 | 9,828 | 0 | 4.043 | 0 | 14.615 | 0 |
| 30 | reverse | 2 | 9,828 | 0 | 3.925 | 0 | 15.127 | 0 |
| 100 | forward | 1 | 19,284 | 2,466 | 8.639 | 14 | 46.463 | 1,643 |
| 100 | forward | 2 | 18,708 | 2,571 | 13.143 | 24 | 65.215 | 1,659 |
| 100 | reverse | 1 | 24,120 | 1,579 | 12.727 | 16 | 31.455 | 673 |
| 100 | reverse | 2 | 21,696 | 2,024 | 10.607 | 35 | 38.943 | 922 |

The candidate completed every offered arrival in both 30-cycle runs. At 100,
both candidate runs emitted more writer events and had lower repeatable-read
p99 than their corresponding baseline trials. Read-committed results were mixed,
and every 100-cycle run missed arrivals. The change is retained for the measured
lookup and writer/RR improvements. Replay stays opt-in.

Next, measure the historical-reader cost of reverse traversal and profile the
remaining reader/native-oracle and maintenance work. Require matched-load writer
progress and reader latency gains before changing admission. Repeat under longer
retention, page reuse and isolated host conditions before a capacity claim.

Node WAL excludes final drain and VACUUM. Allocated checkpoint bytes below are
measured after drain; they are not live data size or reclaimed disk space.

| Cycles/s | Traversal | Trial | Node WAL bytes | Allocated checkpoint bytes |
|---|---|---:|---:|---:|
| 30 | forward | 1 | 19391848 | 7331840 |
| 30 | forward | 2 | 21844784 | 9625600 |
| 30 | reverse | 1 | 22644528 | 10313728 |
| 30 | reverse | 2 | 23179128 | 10821632 |
| 100 | forward | 1 | 61228504 | 30162944 |
| 100 | forward | 2 | 59680560 | 30154752 |
| 100 | reverse | 1 | 75180984 | 39886848 |
| 100 | reverse | 2 | 67609936 | 34865152 |

Across these runs, 112,555 aggregate checks and 14,337 keyed reconstructions
passed. Every retained snapshot, progress gate and final drain passed. There were
zero invariant failures or unexpected errors. Both controlled probes additionally
passed process-crash and logical-restore gates.

These are two runs per build and rate, not confidence bounds or a production
capacity estimate. Each percentile belongs to its own run. Native aggregate
oracle work is part of reader cycle latency. WAL includes all node activity in
the phase and has no backend attribution.

Raw controlled reports: [forward](results/metadata-forward-probe-2026-10-27.json)
and [reverse](results/metadata-reverse-probe-2026-10-27.json).

- [forward, 30 cycles/s, trial 1](results/metadata-forward-30-trial1-2026-10-27.json)
- [forward, 30 cycles/s, trial 2](results/metadata-forward-30-trial2-2026-10-27.json)
- [reverse, 30 cycles/s, trial 1](results/metadata-reverse-30-trial1-2026-10-27.json)
- [reverse, 30 cycles/s, trial 2](results/metadata-reverse-30-trial2-2026-10-27.json)
- [forward, 100 cycles/s, trial 1](results/metadata-forward-100-trial1-2026-10-27.json)
- [forward, 100 cycles/s, trial 2](results/metadata-forward-100-trial2-2026-10-27.json)
- [reverse, 100 cycles/s, trial 1](results/metadata-reverse-100-trial1-2026-10-27.json)
- [reverse, 100 cycles/s, trial 2](results/metadata-reverse-100-trial2-2026-10-27.json)

Forward source fingerprint:
`0a0a97c5d493c7ae386c5576ce0717f6f27d37a4b61e083ab55c533e52782b8d`.
Reverse source fingerprint:
`1db5ba958e2b81cc98874702037cfca0f4992e64577cdf8a108592a1f9f88493`.
Each report has matching extension and runner fingerprints, plus a source archive
and hash manifest in [source-variants](results/source-variants/). All report and
archive hashes were verified. Reverse archives match the final workspace inputs.

The follow-up [historical-reader measurements](historical-readers-2026-10-27.md)
now quantify the historical cost. A snapshot-aware candidate improved that probe
but was removed after mixed-load trials. Unconditional reverse traversal remains.
