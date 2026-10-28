# Reader-tail findings, 27 October 2026

Experimental opt-in reader replay now answers eligible lagging-checkpoint summaries without
publishing state. All 218 tests pass: 66 engine, 33 harness and 119 PostgreSQL
tests. Fourteen new committed-session scenarios verify the public read-only path,
permissions, old snapshots, extrema, resource limits and safe fallback.

The isolated two-event replay took 0.101 ms median versus native SQL's 0.324 ms
on 10,000 warm rows. A 256-event replay took 0.205 ms versus 0.353 ms. The concurrent
run eliminated ordinary-reader heap fallback, but reader tail latency remained
worse than the earlier heap-fallback run. The warm result does not establish a
mixed-workload speedup or production readiness.

Replay requires the three-argument overload with `replay_tail = true`. Existing
two-argument calls and an explicit false argument retain heap fallback on lag.
The measured regression therefore remains behind an explicit opt-in. The stress
report records `reader_tail_opt_in: true`.

## Isolated costs

The cost probe used release Rust binaries and assertion-disabled PostgreSQL 18.6
on macOS ARM64. Each case has 51 prepared requests after an excluded warmup.
Source setup stays outside timing. Two-event setup transfers a balance between
two keys; 256- and 257-event setup repeatedly updates one key. Every API result
must match native SQL and its expected route. Builds and tests finished before
timing began.

| Operation | Median request, ms | p95 request, ms | Median backend WAL, bytes |
|---|---:|---:|---:|
| `native_summary` | 0.322666 | 0.330042 | 0 |
| `checkpoint_summary` | 0.101334 | 0.108292 | 0 |
| `native_two_events` | 0.324083 | 0.337000 | 0 |
| `replay_two_events` | 0.101000 | 0.110459 | 0 |
| `native_256_events` | 0.352541 | 0.412708 | 510 |
| `native_257_events` | 0.355416 | 0.421792 | 510 |
| `replay_256_events` | 0.205166 | 0.258667 | 956 |
| `heap_257_events` | 0.504667 | 0.681083 | 1,320 |
| `apply_two_events` | 0.187625 | 0.215750 | 9,443 |
| `apply_unchanged` | 0.297625 | 0.952416 | 0 |
| `prune_two_events` | 0.671542 | 1.129292 | 245 |

Two-event replay generated zero backend WAL in every instrumented sample. The
256-event replay and 257-event heap fallback generated nonzero WAL. Native SQL
controls after the same update counts also generated WAL. PostgreSQL page
maintenance can write WAL during reads. These counters do not represent logical
checkpoint publication. Separate committed checks verify no assigned XID and no
change to visible checkpoint, chunk or event tuple identities and progress.

Read timing now includes its first capture-head access. Only apply cases query
an expected cursor before timing. This corrects an earlier setup step that could
move head-page maintenance outside the measured read. EXPLAIN runs use separate
setup and include their own backend WAL; they exclude the outer commit record.

## Indexed event intervals

An initial implementation scanned events by source relation. A retained old
snapshot kept deleted prefixes from being reclaimed. That implementation passed
its logical checks but had slow concurrent readers. The final reader uses the
existing `(epoch, ordinal)` primary-key index, bounded after its checkpoint cursor
and through the visible head. Required ordinals must still be contiguous. Missing
already-applied history is irrelevant to reconstructing the pending suffix.

Admin apply uses the same index while checking its full retained interval.
Prune now deletes only between its previous pruned cursor and durable applied
cursor. The head update requires the expected old pruning boundary. Older deleted prefixes no longer belong to that delete range. The final prune
median was 0.672 ms after heavy churn; timings vary between these local runs.

The earlier source-scan reports and matching source archives are retained for
inspection. Range bounds fix unnecessary event-prefix work, but do not establish
that all retained-version lookup costs have been resolved.

## Concurrent run and remaining regression

The 60-second opt-in run passed 30,205 worker aggregate comparisons and
6,187 keyed reconstructions. It committed 6,034 writer transactions and emitted
36,204 events. The independent pruner deleted 10,866 events. Writers and appliers
also completed intentional rollbacks. The initial checkpoint stayed valid in a
snapshot held throughout. Final catch-up left zero retained events and passed
another keyed reconstruction and checkpoint read.

Ordinary readers completed 29,991 checks: 28,153 used bounded replay and 1,838 used
a current checkpoint. None used the heap. Writers separately verified own-XID
heap routing. Sampled pending intervals peaked at 78 events, below the 256-event
replay limit. Node WAL was 120,545,600 bytes. Allocated checkpoint storage at
worker stop was 56,983,552 bytes, including manifests and partitions.

| Reader cycle | Earlier heap-fallback p99, ms | Reader-tail p99, ms |
|---|---:|---:|
| Read committed | 4.227 | 9.127 |
| Repeatable read, nine checks | 39.455 | 76.031 |

This is an observed regression in saturated local runs, without confidence
intervals. The workload includes native verification, competing maintenance,
VACUUM and a long-lived snapshot. It is not an isolated query-latency comparison
or a paced OLTP benchmark. Remaining partition/catalog MVCC lookup work is a
profiling hypothesis, not an attributed root cause. Do not raise replay limits
or claim a broad speedup from eliminating heap fallback alone.

## Reproduction and evidence

```sh
bash scripts/benchmark-production.sh --mixed-seconds 0 --rows 10000 \
  --repetitions 51 --recovery-checks true --refresh-cost-checks true \
  --output benchmarks/results/reader-tail-costs-2026-10-27.json
bash scripts/checkpoint-stress.sh --recovery-checks true \
  --output benchmarks/results/reader-tail-stress-60s-2026-10-27.json
```

- [Final cost report](results/reader-tail-costs-2026-10-27.json)
- [Final stress report](results/reader-tail-stress-60s-2026-10-27.json)
- [Earlier heap-fallback stress](results/chunked-checkpoint-stress-60s-2026-10-27.json)
- [Initial source-scan costs](results/reader-tail-source-scan-costs-2026-10-27.json)
- [Initial source-scan stress](results/reader-tail-source-scan-stress-60s-2026-10-27.json)
- [Cost source archive](results/source-variants/reader-tail-costs-2026-10-27.tar.gz)
- [Cost source manifest](results/source-variants/reader-tail-costs-2026-10-27.manifest.json)
- [Stress source archive](results/source-variants/reader-tail-stress-60s-2026-10-27.tar.gz)
- [Stress source manifest](results/source-variants/reader-tail-stress-60s-2026-10-27.manifest.json)

Both final reports match source fingerprint
`7116ef9cc9da3b059c1ae891d4af8e400b64b0d619cb55763b3fd3aa3fe60f36`.
Independent Python hashing verified all 87 archived source inputs against both
embedded binary fingerprints. The initial source-scan variants retain their own
matching archives and manifests in the same directory.

Mutations to event, row and byte limits failed focused tests. Removing replay
failed its repeated-key test and a source-specific committed oracle before timing.
Changing the indexed lower bound and the pruning cursor argument also failed their
focused tests. A default-entry mutation that enabled replay implicitly also failed
its compatibility gate before timing. Original code was restored before the final runs. Process-crash
and logical-restore gates passed, including reading a surviving committed suffix
before applying it again.

Next, profile catalog lookup and retained versions under the held snapshot, then
measure paced mixed load. Evaluate reader admission using total cost, alongside
bounded retry scheduling for competing maintenance. Keep the opt-in, heap fallback, own-XID
protection and fixed replay limits. Automatic workers, planner routing, broader
SQL scope, long soaks, release upgrades and failover remain open.
