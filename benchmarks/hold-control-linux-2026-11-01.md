# Held-snapshot control on Linux at 0.12.0

A captured table and a plain table take the same paced writes in alternating
30-second rounds while one repeatable-read session holds an old snapshot. The
[workflow run](https://github.com/fikrifirmanf/syntradb/actions/runs/41315009198)
used `benchmarks/scripts/hold-control.sh 40 30 200 cycle` on a GitHub-hosted
Ubuntu 24.04 runner. Each round has two pgbench clients at 200 transactions per
second per table. Each transaction is the soak writer's transfer, insert, key
move and delete. The [versioned data](evidence/hold-control-linux-2026-11-01.json)
holds all 80 round results. The two tables ended with matching sums, and no
transaction failed. This is one run on one shared runner.

| Round | Horizon age | Plain latency / tps | Captured latency / tps |
|---|---|---|---|
| 1 | 12,668 | 1.8 ms / 200 | 5.3 ms / 199 |
| 4 | 50,602 | 11 ms / 201 | 88 ms / 201 |
| 6 | 74,026 | 76 ms / 198 | 3,353 ms / 154 |
| 10 | 110,672 | 4,277 ms / 142 | 6,229 ms / 114 |
| 20 | 174,298 | 8,588 ms / 90 | 9,644 ms / 71 |
| 40 | 259,929 | 10,637 ms / 58 | 11,648 ms / 44 |

Latency is pgbench's average at a fixed offered rate, so it includes queueing
behind schedule.

## What it shows

- Plain PostgreSQL also degrades sharply under this workload: throughput falls
  from 200 to 58 transactions per second and latency reaches about 10 seconds.
  The soak's absolute 5% missed-arrival limit cannot be met by the native table
  under a long hold, so the limit measures PostgreSQL as much as SyntraDB.
- Captured is far slower than plain between horizon ages of about 40,000 and
  110,000 (for example 3.4 s against 76 ms in round 6). Past that both are
  saturated and captured trails plain by roughly 10% to 25%.
- The workload reuses a few keys, which builds long version chains. It is a hard
  case, not a typical one.

No gate was relaxed and this run does not claim the load gate is met. It
suggests the gate should compare SyntraDB with a matched native run at each
horizon age. The band between about 40,000 and 110,000 is the open engineering
target.
