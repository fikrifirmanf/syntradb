# Refresh cost findings

The unchanged-publication path now validates and reuses the current segment.
A clean-cluster comparison measured 51 normal prepared requests per case after
one warmup. Both runs used 10,000 source rows, local ARM64 hardware, release Rust
and PostgreSQL 18.6 with assertions off. No compile or test jobs overlapped timing.

| Case | Before ms | After ms | Before WAL bytes | After WAL bytes |
|---|---:|---:|---:|---:|
| native_transfer | 0.0548 | 0.0496 | 142 | 142 |
| tracked_transfer | 0.0699 | 0.0776 | 213 | 213 |
| source_capture | 0.3304 | 0.3270 | 0 | 0 |
| publication_changed | 0.5700 | 0.5943 | 45,676 | 45,684 |
| publication_unchanged | 0.6123 | 0.0665 | 45,650 | 0 |

Times are median client roundtrips. WAL values are separate median backend
EXPLAIN samples. Transfer wrappers include AFTER STATEMENT trigger work.
Commit records and setup writes are outside those backend counters.

The unchanged call was 9.2 times faster in this narrow comparison.
All 51 after-run WAL probes recorded zero records and bytes for that call.
Committed-session checks also verified unchanged catalog ctid/xmin and no XID.
The path still checks source identity, counters, guards and the complete segment.
It does not eliminate validation cost or read IO.

Changed publication still writes about 45.7 KB per measured call. Its median
latency increased in this pair of runs. Tracked transfers also had a higher
median after-run time, while their measured WAL stayed at 213 bytes.
These are single-run local samples, with no broad write-latency improvement claim.

## Utility and concurrent checks

Utility commands use node LSN intervals. Their WAL includes commit and possible
background work, so it cannot be combined with backend samples as exact actor
attribution. The clean after-run utility medians were:

| Utility | Request ms | Node WAL bytes |
|---|---:|---:|
| source_vacuum | 1.2669 | 248 |
| catalog_vacuum | 0.0792 | 888 |
| reindex | 1.4473 | 204,224 |
| source_ddl | 0.1006 | 448 |

A separate 60-second run passed 174,557 aggregate checks with zero unexpected
errors or invariant failures. It also passed process-crash and logical-restore
checks. Its node WAL total was 3,466,858,192 bytes.
Worker operation counts differ from the earlier stress run. These totals do not
establish a same-work WAL reduction or comparative throughput gain.

## Replay work and next gate

The storage crate now has an atomic keyed-int4 replay kernel with row, change
and owned-buffer limits. It checks prior values, sequence gaps, exact retries,
NULLs and key moves. Ten tests include a fixed-seed independent reference model.
The complete suite passes 157 tests. Deliberate unsafe publication, retry,
prior-value, limit, cancellation and measurement mutations all failed checks.

The kernel remains separate from SQL execution. Next comes a private transactional
change feed with a proven base/snapshot boundary, followed by durable atomic
progress and bounded retention. PostgreSQL heap fallback remains active.

## Reproduction

Run `bash scripts/refresh-costs.sh`. See the
[measurement contract](../docs/refresh-costs.md) and
[replay kernel contract](../docs/keyed-replay-kernel.md).

Raw reports and matching source inputs:

- [Before report](results/refresh-costs-before-2026-10-27.json),
  [archive](results/source-variants/refresh-costs-before-2026-10-27.tar.gz),
  [manifest](results/source-variants/refresh-costs-before-2026-10-27.manifest.json).
- [Clean after report](results/refresh-costs-after-2026-10-27.json),
  [archive](results/source-variants/refresh-costs-after-2026-10-27.tar.gz),
  [manifest](results/source-variants/refresh-costs-after-2026-10-27.manifest.json).
- [Stress and recovery after report](results/refresh-costs-stress-after-2026-10-27.json),
  [archive](results/source-variants/refresh-costs-stress-after-2026-10-27.tar.gz),
  [manifest](results/source-variants/refresh-costs-stress-after-2026-10-27.manifest.json).

The archive fingerprints were checked independently against both binaries.
