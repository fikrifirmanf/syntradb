# Linux grouping with memory controls

The revised million-row comparison passed all 24 cases. With 64 MiB of
`work_mem`, native PostgreSQL wrote no temporary blocks and had lower medians
than Rust in every 65,536-group case. The earlier large-group results cannot
support a general claim that the Rust kernel is faster.

[Run 41175097265](https://github.com/fikrifirmanf/syntradb/actions/runs/41175097265)
measured source `331cbca` on four vCPUs on an AMD EPYC 7763 host. It used Ubuntu
24.04, PostgreSQL 18.6, release builds and 15 timed samples after a warmup.
This host differs from both earlier runs. Compare routes within this run;
do not combine absolute timings across hosts.

The revised [workload](integer-grouping.md) gives Rust the actual dataset group
budget and records it. Each dataset runs at 4 MiB and 64 MiB work memory, with
serial and native-default parallel settings. Both routes return identical ordered
groups. Every returned field matched native SQL after every request.

## Large-group prepared-request medians

All rows below have 65,536 groups. Values are milliseconds, including receipt of
the complete result. The Rust function remains serial under either parallel setting.

| Layout | Work memory | Native workers allowed | Native median | Rust median | Native spill |
|---|---:|---:|---:|---:|---|
| int4/int4 | 4 MiB | 0 | 318.9 | 298.9 | yes |
| int4/int4 | 4 MiB | 2 | 298.1 | 306.9 | yes |
| int4/int4 | 64 MiB | 0 | 268.3 | 283.2 | no |
| int4/int4 | 64 MiB | 2 | 267.0 | 283.3 | no |
| int8/int8 | 4 MiB | 0 | 310.4 | 298.4 | yes |
| int8/int8 | 4 MiB | 2 | 308.8 | 300.0 | yes |
| int8/int8 | 64 MiB | 0 | 254.0 | 275.1 | no |
| int8/int8 | 64 MiB | 2 | 255.4 | 274.0 | no |

At 16 and 1,024 groups, Rust requests had lower medians than serial native SQL.
Native parallel plans had lower medians than Rust throughout those smaller-group
cases. Higher work memory did not reverse that result.

## Limits and evidence

Work memory constrains PostgreSQL plan nodes and the function output tuplestore.
Rust's hash table uses a cardinality budget and does not enforce `work_mem` or
an RSS quota. These are explicit execution controls, not proof of equal private
memory use. The evidence supports retaining native execution as the normal SQL
route and keeping the Rust API opt-in. It does not justify automatic planner
substitution or a broad analytical speed claim.

The [versioned evidence](evidence/grouping-memory-linux-2026-10-31.json) retains
all raw request and server samples, final plans, actual memory settings, group
limits, source fingerprint and original report SHA-256. Repeat these controls
on matched hosts before attributing small differences to implementation changes.
The failed short-hash run stopped during checkout and supplied no benchmark data.

The separate held-snapshot throughput failure remains unresolved. This isolated
grouping comparison does not test or repair that behavior.
