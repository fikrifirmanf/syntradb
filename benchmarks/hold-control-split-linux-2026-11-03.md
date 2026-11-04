# Held-snapshot control with one captured table per client

Question: do writers queueing on the capture head row cause the gap between
captured and plain tables at the cliff? The earlier
[wait-event diagnostics](hold-control-linux-2026-11-01.md#wait-event-diagnostics)
showed `Lock:transactionid` and `Lock:tuple` waits with two clients on one
captured table.

Method: the same held-snapshot control, with `SPLIT=1` so each of the 2 pgbench
clients writes its own captured table and so its own head row. Total offered load
is unchanged (200 transactions per second, 20 rounds of 30 seconds, `cycle`
shape, 200 ms maintenance interval). The code is the experiment branch
`exp/head-chain-hypothesis` at c7fb364, which adds only measurement knobs to
0.12.0 with kept plans and commit-time batching. Three runs on GitHub-hosted
Ubuntu runners. The [versioned data](evidence/hold-control-split-linux-2026-11-03.json)
holds all 60 round results and the verdict lines.

| Run | Worst latency vs plain | Rounds over 2x | Worst throughput ratio | Correctness |
|---|---|---|---|---|
| [41667564573](https://github.com/fikrifirmanf/syntradb/actions/runs/41667564573) | 4.3x (round 7) | 2 | 0.84 | pass |
| [41667582245](https://github.com/fikrifirmanf/syntradb/actions/runs/41667582245) | 4.6x (round 7) | 1 | 0.84 | pass |
| [41667599148](https://github.com/fikrifirmanf/syntradb/actions/runs/41667599148) | 3.9x (round 8) | 1 | 0.84 | pass |

Round 7 or 8, run by run (plain / captured latency):

| Run | Round 7 | Round 10 | Round 20 |
|---|---|---|---|
| 41667564573 | 245 / 1,053 ms | 3,744 / 4,902 ms | 7,919 / 8,702 ms |
| 41667582245 | 497 / 2,293 ms | 4,497 / 5,347 ms | 8,261 / 8,860 ms |
| 41667599148 | 55 / 943 ms | 2,839 / 4,577 ms | 7,800 / 8,466 ms |

## What it shows

- The relative latency check fails in all three runs at 3.9x to 4.6x. The five
  single-table runs at the same 200 ms interval failed at 3.8x to 7.1x, so
  removing writer-on-writer queueing on the head row did not close the gap.
- The head-row lock is therefore not the main cause. Writers did queue on it
  (issue 11), but removing the queue is not enough.
- Throughput and correctness passed in all three runs.
- After the cliff both tables saturate and captured trails plain by about 10%.

## Limits

Three runs on a shared runner. The cliff position moves with runner noise: plain
latency alone ranged from 8 to 388 ms at round 6 across the earlier runs. The
work does not identify the remaining cause. The per-statement EXPLAIN script still
mirrors the per-firing SQL, not the batched insert, so the batched path's own cost
at the cliff is unprofiled.

The design decision that follows from this is in
[capture-ordinal-design.md](../docs/capture-ordinal-design.md): accept and
document the limit (option A), and do not change the capture format on this
evidence.
