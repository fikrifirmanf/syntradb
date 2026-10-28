# Checkpoint soak, 2026-10-27

**Result on 0.1.0: the 20-minute soak failed one limit.** No passing report was
written for that build. The full log is
`results/checkpoint-soak-20min-2026-10-27.FAILED.log`.

**Result on 0.2.0 (28 October): the same 20-minute soak passed every limit,** with
no missed writer arrivals. See [the rerun](#20-minute-rerun-on-020-2026-10-28).
The limits were not changed between the two runs.

Command: `bash scripts/checkpoint-soak.sh --recovery-checks true`, 1,200 seconds,
20 windows of 60 seconds, 30 offered cycles per second per worker, 10,000 source
rows, one laptop. A 240-second smoke run passed earlier, but it held the snapshot
for only two windows.

## Failed limit

Both writers missed 5,572 and 5,573 of 36,000 offered arrivals (15.5%). The limit
is 5%. Reader misses were 8 and 60 of 36,000.

Writer commits per window were stable at about 1,637 for windows 1–4. They then
fell as the snapshot stayed pinned: 933 in window 6, 577 in window 9 and 486 in
window 10. They returned to 1,626 in window 11, the first window after release,
and stayed at 1,636–1,637 afterwards.

| Window | Phase | Writer 0 commits | Chunk table | Oldest xmin age |
|---|---|---|---|---|
| 1 | retained | 1,637 | 11 MiB | 4,546 |
| 4 | retained | 1,637 | 39 MiB | 17,646 |
| 6 | retained | 933 | 62 MiB | 25,075 |
| 10 | retained | 486 | 97 MiB | 33,961 |
| 11 | released | 1,626 | 51 MiB | 5 |
| 13 | released | 1,637 | 6 MiB | 2 |

Appliers and the pruner slowed too (pruner 1,382 → 538 commits per window).
Vacuum ran its 12 cycles per window throughout and could not remove versions the
retained snapshot still needed.

## Diagnosis (2026-10-28)

A plain PostgreSQL model (one hot counter row, an event queue pruned from the
head, 10,000 source rows, an old repeatable-read snapshot, 100–140 tps for five
minutes) slowed only about 25%. The collapse needs something in SyntraDB itself.

A second, shorter diagnostic soak (600 s, 60 cycles per second per worker, with
per-table statistics added to each window) failed the same limit and reproduced
the shape, three times sooner. Writer commits per window: 3,269 (window 1),
2,676, 1,308, 972, 650 (window 5, last retained), then 3,265 after release.
Pruner commits fell from 2,876 to 174. Log:
`results/checkpoint-soak-diag-600s-rate60-2026-10-28.FAILED.log`.

What the statistics show:

- Index tuple fetches on `capture_heads` per writer commit rose from 63 to 150
  across the retained windows. The head row is updated by every writer, the
  pruner and the appliers, 96–99% of those as HOT updates. With the old snapshot
  pinned, page pruning cannot remove the dead versions, so each head lookup walks
  a growing chain.
- From window 2, sessions waited on `Lock:tuple` and `Lock:transactionid`, which
  are queues on a contended row. None waited before the snapshot had aged.
- Fetches on chunks and the source table stayed flat, so the growth is not in the
  checkpoint chunk reads.
- All of it ended in the first window after the snapshot was released.

This is a consistent explanation, not a profile of the backend. A later model run
did not support it; see the next steps.

## Re-diagnosis (2026-10-28)

Four steps on the real system, all at 60 cycles per second per worker with the same
held snapshot. Logs are under `results/`.

1. **Actor ablation.** Removing the readers and keyed monitor, the vacuum worker, or
   the second applier (new `--checkpoint-soak-skip` option) each collapsed writer
   commits the same way: about 3,270 per window falling to 500–1,000, then back to
   3,270 after release. The feed needs a pruner and an applier, so writers, one
   applier and the pruner are the smallest set tested. Ablated runs are
   diagnostic only and never write a report.
2. **CPU stacks** of the busiest backends in a degraded window: time in
   `heap_hot_search_buffer`, `HeapTupleSatisfiesVisibility`, `XidInMVCCSnapshot`
   and sequential scans over the source table, plus long waits.
3. **Session samples** (25 one-second snapshots at window 5): the applier's
   `syntra_apply_prune_capture` waits on a writer's transaction, and writers wait on
   a tuple lock behind it. Writers and the applier were blocked in 22 and 18 of 50
   observations each.
4. **Lock objects** (25 snapshots at window 5, 25 at window 9): during retention 41
   blocked sessions, 17 of them waiting on a tuple lock on `syntra.capture_heads`
   and the rest on transaction ids. After release, 3 blocked sessions and no
   tuple-lock waits on any catalog.

So on the real system the head row does form a lock queue that appears only while
the snapshot is held. That is observation, not yet a controlled cause: no
experiment has shown that removing the contention restores throughput.

Two PostgreSQL models of the same write path (head row with its foreign key, event
insert, pruner, pinned snapshot; one with 3 ms of client time inside each writer
transaction, one with the pruned cursor moved to its own row, one with slot rows)
did not reproduce the collapse. The held snapshot raised model latency 22% against
2% without it, and no variant was convincingly better. The model misses something in
the real path.

## Root cause, measured (2026-10-28)

At window 5 of another 720-second diagnostic run, one lookup of the head row by key
(`EXPLAIN (ANALYZE, BUFFERS)`, the unique index the planner chose) took:

| Phase | Time | Buffers touched | Head heap | Dead head versions |
|---|---|---|---|---|
| Snapshot held (window 5) | 5.0–5.4 ms | 2,235 | 18.3 MB | 91,225 |
| Released (window 9) | 0.008–0.014 ms | 2 | 82 KB | 285 |

PostgreSQL cannot prune a row version created and superseded after the oldest
running snapshot started, even though no session can see it. A current-snapshot
lookup of a row updated N times since then has to step over all N versions. The
head row takes roughly 100 updates per second here (both writers, pruner and
appliers), so its lookup cost grows linearly while the snapshot is held.

Each captured row looked the head row up about three times: the configuration read,
the counter update and the foreign-key check from the event insert. The stress
writers change two rows per statement. At 5 ms per lookup that is about 30 ms of
version walking per writer statement, against a 16.7 ms arrival interval, and the
row lock is held across it, which is the queue seen in the lock samples.

The plain-PostgreSQL models did two lookups per transaction on a single-row writer,
which is why they slowed only 22–28%.

Event range scans are not affected: they start above the pruned cursor and never
visit deleted events. Log and plans: `results/checkpoint-soak-head-lookup-explain-2026-10-28.txt`.

## Fixes measured (2026-10-28)

Two changes, each run through the same 720-second diagnostic at 60 cycles per
second with the snapshot held for the first six windows:

1. **Bindings.** Writers read their configuration from a new, never-updated
   `capture_bindings` row, and child foreign keys point at it. That removes two of
   the three head lookups per captured row.
2. **Marks.** Writers lock the head row (`FOR NO KEY UPDATE`) instead of updating
   it, and append one `capture_marks` row per trigger call. The head row now
   changes only when the pruner folds marks into it. See
   [transactional capture](../docs/transactional-capture.md#base-boundary-and-write-contract).

Committed writer cycles per 60-second window, both writers, snapshot held in
windows 1–6:

| Window | Before | Bindings | Bindings + marks |
|---|---|---|---|
| 1 | 6,546 | 6,336 | 6,546 |
| 2 | 5,742 | 4,730 | 6,519 |
| 3 | 3,344 | 2,810 | 5,416 |
| 4 | 1,950 | 2,154 | 4,676 |
| 5 | 1,394 | 1,714 | 3,922 |
| 6 | 1,168 | 1,718 | 3,143 |
| 7–12, released | about 6,540 | about 6,540 | 6,458–6,546 |

| Measure | Before | Bindings | Bindings + marks |
|---|---|---|---|
| Writer missed arrivals, whole run | 26.4% | 25.2% | 11.7% |
| Writer scheduled p99 | 108 ms | 89 ms | 50 ms |
| Repeatable-read reader missed | 2.9% | 8.3% | 10.8% |
| Head lookup at window 5 | 5.0–5.4 ms | 6.6–7.8 ms | 0.30–0.45 ms |
| Head heap at window 5 | 18.3 MB | 18.6 MB | 0.87 MB |

The "before" column is the run that measured the root cause; its writer counts
are the per-writer figures doubled. With marks, the newest-mark lookup took
0.05–0.09 ms while the snapshot was held. Fetches per writer commit stayed flat
on the head (12–14), marks (13–21) and events tables. On the source table they
rose from about 5,200 to 7,800 while the snapshot was held, and the
repeatable-read reader, which scans the whole source every cycle as its oracle,
slowed with it.

All three runs still fail the 5% limit. Marks halve the loss but writers still
lose about half their throughput by window 6. Logs:
`results/checkpoint-soak-diag-720s-rate60-bindings-2026-10-28.FAILED.log`,
`results/checkpoint-soak-diag-720s-rate60-marks-2026-10-28.FAILED.log`, and plans
in `results/checkpoint-soak-{bindings,marks}-head-lookup-explain-2026-10-28.txt`.

### Ablation with marks: the pruner

The same run with the readers and keyed monitor removed (diagnostic only, log
`results/checkpoint-soak-ablation-noreaders-marks-2026-10-28.log`) held writers at
6,493–6,531 commits for windows 1–4, then fell to 5,632 and 4,432. The pruner fell
first: 2,455–2,774 commits per window, then 586 and 172.

Cause: the pruner's delete of folded marks had no lower bound (`ordinal <= cut`). Marks
below the old cut were already deleted, but a held snapshot keeps them, so each
prune stepped over every mark deleted since the snapshot began: 136,000 dead
marks at the end of window 4 and 166,000 at the end of window 5. The delete runs in the statement that updates the head row, so writers
waiting to lock that row queued behind it. The event delete already started above
the old cut. 0.2.0 now bounds the marks delete the same way (`ordinal > old cut`);
the result is identical because no mark at or below the old cut survives a prune.

### Bounded mark delete: the diagnostic run passes

The full 720-second diagnostic, all actors, after bounding the mark delete. The
harness also vacuumed `capture_marks` for the first time in this run, so the gain
cannot be split between the two changes. Log and report:
`results/checkpoint-soak-diag-720s-rate60-markbound-2026-10-28.{log,json}`.

| Window | Oldest xmin age | Writer commits | Pruner commits |
|---|---|---|---|
| 1 | 8,170 | 6,546 | 3,106 |
| 2 | 16,462 | 6,546 | 3,020 |
| 3 | 24,738 | 6,546 | 3,007 |
| 4 | 33,211 | 6,502 | 2,839 |
| 5 | 41,863 | 5,343 | 1,105 |
| 6 | 49,839 | 4,801 | 251 |
| 7–12, released | 0–5 | 6,540–6,547 | 3,145–3,204 |

Every limit passed: writers missed 1,647 of 43,200 arrivals each (3.8%, limit 5%),
scheduled p99 38 ms, repeatable-read reader missed 3.3%, read-committed reader 0,
no invariant failures or unexpected errors, WAL 8,732 bytes per committed write.

This is the shorter diagnostic, not the 20-minute gate. It holds the snapshot for
six minutes at 60 cycles per second. Writers still lost 18–27% in windows 5 and 6.
The pruner's drop is mostly lost admission: it took `lock_unavailable` 10,811
times while the appliers, which also prune, kept pending events at 12–24.

A repeat on the same build, with plans and session samples taken in window 5
(`results/checkpoint-soak-diag-720s-rate60-markbound-repeat-2026-10-28.{log,json}`,
`results/checkpoint-soak-markbound-window5-explain-2026-10-28.txt`), also passed.
Writers missed 360 of 43,200 arrivals each (0.8%), held 6,544–6,546 commits
through window 5 at age 40,929, and fell to 5,894 in window 6. Variance between
the two runs is large, so neither is a typical figure.

| Lookup at window 5 | Held snapshot | Released |
|---|---|---|
| Checkpoint row by key | 0.06 ms, 28 buffers | 0.015 ms, 2 buffers |
| Head row by key | 0.08 ms, 53 buffers | 0.012 ms, 2 buffers |
| Chunk row by `(source, partition)` | 1.5–1.7 ms, 1,717 buffers | 0.021 ms, 6 buffers |

The checkpoint row is not a hot spot. The chunk primary key is. Readers look
chunks up by exact revision, but each apply upserts dirty partitions through
`(source, partition)`, and each partition keeps every version since the snapshot.
That costs appliers, not writers. Across 20 one-second samples of active sessions,
writer sessions appeared 8 times waiting on another writer's transaction (the
head-row lock that keeps ordinals gap-free) and 16 times on CPU. Harness oracle
reads (reader summaries and the keyed monitor's full source scan) appeared 24
times.

The same build with the readers and keyed monitor removed (diagnostic only,
`results/checkpoint-soak-ablation-noreaders-markbound-2026-10-28.log`) kept writers
at 6,545–6,546 commits in every window, up to an oldest xmin age of 51,052, and
the pruner at 3,102–3,199. Before the mark-delete bound the same ablation fell to
4,432. So the loss left in full runs comes with the reader load: the product's
checkpoint reads plus the harness's oracle scans of the source. This run does not
split the two.

## Limits that passed on 0.1.0

| Measure | Result | Limit |
|---|---|---|
| Catalog bytes, final vs first released window | 13.8 MB vs 61.9 MB | 94.2 MB |
| Oldest xmin age, before release → final | 33,961 → 0 | final below before |
| Released-phase RSS growth | negative on every worker (−62 to −115 MiB) | 64 MiB |
| WAL per committed write | 10,056 B (556 MB total, 55,323 writes) | 65,536 B |
| Scheduled p99, readers | 4.9 ms, 29.9 ms | 1,000 ms |
| Scheduled p99, writers | 142.7 ms, 143.5 ms | 1,000 ms |
| Peak database + WAL | 453 MB | 2,048 MB |
| Pending/retained events | within 4,096 | 4,096 |

RSS fell because the first released window still held pages touched during the
retained phase; the limit only catches growth.

The oracle checks, retained-snapshot verification at the midpoint, final drain and
recovery checks completed before the verdict. The run then failed on service level.

## 20-minute rerun on 0.2.0 (2026-10-28)

Same command, `bash scripts/checkpoint-soak.sh --recovery-checks true`, same
limits, same laptop. Report and log:
`results/checkpoint-soak-20min-2026-10-28.{json,log}`. It ran the release build
of the bounded mark delete, source fingerprint `5740591f…`.

Every limit passed. Writers and both readers missed no arrivals, out of 36,000
each. Writer commits per window stayed at 1,635–1,638 in all 20 windows,
including the ten with the snapshot held. In the 0.1.0 run they had fallen to 486.

| Window | Phase | Writer 0 commits | Oldest xmin age |
|---|---|---|---|
| 1 | retained | 1,637 | 4,660 |
| 5 | retained | 1,636 | 21,768 |
| 10 | retained | 1,636 | 42,872 |
| 11 | released | 1,637 | 15 |
| 20 | released | 1,636 | 4 |

| Measure | 0.1.0 | 0.2.0 | Limit |
|---|---|---|---|
| Writer missed arrivals | 15.5% | 0 | 5% |
| Scheduled p99, writers | 143 ms | 15.1 ms | 1,000 ms |
| Scheduled p99, readers (read committed, repeatable read) | 4.9 ms, 29.9 ms | 4.8 ms, 16.1 ms | 1,000 ms |
| Catalog bytes, final vs first released window | 13.8 MB vs 61.9 MB | 22.4 MB vs 60.9 MB | 1.25 × first + 16 MiB |
| WAL per committed write | 10,056 B | 9,467 B | 65,536 B |
| Peak database + WAL | 453 MB | 465 MB | 2,048 MB |
| Released-phase RSS growth, worst worker | negative | 1.6 MiB | 64 MiB |

No invariant failures or unexpected errors. Retained-snapshot verification, final
drain and the recovery checks (process crash, logical restore, checkpoint retry)
all passed before the verdict.

### Repeat run

A second run of the same command passed every limit too. It ran a later
workspace, fingerprint `6c26833e…`, which adds `syntra_capture_lag` and the
opt-in maintenance worker (not enabled in the soak). Report and log:
`results/checkpoint-soak-20min-repeat-2026-10-28.{json,log}`.

| Measure | First run | Repeat |
|---|---|---|
| Missed arrivals, writers and readers | 0 | 0 |
| Writer 0 commits per window | 1,635–1,638 | 1,635–1,637 |
| Oldest xmin age before release | 42,872 | 43,124 |
| Scheduled p99, writers | 15.1 ms | 15.1 ms |
| Scheduled p99, readers | 4.8 ms, 16.1 ms | 4.8 ms, 16.1 ms |
| Catalog bytes, final vs first released window | 22.4 MB vs 60.9 MB | 22.7 MB vs 63.0 MB |
| WAL per committed write | 9,467 B | 9,663 B |
| Peak database + WAL | 465 MB | 470 MB |

All 26 recovery checks were true; no invariant failures or unexpected errors.

What it does not show: two runs on one laptop, at 30 cycles per second per worker
with 10,000 source rows. Snapshot age reached 42,872. In the 60-cycle
diagnostics writers started to slow at ages near 41,000–50,000 while readers ran,
and stayed flat to 51,000 without them. Longer holds, higher rates and other
hardware are untested.

## What this shows

- Storage and memory recover fully after the old snapshot ends.
- On 0.1.0, while an old snapshot was pinned, write throughput fell roughly 70%
  over ten minutes at this load. On 0.2.0 it held at this load and snapshot age.
- The smoke run could not show this. Windows of 60 seconds over ten minutes can.

## Next

1. Keep the snapshot age policy. `syntra_cleanup_horizon_age()` reports the age
   and `docs/resource-bounds.md` sets alert and page thresholds. The 0.2.0 runs
   would justify higher thresholds on this laptop only.
2. Chunk upserts look rows up by `(source, partition)`, which walks every version
   kept by the snapshot (1.6 ms at window 5 of the diagnostic). Appliers pay it.
   Revisit if a longer hold shows appliers falling behind.
3. Repeat the 20-minute soak on other hardware, and at a higher rate, before
   treating one pass as typical.
