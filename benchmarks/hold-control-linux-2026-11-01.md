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

## Wait-event diagnostics

Two further runs of the same control with `DIAG=1` sampled the pgbench sessions'
wait events every 0.25 seconds and printed per-table statistics each round
([two clients](evidence/hold-control-diag-2clients-linux-2026-11-01.json),
[one client](evidence/hold-control-diag-1client-linux-2026-11-01.json)). Sampling
adds overhead, and each is one run.

**Two clients (run 41320540326).** The slowdown reproduced: in round 6 captured
averaged 3,092 ms at 156 tps against 81 ms at 198 tps for plain. Captured writers
showed `Lock:transactionid` (29 samples) and `Lock:tuple` (14); plain showed none.
In round 7, `capture_marks` held 170,085 dead rows beside 20,990 live, and
`capture_events` 204,102 dead beside 25,188 live.

**One client (run 41323300174).** Lock waits almost disappeared (one or two
samples per round). Captured was still slower early, 98 ms against 3.8 ms in round
1, but now both tables saturated sooner because one session cannot keep up with
200 transactions per second once each transaction slows. From round 7 captured
trailed plain by about 10% in tps and 3% in latency. Most samples in both tables
were `active`, meaning the backend was running rather than waiting.

**Reading.** Writers queue on row locks when there are two, but removing the
queue does not remove captured's extra per-transaction cost; that cost is CPU work
inside the backend. Lock queueing amplifies it, but is not the root cause. The
data do not show which statement does the work. The dead catalog rows are a
candidate, not a finding. The single-client run is also confounded by earlier
saturation, so it cannot give a clean capture-versus-plain ratio.

## CPU profiles

[Run 41326470471](https://github.com/fikrifirmanf/syntradb/actions/runs/41326470471)
repeated the two-client control for 8 rounds and recorded user-space CPU samples
(`perf record -e cpu-clock:u -F 199`, 15 s) of the pgbench backends in rounds 1, 4,
6 and 8, for both tables ([reports](evidence/hold-control-profiles-linux-2026-11-01)).
Captured again slowed far earlier than plain: round 6 averaged 3,162 ms at 155 tps
against 46 ms at 195 tps. The reports are self-time only and truncated to the top
70 rows. Percentages are shares of each backend group's own samples, so they do
not compare absolute cost between tables.

- **Same dominant cost in both tables.** `heap_hot_search_buffer`
  (6.6% to 8.4% of captured samples in rounds 4 to 8, 8.0% to 8.3% of plain),
  `heapam_index_fetch_tuple`, `GlobalVisTestFor` and `GlobalVisTestIsRemovableXid`
  lead both. These are PostgreSQL walking HOT chains through row versions that
  cannot be removed while the old snapshot is held.
- **No SyntraDB function stands out.** In captured, `syntradb.so` symbols appear
  only in round 1, at 0.4% each (`CatalogScan::open`, UTF-8 validation). Rounds 4,
  6 and 8 show none in the top 70 rows.
- **Captured spreads extra work through PostgreSQL itself.** Round 1's captured
  profile includes `base_yyparse`, `expression_tree_walker_impl` and catalog-cache
  lookups (3.4%, 1.4% and 1.9%) that plain lacks, which indicates SQL parsed and
  planned inside the capture trigger. Per-table statistics from the earlier
  diagnostic run show about 19 to 30 index fetches per transaction on the small
  capture catalogs (`capture_bindings`, `tracked_sources`, `capture_heads`,
  `capture_marks`), each walking version chains that grow while the snapshot is held.

These samples do not attribute `heap_hot_search_buffer` to the source table or to
the capture catalogs, so they do not show which accesses cost the extra time.
Attributing it needs call stacks or per-statement buffer counts for the capture
trigger's own queries at a horizon age near 70,000.
