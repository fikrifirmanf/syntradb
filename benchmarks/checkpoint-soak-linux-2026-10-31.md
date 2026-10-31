# Native Linux two-hour soak: offered-load failure

The two-hour 0.7.0 checkpoint soak at commit `7b3609f` failed its service limits.
Both writers missed about 17.1% of offered cycles; the repeatable-read worker
missed 23.5%. The allowed fraction is 5%. The read-committed worker missed two
cycles out of 216,000.

[GitHub run 41133224378](https://github.com/fikrifirmanf/syntradb/actions/runs/41133224378)
used an Ubuntu 24.04 standard runner with four vCPUs on an AMD EPYC 9V74 host.
It ran the production harness for 7,200 seconds with 60-second samples and
30 offered cycles per worker per second. An old snapshot was retained for the
first hour, then released. This is one run, not a matched comparison with 0.6.0.

## Service results

| Worker | Planned cycles | Missed cycles | Missed | Scheduled p99 |
|---|---:|---:|---:|---:|
| writer 0 | 216,000 | 36,987 | 17.12% | 129.3 ms |
| writer 1 | 216,000 | 36,929 | 17.10% | 128.5 ms |
| read committed | 216,000 | 2 | 0.001% | 24.6 ms |
| repeatable read | 216,000 | 50,801 | 23.52% | 151.7 ms |

All four workers passed the 1,000 ms scheduled p99 ceiling. Both writers and
the repeatable-read worker failed the missed-arrival ceiling. A cycle can include several statements and
checks. The repeatable-read worker performs more checks per cycle than the
read-committed worker; their service times are not equivalent query timings.

## Retention and recovery

| Minute | Writer commits/min, mean of two | Repeatable-read commits/min | Pruner commits/min | Manifest size | Horizon age |
|---|---:|---:|---:|---:|---:|
| 1 | 1,637 | 1,800 | 1,357 | 5.5 MiB | 5,057 |
| 30 | 979 | 776 | 1,005 | 175.9 MiB | 138,333 |
| 60 | 568.5 | 501 | 1,247 | 289.7 MiB | 216,454 |
| 61 | 1,610.5 | 1,783 | 1,299 | 290.4 MiB | 3 |
| 65 | 1,636 | 1,800 | 1,304 | 8.2 MiB | 3 |
| 120 | 1,636.5 | 1,800 | 1,376 | 8.1 MiB | 0 |

Writer commit counts exclude intentional rollbacks. At the offered rate, a
fully serviced writer commits about 1,636 transactions per minute and rolls
back the remaining cycles by design.

Writers and the repeatable-read worker slowed as the retained horizon aged.
The pruner kept committing throughout. Service recovered within the first full
minute after release. That pattern is consistent with retained-version costs;
it does not identify which relation or call stack dominates them. Manifest growth
is a profiling lead, not proof that it causes the writer slowdown.

Combined database and WAL storage peaked at about 800.4 MiB, below the 2 GiB
budget. WAL was 7,628 bytes per committed writer transaction, below the 64 KiB
ceiling. The sampled cleanup horizon advanced after release, catalog storage
stopped growing and the released-phase RSS limit passed.

Concurrent correctness checks and retained-snapshot verification reached the end
of the run without a reported invariant failure. The service-limit evaluation
then failed. Final quiescent apply/prune, keyed comparison and checkpoint-reuse
assertions occur after that evaluation and did not run. This is not a complete
passing correctness or production-readiness result.

## Evidence

The [versioned evidence](evidence/checkpoint-soak-linux-070-2026-10-31.json)
contains all 120 windows, limits, measured resource values and worker counts and
latency summaries. Histogram buckets are omitted. It records the source commit,
runner image and raw log SHA-256. The full log remains in the GitHub artifact
for its configured retention period and in the local downloaded results.

The benchmark wrote no passing JSON report. The workflow correctly failed,
unlike the earlier invalid run whose logging pipeline hid an error before the
soak began. Keep the throughput gate open and profile the retained phase before
changing the storage path or claiming an improvement.
