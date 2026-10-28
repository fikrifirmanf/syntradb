# Trusted concurrency findings

One local 60-second run passed with eight client sessions and 10,000 source rows.
Drain finished at 60.017 seconds. The server used PostgreSQL 18.6 with assertions
disabled. Both Rust binaries used release builds. No build or test jobs overlapped
the stress window. This is an initial correctness result, with no comparative
throughput or production readiness claim.

| Check | Observed |
|---|---:|
| Committed writer cycles | 107,699 |
| Rolled-back writer cycles | 10,771 |
| Aggregate comparisons with native SQL | 173,274 |
| Read-only projection hits | 40,359 / 169,452 |
| Successful trusted publications | 58,825 |
| Expected publisher admission conflicts | 637,903 |
| Unexpected errors | 0 |
| Invariant failures | 0 |

Two read committed clients and one repeatable read client checked all rows and
a range. Writer probes checked own writes. Both account transfers and changing
NULL-row counts matched native SQL. Every reader kept its XID unassigned.
The final quiescent publication restored eligible reuse.

The maintenance worker completed every operation:

- `ddl_epoch_change`: 7,929 successful operations.
- `reindex`: 7,930 successful operations.
- `source_vacuum_analyze`: 7,930 successful operations.
- `trusted_catalog_vacuum`: 7,929 successful operations.

## Publication cost
The node generated 4,555,129,600 WAL bytes during stress and drain.
That is 4.56 GB in about one minute. The window includes source writes,
full publications, DDL, REINDEX, VACUUM and background work. No per-actor WAL
attribution was collected. The publishers deliberately retried without pacing.
Admission conflicts consumed attempts without completing a publication.

Allocated source storage changed from
638,976 to 663,552 bytes. The shared trusted catalog changed from
196,608 to 811,008 bytes; after final VACUUM it occupied
851,968 bytes. These totals include earlier fixed benchmark bindings.
VACUUM reclaims space for reuse and can allocate metadata pages. It need not
shrink files. Peak RSS, disk quota enforcement and long-term bloat remain untested.

This result supports snapshot correctness under the declared load. It also shows
that frequent full publication has a WAL cost worth measuring separately.
A production refresh worker needs bounded work and paced admission. Bounded
change replay should be compared with that baseline before automatic refresh.

## Checks and reproduction

The stress oracle was tested by bypassing version matching only for its source.
Earlier checks passed, then a repeatable-read comparison detected stale bytes.
The original engine was restored before this run. Duration tests also rejected
mutated upper bounds. The complete test suite passed 144 tests.

Run `bash scripts/trusted-stress.sh` from the project root. The
[stress contract](../docs/trusted-concurrency-stress.md) describes the workers,
timeouts, failure rules and replay requirements. Results are in the
[raw report](results/trusted-stress-60s-2026-10-27.json). The
[source archive](results/source-variants/trusted-stress-60s-2026-10-27.tar.gz)
and [manifest](results/source-variants/trusted-stress-60s-2026-10-27.manifest.json)
preserve the matching input set.
The independently checked source fingerprint is `f20718ace570df4049a42176c4670648b0e69cc14252b5cc8ae3c67e829354db`.
