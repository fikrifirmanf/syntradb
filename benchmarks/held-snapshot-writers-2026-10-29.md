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
