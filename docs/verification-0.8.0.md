# SQL grouping verification for 0.8.0

The explicit [integer grouping API](heap-grouped-aggregates.md) passed the local
checks below on macOS arm64 with PostgreSQL 18.6 and Rust 1.99.0.
Native Linux results are pending. These checks establish bounded API behavior;
they do not establish production readiness or an analytical speedup.

## Correctness and visibility

`bash scripts/check.sh` passed formatting, workspace Clippy, Rust tests, three
Python counter tests and 218 PostgreSQL tests. Twenty-four PostgreSQL cases
exercise the new grouping path. They compare all nine integer layouts with
native SQL, using duplicate-sensitive multiset comparisons.

The grouping tests cover NULLs, empty input, wide sums, all 65,536 admitted
groups, invalid limits, permissions, unsupported sources and statement snapshots.
Reading the value attribute as the key deliberately made the int4/int4
comparison fail. The correct key mapping was restored before the full check.

The production harness also passed seven independent-session grouping checks:
repeatable-read visibility, own writes, uncommitted isolation, read-only calls,
source DDL locking, serializable rejection and cancellation with subsequent reuse.
Its extension and runner source fingerprints both equal
`1d42140e0db168dbfd23bcbccbb1638bfeea48cf0fb1ea405e1ae84d2adbccdb`.
The local report is `benchmarks/results/heap-grouping-contract-final.json`.
Its small timing workload is a contract check, not a grouping performance result.

The generic missing-test mapper does not recognize the existing `source_tests.rs`
layout. PostgreSQL tests exercise the changed source iterator. The session checks
in `grouping_checks.rs` are integration assertions run by the production harness.

## Version lifecycle

`bash scripts/upgrade-check.sh 0.7.0` passed with pending captured history.
Its 532 catalog entries matched a fresh 0.8.0 install. The catalog comparison
includes function argument names, defaults, security flags and settings.

`bash scripts/upgrade-load-check.sh 0.7.0 30 100` passed with four writers.
All 2,103 committed increments survived, with no failed writes. Heap sum,
captured event count and the final checkpoint agreed. This is a correctness
check under live writes; it is not a throughput result.

`bash scripts/downgrade-0.8.0-check.sh` passed removal and restoration of grouping.
A dependent user view blocked the downgrade atomically. After removing that
dependency, capture stayed exact with each library. Both catalogs matched
fresh installs after the library swaps.

`bash scripts/downgrade-0.7.0-check.sh` also passed from 0.8.0 through 0.6.0 and
back. It waited for an in-flight apply, moved eight deltas inline and matched
fresh catalogs at both versions. The logs are under `benchmarks/results/` with
prefixes `upgrade-070-to-080`, `upgrade-load-070-to-080`, `downgrade-080-fixed`
and `downgrade-080-to-060`.

## Open requirements

The GitHub workflow now contains 24 recovery and lifecycle cases. Its complete
0.8.0 run remains required before merging this feature. Grouping has no planner
substitution, filters, multiple keys or spill to disk. The earlier two-hour
checkpoint soak failed its offered-load limits; this additive API does not fix
that throughput problem.
