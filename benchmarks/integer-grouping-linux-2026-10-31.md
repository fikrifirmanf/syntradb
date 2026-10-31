# First native Linux integer grouping comparison

The first million-row grouping run matched native SQL in all 12 cases.
Rust requests had lower medians than serial PostgreSQL in this workload.
Native parallel plans were faster at the two smaller group counts. This is one
runner observation, not evidence of broad analytical superiority.

[Run 41170771080](https://github.com/fikrifirmanf/syntradb/actions/runs/41170771080)
measured source `3e4b102` on Ubuntu 24.04 x86_64, with four vCPUs on an AMD EPYC
9V45 host. PostgreSQL 18.6 and the extension used release builds. Both routes
read the same heap through one backend; native plans launched two parallel
workers when allowed. JIT was off and `work_mem` was 4 MiB.

## Prepared-request medians

Each route had one warmup and seven timed requests. Values below are milliseconds
and include receipt of every output row. Each request's groups matched the native
oracle in every field.

| Layout | Groups | Native serial | Rust, serial setting | Native, two workers allowed | Rust, two workers allowed |
|---|---:|---:|---:|---:|---:|
| int4/int4 | 16 | 82.4 | 58.9 | 44.1 | 63.3 |
| int4/int4 | 1,024 | 85.0 | 65.0 | 47.6 | 64.7 |
| int4/int4 | 65,536 | 170.1 | 133.1 | 167.2 | 133.7 |
| int8/int8 | 16 | 84.7 | 61.4 | 45.1 | 59.6 |
| int8/int8 | 1,024 | 89.5 | 67.4 | 46.8 | 65.8 |
| int8/int8 | 65,536 | 174.9 | 135.4 | 173.3 | 132.8 |

The Rust function remains serial under either setting. The two columns retain
its separate measurements beside each matched native route.

## Memory and scope limits

The 65,536-group native parallel plans spilled hash aggregation and sorting to
disk. The saved plans show five hash batches and external merge sorts. Rust's
grouping state has a cardinality cap but does not enforce `work_mem` for its
hash table. Its SQL output and outer sort still use PostgreSQL memory rules.
These measurements therefore do not establish equal private memory use or a
better aggregation algorithm under an equal memory budget.

This first harness passed a 65,536-group cap to the Rust function in every case,
including the small-group datasets. That choice can impose extra allocation
cost. A follow-up should expose the configured cap and compare an in-memory
native plan with enough work memory, while retaining these original results.

The [workload contract](integer-grouping.md) describes NULLs, signed values,
query order and timing methods. It covers two layouts with one integer key,
warm data and no concurrent writes. It does not cover skew, filters, multiple
keys or cold caches. Seven samples describe these requests; they cannot estimate
production tail latency.

## Evidence and next checks

The [versioned evidence](evidence/grouping-linux-2026-10-31-run1.json) preserves
every prepared-request sample, server sample and final execution plan. It also
records the actual source commit, workflow commit, source fingerprint, runner
details and original report SHA-256. The workflow commit differs from the source
because its source-ref input selected a feature branch.

## Repeat on another host model

The [15-sample repeat](https://github.com/fikrifirmanf/syntradb/actions/runs/41171390584)
also passed all 12 cases against the same immutable source commit. Its runner
used an AMD EPYC 9V74 host, with the same four vCPUs and runner image. It was
slower overall, so do not combine its absolute timings with the first run.
The [second evidence file](evidence/grouping-linux-2026-10-31-run2.json) retains
its samples and plans separately.

| Layout | Groups | Native serial | Rust, serial setting | Native, two workers allowed | Rust, two workers allowed |
|---|---:|---:|---:|---:|---:|
| int4/int4 | 16 | 123.2 | 88.2 | 63.9 | 88.9 |
| int4/int4 | 1,024 | 130.2 | 96.4 | 68.6 | 95.7 |
| int4/int4 | 65,536 | 241.0 | 185.7 | 219.7 | 183.6 |
| int8/int8 | 16 | 122.4 | 90.8 | 65.0 | 90.4 |
| int8/int8 | 1,024 | 131.8 | 98.9 | 69.3 | 99.1 |
| int8/int8 | 65,536 | 246.2 | 241.2 | 236.9 | 255.7 |

Native parallel plans again won at the smaller group counts. At 65,536 wide
groups, Rust's serial advantage shrank to about 2%, and the native parallel
median was lower than Rust's. The large-group result is therefore mixed across
these hosts and remains subject to the memory caveat above. Add an in-memory
native control before attributing the difference to the grouping kernel.

The checkpoint throughput gate remains open. This isolated grouping workload
does not address the earlier held-snapshot failure.
