# Checkpoint soak

```sh
bash scripts/checkpoint-soak.sh --recovery-checks true
```

The soak reuses the [checkpoint stress](checkpoint-concurrency-stress.md) workers
and oracles for longer, paced runs. The script defaults to 1,200 seconds,
60-second windows, 30 offered cycles per second per worker, 10,000 source rows
and the combined apply/prune API. Later arguments override these defaults.

`--checkpoint-soak-seconds` accepts 240–14,400 seconds. It needs an explicit
`--checkpoint-worker-rate` of 10–1,000 and cannot be combined with
`--checkpoint-stress-seconds`. The duration must be an even number, at least
four, of whole `--checkpoint-soak-window-seconds` windows (10–600 seconds).
Saturated soaks are rejected: a 60-second saturated run already allocated up to
344 MB of checkpoint storage, and local disk is finite.

## Workload

The eight stress workers run unchanged: two writers, read-committed and
repeatable-read readers, two appliers, an independent pruner and the keyed
monitor. All aggregate, route, keyed-reconstruction, retention and rollback
checks remain active. A ninth worker runs `VACUUM (ANALYZE)` on the capture
events, marks, chunks, manifests and heads and on the source table every five
seconds. The first two runs with marks, both on 2026-10-28, did not vacuum the
marks table; autovacuum cleaned it on its own schedule.
The monitor's own periodic vacuum is turned off in soaks so the two cannot contend.

An old repeatable-read snapshot is pinned before workers start and held for the
first half. At the midpoint it must still read the original checkpoint and match
its first native answer. It is then committed. The second half shows whether
vacuum recovers once the oldest snapshot is gone.

## Samples

At every window boundary the coordinator records:

- committed progress per worker since the previous window;
- database and WAL directory bytes, and the current WAL position;
- catalog bytes for chunks, manifests, events, heads and marks, plus source bytes;
- dead tuples and autovacuum runs for those relations;
- the oldest backend xmin age, excluding the coordinator;
- pending and retained events;
- each worker backend's RSS, read with `ps`.

Each sample is printed as one JSON line, so data survives a failed run.

## Pass/fail limits

Checked at each window:

- every worker, including vacuum, commits at least once;
- pending and retained events stay within the 4,096-event budget;
- database plus WAL bytes stay within `--checkpoint-soak-disk-budget-mb`
  (default 2,048).

Checked at the end:

- final catalog bytes are at most 125% of the first fully released window plus
  16 MiB;
- the final oldest xmin age is below its age just before the release;
- no worker backend's RSS grows by more than 64 MiB from the first fully released
  window to the end;
- WAL per committed writer transaction is at most 64 KiB;
- writer and reader scheduled p99 is at most 1,000 ms, and at most 5% of their
  offered arrivals are missed.

These are regression ceilings, not service levels. Earlier paced 30-second runs
stayed well below each one. RSS includes shared buffers the backend has touched,
which is why only released windows, after warm-up, are compared. Every existing
stress gate also applies: accounting of emitted and pruned events, intentional
rollbacks, independent pruning, final drain and quiescent checkpoint reuse.

Violating any limit fails the run without writing a passing report. The report's
`checkpoint_stress.soak` object lists every window, each limit and its measured
value.

## Layouts and recorded runs

`--checkpoint-column-type int4|int8|numeric` picks the feed layout (default
`int4`). In `int8` mode the key and value columns are `bigint` and every value is
above 5,000,000,000, so a width that truncates to 32 bits fails the aggregate and
keyed checks. In `numeric` mode the key is `int4` and the value `numeric(18,2)`,
seeded at 50,000,000.37 and up in steps of 0.37. Transfers move 0.01 and a moved
key gets 0.07, so the harness works in hundredths, the stored unit. The keyed
monitor decodes 8-byte values and compares them with `(v*100)::int8` from the
heap. A 20-minute soak per run, 9 workers, 30 offered cycles per second per
worker, one laptop, workspace 0.3.0, 2026-10-28:

| Layout | Run | Writer missed arrivals | Writer p50 / p99 | Writer commits in the worst 60 s window (of 1,637 possible) |
|---|---|---|---|---|
| int4 | `checkpoint-soak-20min-int4-030` | 17 and 19 of 36,000 | 4.5 / 14.0 ms | 1,626 |
| int8 | `checkpoint-soak-20min-int8-repeat` | 64 and 63 | 4.1 / 16.3 ms | 1,580 |
| int8 | `checkpoint-soak-20min-int8` | 602 and 609 | 4.5 / 50.9 ms | 1,410 |

All three passed every limit above, with no unexpected errors and no invariant
failures. The two 0.2.0 `int4` runs recorded no missed arrivals.

What the runs do not settle:

- The first `int8` run had tails three to four times worse than the other two,
  with dips across minutes 6 to 14. The repeat did not reproduce them. The cause
  was not identified; it may be other activity on the laptop, but nothing
  measured shows that.
- Both `int8` runs dipped in the window that ends at the 600 s midpoint, where
  the soak releases the retained snapshot. The `int4` run did not. Two runs show
  a pattern, not a cause.
- Three runs do not support "int8 costs the same as int4". They support "int8
  passes the soak limits on this laptop".

### 0.4.0 and a control, 2026-10-28

The same `int4` soak, later the same day:

| Run | Tree | Writer missed arrivals | Writer p50 / p99 | Worst 60 s window |
|---|---|---|---|---|
| `checkpoint-soak-20min-int4-040` | 0.4.0, first admission retry | 1,521 and 1,533 | 4.2 / 61.3 ms | 1,218 |
| `checkpoint-soak-20min-int4-040-probe` | 0.4.0, current | 1,148 and 1,139 | 4.4 / 56.5 ms | 1,189 |
| `checkpoint-soak-20min-int4-030-probe-control` | 0.3.0 baseline plus the current admission retry | 632 and 644 | 4.4 / 44.6 ms | 1,446 |
| `checkpoint-soak-20min-int4-040-probe-repeat` | 0.4.0, current | 27 and 33 | 4.4 / 15.0 ms | 1,622 |
| `checkpoint-soak-20min-int4-030-probe-control-repeat` | 0.3.0 baseline plus the current admission retry | 22 and 19 | 4.7 / 18.9 ms | 1,621 |

All five passed every limit. Each pair ran back to back. The control differs from the current tree only by
the partition delta work. A first 0.4.0 attempt stopped in the soak's own keyed
monitor (`...040-2026-10-28.FAILED-keyed-monitor.log`): the monitor decoded base
bytes and expected them to hold the current revision. It now checks each delta's
digest, length, binding and base and merges it before comparing with the heap; a
monitor that skips the merge fails the stress run.

- The first 0.4.0 run used a retry that also waited for concurrent maintenance
  callers. That run is not comparable on maintenance; see
  [admission and vacuum](resource-bounds.md#admission-and-vacuum).
- In the first pair, both trees slowed only while the old snapshot was held, in
  minutes 4 to 10, and recovered after its release. In the repeat pair neither
  did, and both matched the morning's 17 and 19 misses. Whatever slowed the first
  pair affected the tree without deltas as well; its cause was not identified.
- Across the two pairs, 0.4.0 and the control are within each other's spread.
  Two runs per tree show no latency cost from the deltas in this soak; they do
  not rule out a small one.

### numeric, 2026-10-28

Thirteen 20-minute `numeric(18,2)` soaks on 0.4.0, same settings, and two `int4`
soaks run between two of them for comparison:

| Run | Result | Writer missed arrivals | Writer p50 / p99 | Worst 60 s window |
|---|---|---|---|---|
| `checkpoint-soak-20min-numeric-040` | failed at about 720 s | – | – | – |
| `checkpoint-soak-20min-numeric-040-rerun` | passed | 603 and 595 | 2.9 / 46.3 ms | 1,438 |
| `checkpoint-soak-20min-numeric-040-third` | passed | 375 and 367 | 2.7 / 37.9 ms | 1,502 |
| `checkpoint-soak-20min-numeric-040-r4` | passed | 703 and 697 | 2.8 / 54.2 ms | 1,315 |
| `checkpoint-soak-20min-int4-040-diag-r1` | passed | 15 and 13 | 4.4 / 13.6 ms | 1,629 |
| `checkpoint-soak-20min-numeric-040-r5` | passed | 392 and 387 | 2.8 / 44.9 ms | 1,355 |
| `checkpoint-soak-20min-int4-040-diag-r2` | passed | 8 and 11 | 4.2 / 13.3 ms | 1,630 |
| `checkpoint-soak-20min-numeric-040-steps` (2026-10-29) | passed | 8 and 11 | 2.8 / 17.8 ms | 1,631 |
| `checkpoint-soak-20min-numeric-040-r6` (2026-10-29) | passed | 28 and 29 | 2.6 / 13.7 ms | 1,611 |
| `checkpoint-soak-20min-numeric-040-r7` (2026-10-29) | passed | 0 and 0 | 2.6 / 11.4 ms | 1,636 |
| `checkpoint-soak-20min-numeric-040-r8` (2026-10-29) | passed | 0 and 0 | 2.6 / 11.3 ms | 1,636 |
| `checkpoint-soak-20min-numeric-040-r9` (2026-10-29) | passed | 0 and 0 | 2.6 / 12.4 ms | 1,635 |
| `checkpoint-soak-20min-numeric-040-r10` (2026-10-29) | passed | 0 and 0 | 2.6 / 11.3 ms | 1,635 |
| `checkpoint-soak-20min-numeric-040-r11` (2026-10-29) | passed | 0 and 0 | 2.6 / 11.2 ms | 1,635 |
| `checkpoint-soak-20min-numeric-040-r12` (2026-10-29) | passed | 0 and 0 | 2.6 / 11.3 ms | 1,635 |

The passing runs had no unexpected errors and no invariant failures, with
35,523 to 36,000 keyed checks each. Those that slowed did so while the old
snapshot was held and recovered after its release, like the first 0.4.0 `int4`
pair above.

The first run stopped with `pruner: db error`, two minutes after the snapshot
was released (`...numeric-040-2026-10-28.FAILED-pruner-db-error.log`). Its cause
is not known. The harness then printed only the first failing worker in join
order, without PostgreSQL's code or message, and removed the server log with the
cluster. It now prints every worker's error with SQLSTATE, message and detail,
and the last 60 lines of the server log on failure; a forced failure showed both.
None of the fourteen later runs reproduced the error, including seven `numeric`
runs made only to catch it with the new diagnostics. One failure in fifteen runs
with no recorded cause is open, not resolved.

In the first four passing `numeric` runs, writers missed 367 to 703 arrivals
with a p99 of 38 to 54 ms, against 8 to 15 and 13 to 14 ms for the two `int4`
runs. The dips came in minutes 8 to 10, when the held snapshot had left 100,000
to 130,000 dead rows in the source. The sixth `numeric` run reached the same
131,092 dead rows with no dip: 8 and 11 missed arrivals, a 17.8 ms p99. Of the
seven runs on 2026-10-29 after it, six missed no arrivals and one missed 28 and
29, with p99s of 11 to 14 ms. These runs do not show that `numeric` costs more; like the 0.4.0 `int4` pairs above,
the soak varies from run to run on this laptop for reasons not identified.

The sixth run timed each writer statement. Transfers, which update two rows,
had the highest p99 (10.9 ms) and maximum (245 ms); commits stayed under 0.35 ms
at p99. Heap-route reads of the writer's own uncommitted change had a p99 of
5.7 ms. Measured alone at 130,000 dead rows, that read takes 1.97 ms for
`numeric` and 0.87 ms for `int4`, too small a gap to explain the earlier tails.

Controls: a 60 s stress run with the extension's numeric min and max off by one
stored unit failed with `checkpoint stress aggregate or snapshot invariant
failed`. The same off-by-one in the harness's own conversion of min, which only
the repeatable-read comparison uses, passed: the checks compare the extension
with PostgreSQL inside SQL, not the harness's copy of min and max.

### Two-hour hold, int4, 2026-10-29

One 7,200-second `int4` soak on 0.4.0, otherwise at the default settings,
holds the old snapshot for 60 minutes instead of 10
(`benchmarks/results/checkpoint-soak-2h-int4-040-2026-10-29.log`). It failed
the missed-arrival limit. No worker failed, and the retained snapshot still
read back correctly before its release. An 80-minute rerun with a 40-minute hold
failed the same way; its writer timings put two-row transfers highest, with a
p99 of 42 ms (`...checkpoint-soak-80min-int4-040-steps-2026-10-29.log`).

| Minute | Horizon age | Writes per writer per minute | Dead source rows | Database |
|---|---|---|---|---|
| 1 | 4,516 | 1,637 | 13,226 | 24 MB |
| 21 | 88,918 | 1,635 | 275,052 | 230 MB |
| 30 | 127,244 | 1,146 | 374,313 | 348 MB |
| 45 | 171,516 | 449 | 469,500 | 519 MB |
| 60 | 188,455 | 328 | 512,920 | 584 MB |
| 61 (released) | 2 | 1,618 | 7,354 | 584 MB |
| 120 | 0 | 1,637 | 5,732 | 145 MB |

Writers held the offered 1,636 writes a minute for the first 21 minutes, to a
horizon age near 89,000, then fell steadily to a fifth of it. The
repeatable-read reader and the keyed monitor slowed later, and the pruner
committed 7 times in the last held minute against about 1,500 before. All of
them returned to full rate in the first minute after release, and stayed there
for the remaining hour. At minute 60 the manifest catalog held 188 MB and the
chunk catalog 117 MB, on a cluster with the default 128 MB `shared_buffers`.
A plain table under the same kind of hold did not slow, and the largest cost
measured so far is the statement-tracking counter rows that every captured
statement updates; see [captured writers under a held
snapshot](../benchmarks/held-snapshot-writers-2026-10-29.md).

The database shrank from 584 MB to 145 MB after release. Each writer backend's
RSS rose from 30 MB to 151 MB and stayed there; it includes touched shared
buffers.

### Two-hour hold on 0.5.0, int4, 2026-10-29

The same soak on 0.5.0, where captured statements no longer update a counter
row, also failed the missed-arrival limit
(`benchmarks/results/checkpoint-soak-2h-int4-050-2026-10-29.FAILED-missed-arrivals.log`).
Each writer missed 41,770 of 216,000 offered arrivals (19%), the
repeatable-read reader 35,126 and the keyed monitor 17,428. No worker failed,
no invariant failed, and every worker returned to full rate in the first minute
after release.

| Minute | Horizon age | Writes per writer per minute | Pruner commits | Database |
|---|---|---|---|---|
| 15 | 68,130 | 1,637 | 1,433 | — |
| 21 | 95,713 | 1,460 | 484 | 272 MB |
| 30 | 124,975 | 798 | 30 | 382 MB |
| 45 | 157,792 | 722 | 5 | 503 MB |
| 60 | 179,808 | 510 | 13 | 566 MB |
| 61 (released) | 1 | 1,627 | — | 365 MB |

Writers slowed from about the same horizon age as on 0.4.0, fell less far by
minute 60 (510 against 328 writes a minute), and fell faster between minutes
21 and 30. One run of each does not separate the versions. The pruner fell
behind before the writers did on both, and at minute 60 the manifest and chunk
catalogs held 218 MB and 132 MB, against the default 128 MB `shared_buffers`.
The [held-snapshot control](../benchmarks/held-snapshot-writers-2026-10-29.md#050-captured-writes-skip-the-counter-rows),
which runs writers and the maintenance worker but no readers, held captured
writes near 4 ms to an age of 500,000 on 0.5.0. Removing the counter rows
therefore did not remove the soak slowdown; its remaining cause is not known.

## Scope

One host, one database, one captured source, and catalogs that fit in memory.
The soak does not test disk exhaustion, replicas, physical restore, upgrades, or
memory pressure from other processes. The results are not a comparison with
other database engines.
