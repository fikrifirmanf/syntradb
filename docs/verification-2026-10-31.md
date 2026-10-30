# Verification on 2026-10-31

SyntraDB remains a prototype. This record separates completed checks from queued
or unfinished runs. The [readiness gates](readiness.md) still apply.

## Completed local checks

On macOS arm64, the fixes through commit `7b3609f` passed:

- `scripts/check.sh`: formatting, workspace Clippy, engine and benchmark tests,
  and 194 PostgreSQL integration tests.
- `scripts/maintenance-check.sh`: source DDL lock isolation, SIGTERM during a
  lock wait, restart, busy maintenance locks, broken-source isolation and
  maintenance across two databases.
- `scripts/recovery-checks.sh`: process crash rollback, logical backup and restore,
  rejection of writes before reseeding, and successful capture after reseeding.

The new missing-registration SQLSTATE test failed before the fix and passed
after it. The source-lock check failed before the worker timeout fix: an unrelated
feed stayed pending for the full 30-second assertion window.

## Native Linux build

[Run 41131634854](https://github.com/fikrifirmanf/syntradb/actions/runs/41131634854)
passed the full check on Ubuntu 24.04 x86_64 at commit `1baa167`, including
193 PostgreSQL tests. This supplies a native x86_64 baseline. The patched commit
adds one SQLSTATE regression test and needs its own CI result.

## Rejected benchmark result

[Run 41131630993](https://github.com/fikrifirmanf/syntradb/actions/runs/41131630993)
displayed success but did not complete a soak. Logical restore correctly rejected
a source write with missing tracking registration, but returned SQLSTATE `XX000`
instead of the expected `55000`. The logging pipeline then hid the benchmark's
nonzero exit status. There was no completed JSON report.

The tracking trigger now uses the prerequisite-state error code `55000`.
Both long-running workflows select an explicit Bash shell with pipeline failure
propagation. The soak workflow also checks that a JSON report contains the requested
duration and reports zero unexpected errors and invariant failures.

This run is invalid as performance or soak evidence.

## Runs on the corrected commit

These runs target `7b3609f`; their results were pending when this record was started:

- [Native Linux checks](https://github.com/fikrifirmanf/syntradb/actions/runs/41133222936).
- [Two-hour checkpoint soak](https://github.com/fikrifirmanf/syntradb/actions/runs/41133224378).
- [Recovery and lifecycle checks](https://github.com/fikrifirmanf/syntradb/actions/runs/41133228732),
  covering 21 cases. See the [workflow contract](../benchmarks/github-actions.md).

No completed two-hour result is claimed here. The earlier laptop run stopped at
minute 103 after sleep and unrelated load disturbed it. Comparing 0.6.0 and 0.7.0
still requires matched workloads on comparable hardware.

## Storage fuzz smoke check

The new [decoder fuzz target](../fuzz/README.md) completed 11,453,700 inputs in
61 seconds on macOS arm64 with AddressSanitizer and no failure. Inverting its
checkpoint canonical-encoding assertion made a known fixture fail immediately;
restoring the assertion made that fixture pass. This checks that assertion
failures propagate through the harness. A longer native Linux run is still needed.

## Remaining release requirements

Automatic planner routing, broader analytical operators and types, deployment
packages, a license decision and a support contract remain unfinished. Long
fuzzing, broader load tests and failure tests across real machines still need
evidence. Passing the listed checks cannot establish a complete production database.
