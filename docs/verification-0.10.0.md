# Temporal aggregate verification for 0.10.0

The local PostgreSQL 18.6 suite passed 230 tests, including 12 temporal cases.
They compare all three time-column types and all four value layouts with native
SQL. They also cover boundaries, NULLs, wide sums, decimal NaN, BC dates,
infinities, DST, own writes, statement snapshots and access/type rejection.

The production harness passed 18 fresh-read modes, 48 conflicting transaction
pairs, six read-time serialization aborts with backend reuse, three retained
temporal snapshots and three cached/capture guards. Conflict participants do
not run a native source oracle in their transaction. The native invariants run
after participants finish, so the verifier cannot supply a missing dependency.
All six matching/fully-filtered cancellation cases passed with SQLSTATE 57014
and a successful subsequent read on the same backend.

Changing the timestamp upper comparator from `<` to `<=` made the boundary
regression fail. The correct comparator was restored before final verification.
The generic missing-test mapper does not recognize the included pgrx test file;
the 12 executed PostgreSQL cases provide its behavior coverage.

A local 0.10.0 → 0.9.0 → 0.10.0 round trip passed with both libraries. Dependent
views blocked removal of the new API atomically. Fresh and migrated extension
catalogs matched at each version, and capture replay remained exact.
The separate 0.9.0 upgrade check matched all 538 catalog entries to a fresh
0.10.0 install. A 30-second live upgrade preserved all 1,892 committed writes;
no writes failed, and heap, event count and the drained checkpoint agreed.

The old 0.9.0 source archive comes from `4091455`. Its 282 retained regular files
match the original Git bytes and modes. Only nested baseline archives were
omitted. The archive's SHA-256 is
`6209e1ce32644ad373c1cc733113fc52565017977a6fdbd13ace9bcaf043abd4`.
The old loaded library reported its original source fingerprint during the
round trip. The [baseline provenance](../build-support/upgrade-baselines/syntradb-0.9.0-src.json)
lists every retained file and exclusion.

Native Linux CI, pilot archive installation and the expanded 30-case lifecycle
suite remain pending. No performance claim or throughput-gate closure follows
from these functional checks. The earlier held-snapshot load failure remains open.
