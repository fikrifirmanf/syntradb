# Paced checkpoint results on 27 October 2026

Paced tests expose missed work that service latency alone can hide. Replay helps
some overloaded readers in these runs, but no replay-on run completed every
offered arrival. The public API keeps replay off by default.

Six fresh disposable clusters ran for 30 seconds each with 10,000 source rows.
The [workload contract](../docs/checkpoint-paced-load.md) defines eight schedules,
all native and keyed oracles, dropped slots and timing. Each worker was offered
900, 3,000 or 9,000 cycles. Writer misses below combine both writers; reader misses
are cycle counts for one worker. Repeatable-read cycles include nine checks.
All times are scheduled-arrival-to-completion p99 milliseconds for successful
cycles. Rejected cycles have separate histograms in the raw reports.

| Cycles/s per worker | Replay | Emitted events | Writer arrivals missed | Read-committed p99, ms | RC misses | Repeatable-read p99, ms | RR misses |
|---|---|---:|---:|---:|---:|---:|---:|
| 30 | off | 9,828 | 0.0% | 3.629 | 0 | 15.903 | 0 |
| 30 | on | 9,264 | 5.6% | 24.511 | 32 | 72.063 | 53 |
| 100 | off | 30,996 | 5.3% | 3.881 | 0 | 25.599 | 453 |
| 100 | on | 29,118 | 11.1% | 3.903 | 0 | 26.255 | 459 |
| 300 | off | 22,476 | 77.1% | 7.131 | 329 | 40.223 | 7,803 |
| 300 | on | 26,196 | 73.3% | 3.631 | 26 | 29.663 | 7,123 |

At 100 cycles/second, replay increased writer misses and did not improve reader
tails in this pair. At 300, replay reduced reader misses and increased emitted
events, but writers still missed most arrivals. These runs cannot support a rule
that enables replay solely because offered load is high. Native oracle work is
part of both reader modes; these are workload cycle tails, not standalone query
timings. In-flight work can finish after the scheduled phase ends.

At 30 cycles/second, replay off missed no arrivals across all eight workers.
Replay on still missed work and had much worse tails in its separate run.
A lower offered rate alone did not eliminate replay stalls. Slot alignment, retained snapshots, catalog work, VACUUM,
storage latency and host scheduling can all affect these measurements. This test
does not attribute stalls to any one cause. There was no compiler or other test
job running alongside the timed worker phases, but the host was not isolated.

These are single local runs per rate and mode. Execution order was 100 off/on,
300 on/off, then 30 on/off. Opposite pair ordering does not replace randomized
repetitions or confidence bounds. Dropped arrivals are absent from latency
histograms and must be considered alongside the percentiles. Maintenance workers
also report lock rejections, missed work, WAL and allocated files in the raw data.

Across all six runs, native aggregate checks totaled 115,346 and keyed
reconstructions totaled 16,427. All invariant and unexpected-error counts
were zero. Retained snapshots stayed valid, every progress gate passed and final
drain left no events. The 300/off run additionally passed process-crash and
logical-restore checks for this source build. Four new Cargo tests and three
rejected mutations cover schedule and CLI boundaries. The full check passed
226 tests, Clippy and compilation.

Next, attribute the remaining stalls and repeat matched runs before defining
an admission threshold. Measure reader service time, head/manifest work and
maintenance contention while tracking writer misses. Any admission policy must
bound both missed work and latency, preserve OLTP progress and beat heap fallback
at matched load. Keep replay opt-in until those criteria hold. Longer soaks,
resource budgets and wider SQL support remain production gates.

Raw reports:

- [30 cycles/s, replay off](results/checkpoint-paced-30-off-2026-10-27.json)
- [30 cycles/s, replay on](results/checkpoint-paced-30-on-2026-10-27.json)
- [100 cycles/s, replay off](results/checkpoint-paced-100-off-2026-10-27.json)
- [100 cycles/s, replay on](results/checkpoint-paced-100-on-2026-10-27.json)
- [300 cycles/s, replay off](results/checkpoint-paced-300-off-2026-10-27.json)
- [300 cycles/s, replay on](results/checkpoint-paced-300-on-2026-10-27.json)

All reports share source fingerprint
`0a0a97c5d493c7ae386c5576ce0717f6f27d37a4b61e083ab55c533e52782b8d`.
Their extension and runner fingerprints agree. Matching source archives and
report/archive hash manifests are in [source-variants](results/source-variants/).
Each archive was checked against the final source bytes.
