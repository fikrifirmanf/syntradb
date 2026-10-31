# Held-snapshot CPU profiles on Linux

The [two-hour diagnostic run](https://github.com/fikrifirmanf/syntradb/actions/runs/41209482855)
failed three offered-arrival gates at `05d4c14`. The original limits stayed in
place. Final apply/prune, keyed comparison, checkpoint reuse and vacuum passed.
The [versioned evidence](evidence/profiled-soak-linux-2026-10-31.json) preserves
the verdict, all 120 windows, worker counters and profile summaries.

| Worker | Missed / offered | Missed | Scheduled p99 |
|---|---|---|---|
| Writer 0 | 46,939 / 216,000 | 21.73% | 152.703 ms |
| Writer 1 | 46,737 / 216,000 | 21.64% | 152.959 ms |
| Read committed | 2 / 216,000 | 0.001% | 27.423 ms |
| Repeatable read | 60,017 / 216,000 | 27.79% | 166.271 ms |

The missed-arrival ceiling remains 5%; the p99 ceiling remains 1,000 ms.
Resource gates passed: 7,577 WAL bytes per committed writer cycle, about 708 MiB peak
database plus WAL, and final catalog bytes below the release-period ceiling.
The oldest snapshot reached age 187,554 before release and cleared afterward.

## What the stacks show

All four scheduled CPU windows completed, with 2,227, 2,575, 2,695 and 933
samples at minutes 8, 28, 58 and 65. None reported lost samples. PostgreSQL and
SyntraDB symbols are readable. Some libc and vDSO frames remain unnamed, and
some C-to-Rust guard boundaries truncate their parent chains. These are
userspace samples of the nine client workers, excluding background workers.

At minute 58 the repeatable-read backend contributed 629 samples. The native
`SeqNext` path accounts for about 19.85% of all sampled CPU, roughly 535 samples.
Its stacks lead through `ExecAgg` and the top-level query executor. The read
statement runs a native count/sum/min/max beside each SyntraDB result, nine
times per repeatable-read cycle. That verification scans retained source row
versions. SyntraDB's fresh heap fallback uses its own table scan, so `SeqNext`
does not identify that fallback path.

Writer 0 contributed 576 samples at minute 58. B-tree insertion's inclusive
share was about 11.13% of global CPU, roughly 300 samples. Visible parent
chains lead through source insert/update execution; other samples lead through
capture's SPI and catalog calls. Capture overhead remains present. These
profiles do not establish an exact split across every guard boundary.

The two appliers together contributed 69 samples at minute 58. Checkpoint
maintenance was not the dominant sampled CPU consumer in this run. This is
evidence against choosing a checkpoint-layout rewrite as the next throughput
fix. It does not prove that every maintenance path scales.

Inclusive percentages overlap and must not be added. They use all worker CPU
as their denominator, not wall time or a single backend's time.

## Release of the old snapshot

In minute 60, writers committed 463 and 480 cycles, while repeatable read
committed 446. In the first release minute those counts recovered to 1,616,
1,616 and 1,783. At minute 65 they reached 1,636, 1,636 and 1,800. The writer's
intentional rollback schedule means 1,636 committed cycles can represent its
full offered rate.

This supports retained PostgreSQL row versions as a major cost. The benchmark
combines native source writes, native verification and extension work; a failed
combined throughput gate cannot assign all of that cost to SyntraDB. A matched
native control would be needed to quantify capture overhead under this history.
The earlier [writer controls](held-snapshot-writers-2026-10-29.md) provide context,
but ran on another platform and an older extension version.

No gate was relaxed, no native oracle was removed, and no throughput fix is
claimed. Profiling adds overhead and the earlier unprofiled run used another
host, so the difference between their missed-arrival percentages is not a
regression measurement. The load gate stays open while packaging and other
independent readiness work proceeds.
