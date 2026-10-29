# Captured writers under a long-held snapshot

The [two-hour soak](../docs/checkpoint-soak.md#two-hour-hold-int4-2026-10-29)
failed: writers slowed once the cleanup horizon age passed about 89,000. These
runs check whether PostgreSQL alone slows the same way, and which SyntraDB row
the cost comes from. All ran on 0.4.0 on one laptop, with one old
repeatable-read snapshot held throughout.

## Captured and plain tables side by side

`benchmarks/scripts/hold-control.sh 40 30 200` creates two 10,000-row tables
with the same shape, one captured and one plain. In 40 alternating 30-second
rounds, two pgbench clients run two-row transfers on each table at 200
transactions per second. The maintenance worker drains the captured feed.
Latency is pgbench's average and includes time spent behind schedule
(`results/hold-control-int4-040-2026-10-29.log`).

| Round | Plain latency | Captured latency | Captured tps | Dead rows, each table |
|---|---|---|---|---|
| 1 | 3.5 ms | 4.6 ms | 205 | 12,000 |
| 10 | 3.5 ms | 26 ms | 195 | 120,000 |
| 20 | 3.6 ms | 15 ms | 204 | 240,000 |
| 30 | 3.5 ms | 368 ms | 201 | 360,000 |
| 40 | 3.6 ms | 2,379 ms | 169 | 470,000 |

With the same dead rows, the plain table kept its latency. The cost is in
capture, not in PostgreSQL's version chains on the source table. Both tables
ended with matching sums. Eleven captured transactions and no plain ones failed,
all with `deadlock detected`: transfers lock two rows in random order, and slow
transactions overlap more often. Capture documents deadlocks as retryable.

## Which row

Writers lock the feed head row and update one of 16 statement-tracking counter
rows in `syntra.source_generation_shards`. Pruning updates the head row. Every
captured statement increments one counter row. A held snapshot keeps every
version of both, and finding the current version walks the older ones.

Each script below holds a snapshot, adds versions to one kind of row with a
no-op `UPDATE` as superuser, and then times 2,000 single-row updates on each
table with one client.

| Versions added | Plain | Captured |
|---|---|---|
| none | 0.15 ms | 0.59 ms |
| 20,000 to the head row | 0.07 ms | 0.68 ms |
| 20,000 to each counter row | 0.07 ms | 1.67 ms (first run), 1.75 ms (second) |
| 40,000 to each counter row | 0.04 ms | 1.94 ms |
| 60,000 to each counter row | 0.04 ms | 3.38 ms |
| 80,000 to each counter row | 0.04 ms | 6.33 ms |

Sources: `benchmarks/scripts/hold-head.sh`, `hold-shards.sh`, and
`results/hold-head-int4-040-2026-10-29.log`,
`hold-shards-int4-040-to-20000-2026-10-29.log` and
`hold-shards-int4-040-2026-10-29.log`. The second shard run started at
0.39 ms.

Head-row versions alone cost little; 0.2.0 already stopped writers updating
that row. The counter rows grow the write cost with the number of versions
behind them, the pattern 0.2.0 removed from the head row. They are the largest
cost found so far. These runs do not show that they explain the whole soak
slowdown: the soak also leaves dead marks, events and checkpoint rows, which
were not isolated.

## Next step

The counter rows exist for statement tracking, which trusted reuse compares as
a 16-entry version vector. Removing their updates needs a new tracking format
and an upgrade path that keeps reuse correct; the options are in
[a proposal](../docs/tracking-counter-proposal.md). Until then the snapshot age policy in
[resource bounds](../docs/resource-bounds.md#snapshot-age-policy) stands: page at
an age of 10,000, well below where writers slowed.

## 0.5.0: captured writes skip the counter rows

0.5.0 implements option A of the proposal: statements on a captured source no
longer update a counter row. The same control script, unchanged, ran again on
0.5.0 on the same laptop
(`results/hold-control-int4-050-2026-10-29.log`).

| Round | Plain latency | Captured latency | Captured tps | Horizon age |
|---|---|---|---|---|
| 1 | 1.9 ms | 23.5 ms | 198 | 12,000 |
| 2 | 2.0 ms | 2.5 ms | 197 | 25,000 |
| 10 | 1.9 ms | 2.9 ms | 195 | 127,000 |
| 20 | 2.0 ms | 3.0 ms | 199 | 253,000 |
| 30 | 2.0 ms | 3.5 ms | 201 | 378,000 |
| 40 | 2.0 ms | 4.3 ms | 195 | 503,000 |

No transaction failed on either table, and both ended with matching sums. The
round 1 figure did not recur and looks like warm-up. Plain latency was lower
than in the 0.4.0 run (1.9 ms against 3.5 ms), so the laptop was less loaded;
compare each run's captured column with its own plain column. On 0.4.0 the
captured side went from 1.3 times plain to 660 times plain by round 40; on
0.5.0 it went from 1.3 to 2.1 times.

Captured latency still rises slowly, from 2.5 ms to 4.3 ms over 500,000 dead
rows per table. The marks, events and checkpoint rows that the hold keeps were
not isolated and may explain it. This is a 20-minute control at 200 tps, not
the two-hour soak; the soak must hold writer speed past an age of 89,000 before
the slowdown counts as fixed.
It did not: the two-hour soak on 0.5.0 still slowed from about the same age
([0.5.0 soak](../docs/checkpoint-soak.md#two-hour-hold-on-050-int4-2026-10-29)).
The runs below look for the remaining cost.

## 0.5.0: what still slows the soak

### Readers and cache size ruled out

Two one-hour soaks on 0.5.0 held the snapshot for 30 minutes with the
repeatable-read reader, the read-committed reader and the keyed monitor removed
(`--checkpoint-soak-skip`). One used the default 128 MB `shared_buffers`, the
other 1 GB (new `--shared-buffers-mb` option). Both end with `checkpoint stress
did not exercise pruning, keyed checks and vacuum`, because the keyed monitor is
skipped; the logs carry `.FAILED-no-keyed-checks-by-design`.

| Horizon age | Writes per minute, 128 MB | Writes per minute, 1 GB |
|---|---|---|
| about 46,000 | 1,636 | 1,637 |
| about 93,000 | 1,595 | 1,636 |
| about 121,000 | 1,279 | 1,376 |
| about 125,000 | 837 | 876 |
| released | 1,633 | 1,633 |

Writers slowed at the same ages as in the full two-hour soak (798 a minute at
125,000), with or without readers, and with eight times the buffer cache.

### Stack samples

`benchmarks/scripts/soak-sample.sh` ran macOS `sample` on the pruner, the first
writer and the first applier for 10 seconds at minutes 8, 20, 26 and 29 of the
1 GB run (`results/soak-samples-noreaders-sb1g-050-2026-10-29/`). The harness
now names each soak backend by its actor in `application_name`.

At minute 29 the writer's samples, outside the time spent waiting for its client:

| Where | Samples |
|---|---|
| Waiting for another transaction's lock on the feed head row (`heap_lock_tuple` under the capture trigger's `FOR NO KEY UPDATE`) | 2,221 |
| Walking dead versions of source rows in its own statements | about 890 |
| Walking versions of the head row inside the lock statement | 307 |
| Walking versions in the trigger's native scan of marks | 166 |

The applier spent 685 samples waiting for the same lock in its prune `UPDATE`
of the head row. The pruner was idle 99% of the time: with combined
maintenance the appliers prune too, so its falling commit count is a symptom.

The capture trigger locks the head row and keeps the lock to commit, so that
event ordinals stay gap-free and follow commit order. Captured transactions
therefore take the lock one at a time, each for the rest of its transaction:
the soak writer's transfer takes it, then the insert, key move, delete and
commit, four client round trips, run under it.

### The soak writer's statements on a plain table

`hold-control.sh 40 30 55 cycle` repeats the control with the soak writer's
statements: the two-row transfer, then insert a per-client key, move it and
delete it, reusing the same keys every cycle. The rate is the two soak writers'
combined 55 transactions per second
(`results/hold-control-cycle-int4-050-2026-10-29.log`).

| Round | Horizon age | Plain | Captured |
|---|---|---|---|
| 1 | 2,000 | 3.4 ms | 5.7 ms |
| 10 | 40,000 | 9.6 ms | 11.8 ms |
| 20 | 80,000 | 13.4 ms | 12.6 ms |
| 30 | 120,000 | 12.0 ms | 12.1 ms |
| 35 | 140,000 | 12.5 ms | 19.2 ms |
| 40 | 159,000 | 12.1 ms | 39.5 ms |

No transaction failed and both tables ended with matching sums. The plain table
slowed 3.5 times: reusing keys under a held snapshot walks their dead versions,
which PostgreSQL pays without SyntraDB. The captured table followed it to an
age of about 120,000 and then rose faster. The soak, which adds two appliers
taking the same lock, slowed from about 90,000.

### Reading

The evidence fits one mechanism: every captured transaction and every prune
holds the head-row lock in turn, and a held snapshot makes each holder slower,
partly through PostgreSQL's own dead versions and partly through the head row
and marks. Once the summed hold time nears one second per second the queue
grows and writers miss their arrivals. Faster lookups would move the point;
only a shorter hold, or no shared lock, would remove the queue. One laptop and
one run of each.

## 0.6.0: the head lock taken at commit

0.6.0 fires the capture row trigger at commit (see
[upgrades](../docs/upgrades.md#050-to-060)), so a captured transaction holds
the head-row lock only from commit-time capture to commit. The cycle control
was rerun on it (`hold-control.sh 40 30 55 cycle`,
`benchmarks/results/hold-control-cycle-int4-060-2026-10-29.log`).

| Round | Horizon age | Plain ms | Captured ms (0.6.0) | Captured ms (0.5.0, age) |
|---|---|---|---|---|
| 5 | 18,000–20,000 | 4.9 | 6.6 | 9.0 (20,000) |
| 10 | 38,000–40,000 | 6.6 | 8.0 | 11.8 (40,000) |
| 33 | ~113,000 | 13 | 11 | 12.1 (120,000, round 30) |
| 36 | 122,000–124,000 | 20.8 | 10.9 | 45.9 (143,000) |
| 40 | 138,000–140,000 | 13.3 | 392.5 | 39.5 (159,000) |

From round 14 to round 32 (about 16:59 to 17:09) both tables, plain included,
ran at 0.2 to 14 s per transaction. The power log shows other applications
active on the laptop then, and the 15-minute load average stood at 45 shortly
after. That window measures the machine, not SyntraDB, and the run cannot
settle whether the late captured rise moved. Outside it, the captured table
stayed within about 1.5 ms of plain to round 39, and round 40 rose to 392 ms.
No transaction failed and both sums matched. A rerun on an idle machine is
needed.
