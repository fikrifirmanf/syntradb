# Transactional capture findings

The opt-in capture path passed 175 tests and eleven committed-session scenarios.
Process-crash checks retained committed base rows and events. Uncommitted source
changes and cursor reservations disappeared after restart. Logical restore omitted
capture history and required explicit reseeding before writes resumed.

The row reconstruction check uses a fresh Rust replay state and compares native
PostgreSQL rows at the same snapshot. It tests a writer that commits while seeding
waits, repeatable-read prefixes, own writes, rollback and savepoints. Concurrent
writers also commit in the reverse of their numeric XID order. Event ordinals
preserve the real dependency order.

The first implementation rejects other source triggers and foreign-key triggers.
Nested callbacks can reverse tuple-change order relative to row capture. Those
sources stay outside this scope. Capture retirement preserves source data and
ordinary statement tracking; its rollback restores both guards and history.

## Local write cost

This run used 10,000 source rows and 51 samples per case after one warmup.
PostgreSQL 18.6 had assertions off; Rust used release builds on local ARM64.
No compile or test jobs overlapped timing.

| Route | Median request ms | Median statement WAL bytes |
|---|---:|---:|
| native_transfer | 0.0569 | 142 |
| tracked_transfer | 0.0691 | 213 |
| captured_transfer | 0.2265 | 965 |
| source_capture | 0.3261 | 0 |
| publication_changed | 0.5589 | 45,692 |
| publication_unchanged | 0.0640 | 0 |

Transfer wrappers include row and statement triggers. Timings are normal prepared
client roundtrips; WAL comes from separate root SELECT EXPLAIN probes. Commit
records and setup work stay outside those backend counters. The captured route
also verifies that all measured transfers produced the expected event count and
source values, including warmup and EXPLAIN calls.

Capture adds write overhead compared with statement tracking alone. Its cursor
serializes writers per source, and a full log rejects writes. These component
measurements do not establish fresh HTAP throughput or a speed advantage over
another database. Capture remains explicitly opt-in.

## Bounds, checks and next step

Limits are sixteen sources, 100,000 seed rows and 4,096 retained events per source.
There is no automatic pruning. Admin reseeding and retirement discard history
explicitly. Other triggers, foreign keys, RLS, inheritance and TRUNCATE remain
outside the installed capture scope. See the
[capture contract](../docs/transactional-capture.md) for maintenance and restore behavior.

Deliberately using an old seed snapshot omitted a committed writer and failed
row reconstruction. Using old metadata allowed a DDL gap and failed its check.
Budget, retirement and trigger-scope mutations also failed. The assertion-enabled
server caught an unpinned metadata snapshot during development; the final code
pins it through both scans.

Next, persist materialized rows and applied progress together. Then test crash
during apply, retained old readers and bounded pruning. Existing live SQL still
uses trusted publication or heap fallback. The log is durable; an incremental
materialized replay checkpoint and SQL replay routing remain unfinished.

## Reproduction

Run `bash scripts/refresh-costs.sh --recovery-checks true`. The
[raw report](results/transactional-capture-2026-10-27.json),
[source archive](results/source-variants/transactional-capture-2026-10-27.tar.gz)
and [manifest](results/source-variants/transactional-capture-2026-10-27.manifest.json)
preserve the measured build. Its source fingerprint was checked independently
against both binaries. The default probe measures write costs without a saturated
capture soak or an equal-offered-load latency comparison.
