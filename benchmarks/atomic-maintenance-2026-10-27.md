# Atomic checkpoint maintenance on 27 October 2026

The explicit combined API reduces repeated validation for unchanged checkpoints.
It keeps the full stored-byte validation after changed publication. This preserves
corruption detection and limits the changed-path speed gain.

The [API contract](../docs/atomic-checkpoint-maintenance.md) describes admission,
rollback, retry and the captured pruning cut. Existing separate calls and reader
defaults remain available. The combined stress option is off by default.

Two fresh 51-sample cost runs used 10,000 source rows. Both paths use one prepared
SQL round trip and one implicit transaction. A materialized CTE orders the
separate apply and prune calls. Setup and checks are outside timing; EXPLAIN WAL
is a separate probe. Cases run sequentially in the same order in both trials.
These are median request milliseconds and backend statement WAL bytes.

| Case | Trial | Separate ms | Combined ms | Separate WAL bytes | Combined WAL bytes |
|---|---:|---:|---:|---:|---:|
| two_events | 1 | 0.491834 | 0.491083 | 9986 | 9992 |
| unchanged | 1 | 0.398292 | 0.214083 | 0 | 0 |
| two_events | 2 | 0.557875 | 0.536417 | 9986 | 9992 |
| unchanged | 2 | 0.483083 | 0.245791 | 0 | 0 |

Unchanged maintenance takes roughly half the time in both runs. Changed
maintenance has little improvement in the first run and a small improvement in
the second. Changed WAL is similar; unchanged calls generate zero statement WAL.
WAL counters exclude commit records. This does not establish production capacity
or speed superiority across database engines.

Four fresh 30-second paced runs offered 100 cycles/second to each of eight
workers. Order was separate/combined/combined/separate. Appliers used the combined
API only on their apply-plus-prune half of cycles. Apply-only cycles and the
independent pruner stayed active. Both modes opted into reader replay. There
were no compiler or test jobs alongside timed phases; the host was not isolated.

Successful scheduled-cycle p99 below includes dispatch delay and the native
aggregate oracle. Writer misses combine two workers, each with 3,000 arrivals.
Reader misses are cycle counts for one worker. Repeatable-read cycles have nine
checks. Dropped arrivals are excluded from latency histograms.

| Mode | Trial | Emitted events | Writer misses | RC p99, ms | RC misses | RR p99, ms | RR misses |
|---|---:|---:|---:|---:|---:|---:|---:|
| combined | 1 | 32,268 | 86 | 3.599 | 1 | 19.199 | 25 |
| combined | 2 | 32,646 | 15 | 3.041 | 0 | 7.567 | 0 |
| separate | 1 | 32,610 | 23 | 3.011 | 0 | 7.851 | 1 |
| separate | 2 | 32,070 | 121 | 3.031 | 0 | 8.327 | 0 |

Mixed results vary. The first combined run has worse repeatable-read tails than
its baseline; the second is better. Writer event counts are close to the offered
limit in these trials. No consistent broad workload win follows from two pairs.
Use the explicit API for its measured unchanged-state benefit, and keep automatic
maintenance and reader replay admission as separate decisions.

The additional saturated 60-second combined run passed 98,816 aggregate
checks and 7,335 keyed reconstructions. Writers emitted 43,320 events.
Every worker and the independent pruner made progress. The retained snapshot
stayed valid; final drain emptied the feed. Node WAL was 164,974,384
bytes, excluding final drain and VACUUM. Allocated checkpoint storage after drain
was 87,670,784 bytes. These figures are not
memory limits or reclaimed disk size. Crash checks exercised combined commit, abort and retry. Existing logical-restore
checks also passed in the cost and saturated runs.

Across the five worker runs, 219,343 aggregate checks and 19,314 keyed checks
passed. There were zero invariant failures or unexpected errors. Independent
report checks reconcile committed writer events, pruned counts, offered arrivals,
old snapshots and final drains. All seven runs also passed fifteen committed
checkpoint scenarios, including no-XID/no-rewrite retries, transaction restrictions
and a deterministic producer commit during paused publication.

Six PostgreSQL tests cover corruption, stored-byte alteration, initial seed,
empty state, retry identity, prune rollback and ACLs. Two Cargo tests cover the
benchmark option. Five mutations failed focused tests. The full check passed
235 tests, Clippy and compilation. Private-catalog DDL fixtures now default to
serial execution; independent sessions and worker stress retain concurrency tests.

Next, evaluate resource limits and longer retention with the combined operation.
Preserve post-publication byte checks, snapshot cuts and rollback guarantees.
Changed work still pays full validation, and the mixed results do not justify
changing defaults or adding automatic maintenance workers.

Raw reports:

- [atomic-maintenance-combined-100-trial1-2026-10-27](results/atomic-maintenance-combined-100-trial1-2026-10-27.json)
- [atomic-maintenance-combined-100-trial2-2026-10-27](results/atomic-maintenance-combined-100-trial2-2026-10-27.json)
- [atomic-maintenance-costs-trial1-2026-10-27](results/atomic-maintenance-costs-trial1-2026-10-27.json)
- [atomic-maintenance-costs-trial2-2026-10-27](results/atomic-maintenance-costs-trial2-2026-10-27.json)
- [atomic-maintenance-separate-100-trial1-2026-10-27](results/atomic-maintenance-separate-100-trial1-2026-10-27.json)
- [atomic-maintenance-separate-100-trial2-2026-10-27](results/atomic-maintenance-separate-100-trial2-2026-10-27.json)
- [atomic-maintenance-stress-60s-2026-10-27](results/atomic-maintenance-stress-60s-2026-10-27.json)

The seven timing reports use source fingerprint
`d6c6f972ec7159ee11e556fedf76aa8a6345ccda7bc076f6876a3fcb81736636`.
The [final verification report](results/atomic-maintenance-final-verification-2026-10-27.json)
uses `08cc90785623074b21d8d99cdf28164dd35ff608a1eaf0d54ae47254b6c18592`.
The only input difference is a test fixture DELETE restricted to its two known
keys. Engine, SQL API, benchmark and script source bytes are identical. The final
three-sample run verifies the build and recovery gates; it is excluded from the
performance tables.

Every extension and runner fingerprint agrees with its report. Report/archive
hashes were verified in [source-variants](results/source-variants/). Each archive
contains all 94 recorded inputs. The final verification archive matches the
workspace; timing archives retain the earlier test-fixture variant.
