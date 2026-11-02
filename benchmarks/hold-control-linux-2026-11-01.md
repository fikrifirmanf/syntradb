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

## Per-statement EXPLAIN under a held snapshot

[Run 41331086964](https://github.com/fikrifirmanf/syntradb/actions/runs/41331086964)
(`benchmarks/scripts/hold-explain.sh`, [full log](evidence/hold-explain-linux-2026-11-02.log))
aged a captured table under a held snapshot at 100 paced cycles per second, with a
background apply-and-prune loop, and at four horizon ages ran
`EXPLAIN (ANALYZE, BUFFERS)` on the statements the capture trigger runs for each
changed row. The SQL mirrors `crates/syntra-pg/src/capture.rs`. The two catalog
reads that the trigger makes through the native reader (the newest mark and the
binding lookups) are emulated with equivalent queries. Each measurement ran in an
otherwise idle session, so it excludes queueing behind other writers.

| Horizon age | Head lock and join | Newest mark | Event and mark insert |
|---|---|---|---|
| 6,526 | 14 buffers, 0.058 ms, plan 0.24 ms | 2 buffers, 0.013 ms | 35 buffers, 0.354 ms, plan 0.07 ms |
| 32,758 | 40 buffers, 0.077 ms, plan 0.27 ms | 4 buffers, 0.022 ms | 36 buffers, 0.417 ms |
| 65,127 | 75 buffers, 0.156 ms, plan 0.41 ms | 4 buffers, 0.033 ms | 37 buffers, 0.409 ms |
| 82,520 | 95 buffers, 0.192 ms, plan 0.42 ms | 3 buffers, 0.019 ms | 37 buffers, 0.410 ms |

The binding and tracked-source lookups stayed at 2 buffers and under 0.01 ms at
every age. At the same ages the paced writers' average latency went from 5.3 ms to
18.9 ms to 2,702 ms (samples from chunks 1, 7 and 13 of the log).

**Reading.**

- No single statement is slow. Even at age 82,520 each takes under 0.5 ms, and the
  catalog tables the earlier statistics flagged as bloated (`capture_marks`,
  `capture_events`) are not walked: their reads stay at a few buffers because the
  scans start at the newest entry. That removes dead catalog rows from the list of
  suspects for the per-statement cost.
- Only the head-row lock grows with age: 14 to 95 buffers and 0.058 to 0.192 ms,
  with its planning time rising from 0.24 to 0.42 ms. The head row gains a version
  with every prune and apply while the snapshot is held.
- The trigger fires once per changed row, and a soak-writer cycle changes five
  rows. Planning and running the two SQL statements costs about 0.7 ms per firing,
  so about 3.5 ms per cycle. That matches the 3.5 ms by which captured exceeded
  plain in round 1 of the control (5.3 ms against 1.8 ms at age 12,668).
- A fixed extra cost of that size per transaction lowers the throughput at which
  the writers saturate. A queueing cliff would then appear earlier for captured than
  for plain, even though the cost itself grows slowly. This fits the earlier
  observations but is not proved: the single-client run was confounded by
  saturation, and no run has changed the per-row cost and shown the cliff move.

**Candidate changes** (none made, each needs the control rerun to test it):

1. Save and reuse the two SPI plans instead of parsing and planning them on every
   firing. This removes about 0.3 ms of the roughly 0.7 ms per firing and needs no
   format change.
2. Do the head lock and the event and mark insert once per transaction instead of
   once per changed row.
3. Insert the event and mark rows through the heap API instead of SQL.

## Kept SPI plans

[Run 41332804819](https://github.com/fikrifirmanf/syntradb/actions/runs/41332804819)
repeated the 40-round control with the capture trigger's two statements prepared once
per backend ([data](evidence/hold-control-saved-plans-linux-2026-11-02.json)). The
plain table, which the change does not touch, behaved almost identically in both runs
(for example 76 ms against 64 ms in round 6), so the runner did not drift much.

| Round | Horizon age | Captured, before | Captured, kept plans | Plain, same run |
|---|---|---|---|---|
| 1 | 12,868 | 5 ms / 199 tps | 3 ms / 206 | 2 ms |
| 4 | 50,914 | 88 ms / 201 | 26 ms / 202 | 11 ms |
| 5 | 63,409 | 713 ms / 183 | 215 ms / 195 | 22 ms |
| 6 | 75,082 | 3,353 ms / 154 | 1,880 ms / 171 | 64 ms |
| 8 | 95,689 | 5,157 ms / 126 | 4,236 ms / 135 | 1,995 ms |
| 20 | 175,507 | 9,644 ms / 71 | 10,007 ms / 68 | 8,550 ms |
| 40 | 259,021 | 11,648 ms / 44 | 11,608 ms / 44 | 10,579 ms |

The change helps, modestly. Per-transaction latency below the cliff drops by about a
third, and the cliff moves right by roughly one round (about 12,000 of horizon age).
It does not close the gap: at about 75,000 captured still averages 1,880 ms against
64 ms for plain, and past about 100,000 both tables are saturated and the two runs
are indistinguishable. This is one run each, so differences of a few percent are
within noise. The remaining per-firing cost (the head-row lock, the event and mark
insert, and five firings per cycle) is untouched.

## Commit-time batching

[Run 41340936860](https://github.com/fikrifirmanf/syntradb/actions/runs/41340936860)
repeated the control with deferred firings at commit buffered and written by one
pre-commit insert and mark per source ([data](evidence/hold-control-batched-linux-2026-11-02.json),
design in [capture-batching-design](../docs/capture-batching-design.md)). It also
includes kept plans. Sums matched and no transaction failed.

| Round | Horizon age | Captured, kept plans | Captured, batched | Plain, this run |
|---|---|---|---|---|
| 1 | 12,772 | 3 ms / 206 | 3 ms / 203 | 2 ms |
| 4 | 51,018 | 26 ms / 202 | 18 ms / 201 | 12 ms |
| 5 | 63,770 | 215 ms / 195 | 64 ms / 202 | 19 ms |
| 6 | 76,004 | 1,880 ms / 171 | 742 ms / 186 | 66 ms |
| 7 | 87,257 | n/a | 2,391 ms / 163 | 415 ms |
| 8 | 97,351 | n/a | 3,908 ms / 146 | 1,951 ms |
| 20 | 178,044 | n/a | 9,684 ms / 71 | 8,841 ms |
| 40 | 267,092 | n/a | 11,292 ms / 50 | 10,561 ms |

Below the cliff the captured table is now within about 1 ms of plain (round 1: 2.5
against 1.9 ms), and the cliff moves right by about another round: round 6 falls from
1,880 to 742 ms. It still does not close the gap through the middle of the curve (round
7: 2,391 ms against 415 ms), and past about 110,000 of horizon age captured trails plain
by 10 to 20 percent in throughput (50 against 59 tps at round 40). One run per variant on
shared runners, so treat differences of a few percent as noise. The load gate stays open.
