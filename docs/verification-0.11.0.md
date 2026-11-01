# Text and UUID grouping verification

The local PostgreSQL 18.6 suite passed 246 tests, including 16 new grouping
cases. Six Rust kernel tests passed separately. The Python suite passed 18 tests.
Workspace Clippy, formatting and compilation checks passed.
The generic test mapper does not detect the included pgrx test file; the 16
executed PostgreSQL cases cover the new source-scan paths.

The new PostgreSQL cases compare both key types across all three integer value
widths with native GROUP BY. They cover NULL groups, empty strings, Unicode,
trailing spaces, wide sums, UUID spelling normalization, deterministic ICU
collations, rejection of nondeterministic collations, byte budgets, toasted keys,
own writes, modifying CTEs, permissions and RLS.

Two negative controls failed as intended. Disabling the retained-byte guard
failed a kernel budget test. Disabling the collation guard failed the test that
requires rejecting a case-insensitive nondeterministic collation. Both guards
were restored before final verification.

The production harness passed 24 fresh-read modes, 60 conflicting transaction
pairs and eight read-time serialization aborts with backend reuse. Two new
repeatable-read checks preserve the old grouped keys and values across a writer's
key move. Two new cancellation checks return `57014` and then reuse the same
backend successfully. The existing temporal and cached-state guards still pass.
A separate LATIN1 database rejects text grouping while UUID grouping succeeds.

Conflict participants use the custom APIs without a native source oracle in
the same transaction. Native invariants run only after the participants finish.

Local lifecycle checks passed:

- Upgrade from 0.10.0 with pending captured history. All 542 catalog entries
  match a fresh install. Old temporal APIs remain callable before the update;
  new grouping APIs become callable after it.
- Thirty seconds of live upgrade writes. All 1,892 committed writes survive,
  no writes fail, and heap, events and the drained checkpoint agree.
- A 0.11.0 → 0.10.0 → 0.11.0 round trip with both libraries. A dependent view
  blocks downgrade atomically, catalogs match fresh installs, and capture stays exact.

The 0.10.0 baseline comes from `54eea4f`. Its 293 retained regular files match
that Git tree's bytes and modes. Only nested baseline archives were omitted.
Its SHA-256 is
`78cae4610c074f6ebd053b4d2718802ea6857da99371dfcceb614c71991c5b17`.
The old library reports its original source fingerprint during the downgrade.
The [provenance record](../build-support/upgrade-baselines/syntradb-0.10.0-src.json)
lists retained files and exclusions.

The final production runner and library share source fingerprint
`4dcf8997f4b929b5b306856cf7d8b7d53b24886829e28d6d926d3d18a75fdfba`.
The macOS arm64 pilot archive passed both staged and extracted installation
checks, including text and UUID grouped sums. The
[local evidence](../benchmarks/evidence/owned-grouping-local-2026-11-01.json)
records the report and log hashes, package manifest and archive checksum at `031a6f2`.

The [native Linux CI run](https://github.com/fikrifirmanf/syntradb/actions/runs/41276204629)
passed 246 PostgreSQL tests, 18 Python tests and the Rust checks at `031a6f2`.
Both staged and extracted pilot archive installation checks passed. Text and
UUID grouped sums match the fixture, and the loaded fingerprint matches the
local build. The [dependency run](https://github.com/fikrifirmanf/syntradb/actions/runs/41276204657)
also passed. The [Linux evidence](../benchmarks/evidence/owned-grouping-linux-2026-11-01.json)
records terminal jobs, log hashes and the producer's archive checksum.

The expanded [33-case lifecycle suite](https://github.com/fikrifirmanf/syntradb/actions/runs/41276235562)
passed all 33 cases and its planning job at `031a6f2`. The
[terminal job record](../benchmarks/evidence/owned-grouping-lifecycle-linux-2026-11-01.json)
preserves every result. These functional checks make no performance claim and
do not close the earlier held-snapshot throughput gate.
