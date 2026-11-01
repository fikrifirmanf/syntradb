# Version 0.12 verification

The grouped decimal implementation merged to `main` through
[PR 1](https://github.com/fikrifirmanf/syntradb/pull/1) as merge commit
`cc1b003`. This record makes no throughput or production-readiness claim.

## Local checks

The [local evidence](../benchmarks/evidence/grouped-decimal-local-2026-11-01.json)
records implementation commit `1c9c5de90f5a036bfb6abca8696b84839a6d81d1`.
The macOS arm64 release runner, extension and pilot bundle share source fingerprint
`c4f202e48a07aed287a869fcf364759db684fa96f6850a50543c0d65d0e65a13`.

The final local check passed 274 PostgreSQL tests, including 28 decimal tests.
It also passed 18 Python tests, Rust tests, formatting and Clippy. Complete native GROUP BY comparisons cover all five key layouts at scales
0, 2 and 18. Boundary cases cover NaNs, NULLs, precision, limits, permissions,
collations, toasted keys and modifying statements. Five new Rust tests cover the
decimal kernel. Negative controls broke NaN handling and a decimal sum assertion;
both tests failed before restoration.

The native production driver passed 33 serializable read modes, 78 conflicting
transaction pairs and 11 read-time serialization aborts with backend reuse.
Decimal calls also passed three retained key/value snapshot checks and three
cancellation/reuse checks. A LATIN1 database refused decimal text grouping and
returned the exact UUID-grouped sum.

The local 0.11-to-0.12 upgrade preserved pending capture history and matched all
548 catalog entries against a fresh installation. The reverse round trip used
both libraries and refused a dependent user view atomically. The upgrade under
load committed 2,214 writes without failures; its SQL update took 4 ms.
These lifecycle checks exercised the implementation before two test-only comments
were added. The local evidence identifies that earlier commit explicitly.

The final pilot archive has SHA256
`558376d58b434414dce666981027c6265f626e08caae6691c6069b62719836d5`.
The staged files and extracted archive each installed into a private PostgreSQL
cluster. Capture, checkpoint, temporal, integer grouping and decimal grouping
checks passed. No external database was deployed.

Backend, compile, test and copy audits passed. The backend audit's 66 warnings
are unwrap assertions inside PostgreSQL tests. The missing-test mapper does not
recognize included pgrx tests or the native driver; their execution supplies the
coverage evidence above.

## GitHub checks

All Linux and dependency checks passed:

- [Rust, PostgreSQL, pilot bundle and native concurrency checks](https://github.com/fikrifirmanf/syntradb/actions/runs/41291238535)
  on the PR head `86ba653`, and again on [`main` after the merge](https://github.com/fikrifirmanf/syntradb/actions/runs/41310691858).
- [Dependency advisories](https://github.com/fikrifirmanf/syntradb/actions/runs/41291238476)
  on the PR head, and again on [`main` after the merge](https://github.com/fikrifirmanf/syntradb/actions/runs/41310691857).
- [All 36 recovery and lifecycle cases](https://github.com/fikrifirmanf/syntradb/actions/runs/41291005719),
  run on `1c9c5de`. The later `86ba653` commit only added this record.

The existing held-snapshot throughput gate remains failed. These correctness
checks do not replace that performance gate.
