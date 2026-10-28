# Catalog lookup probe

Checkpoint readers now probe each manifest reference by source, epoch, partition
and revision. The `capture_chunks_version_index` supports these 64 exact probes.
One native scan retains its index handle and snapshot across rescans. Keys map
from table attributes to index positions and sort before each scan. Payload bounds,
digests, row counts, representation checks and source visibility still apply.

A missing version index rejects checkpoint loading. Public reads can use their
heap fallback; maintenance fails closed. Fresh extension installation creates the
index. Existing installations need a schema upgrade before using this binary;
there is no automated release upgrade yet. Do not replace binaries in place and
assume the catalog has migrated.

Run the diagnostic in its disposable PostgreSQL cluster:

```sh
bash scripts/catalog-probe.sh --repetitions 31 --recovery-checks true
```

The flag `--catalog-probe-checks true` enables this probe in the benchmark runner.
It is off by default. A read-only repeatable-read session pins an old source
snapshot while another session commits 0, 64, 256 and 1,024 churn cycles. Each
cycle changes two keys, applies the checkpoint and prunes events. Writers pause
for each set of samples. Samples cover a current checkpoint and a two-event tail.
After the old reader exits, VACUUM runs and both paths are sampled again.

Every sample checks all aggregate fields against native SQL, the expected route
and absence of an assigned XID. The old reader must retain its original answer.
Backend-local counters cover the measured requests plus one warmup. Divide each
counter delta by `counter_requests_including_warmup` for per-request work.
Index tuple counters measure physical entries visited, including invisible
versions. Heap block counters count buffer accesses, including repeated hits.
The table tuple-fetch counter is bitmap-specific and can be zero during these
index scans; it does not count visible chunk rows. Separate EXPLAIN output is
excluded from these counter deltas. Prepared round-trip samples exclude warmup.

These measurements isolate retained-version lookup work. They do not establish
concurrent throughput or cross-database speed. See the
[results and remaining costs](../benchmarks/catalog-lookups-2026-10-27.md).

Head, manifest and capture configuration readers traverse their existing
primary-key indexes backward. MVCC selects the
single visible row for the source key. Tuple position does not identify the
newest logical version, and page reuse can reduce the benefit. An old snapshot
may visit newer invisible entries before reaching its own version. Missing
metadata indexes retain the existing heap scan behavior. No new index or stored
format is required for this traversal change.

The [metadata traversal findings](../benchmarks/metadata-traversal-2026-10-27.md)
retain controlled before/after work and two paced trials per build and rate.

The probe now times the retained reader at each depth as well. These samples
reuse its active repeatable-read transaction without BEGIN or COMMIT. Each
summary must match native SQL at that snapshot and leave its XID unassigned.
After each sample set, the native result must still equal the original source
answer. This detects an accidentally ended historical transaction.

Each churn cycle now has a round-trip timing for its implicit transaction:
source transfer, checkpoint apply, event prune and commit. The recorded vector
contains only newly requested cycles since the previous stage. It excludes
reader sample work. Tail settlement separately times apply, prune and COMMIT
within one explicit transaction; pruning must remove exactly two events.
There is one settlement per stage, so these individual timings do not estimate
a latency distribution. The settlement protocol differs from older reports,
which sent apply and prune in one batch. Compare the same probe version across
traversal variants.

An archived experiment used the latest head's xmin and PostgreSQL snapshot
membership to choose traversal direction, with a 128-XID gap heuristic. It
reduced historical lookup work in the controlled probe, but mixed-load trials
did not establish acceptable writer behavior. The runtime retains unconditional
reverse traversal. See the [historical findings](../benchmarks/historical-readers-2026-10-27.md)
for all variants and the rejected policy's scope.
