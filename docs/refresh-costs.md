# Refresh cost measurements

```sh
bash scripts/refresh-costs.sh
```

This optional probe runs after the ordinary benchmark checks. It measures native
and tracked two-row transfers, capture, changed publication and unchanged
publication. Defaults are 10,000 rows and 51 samples per case after one warmup.
Cases run one at a time in a disposable cluster with release builds.

The probe also measures a two-row transfer with keyed transactional capture.
That source uses at most 100,000 base rows and the 4,096-event budget. Its wrapper
includes both row-event append and statement tracking in the WAL measurement.

Normal prepared requests supply client roundtrip latency. Setup writes and result
checks stay outside each timing. Separate EXPLAIN probes collect WAL records,
full-page images and bytes. The report retains every sample.

All three transfer probes call a PL/pgSQL wrapper. Its root SELECT instrumentation
includes the UPDATE and AFTER STATEMENT tracking trigger. Direct EXPLAIN UPDATE
does not include that trigger's WAL in its root plan counters. Child nodes already
contribute to the root totals; adding their counters would count work twice.

These backend counters exclude the outer commit record. Source changes that
prepare a stale publication are outside its measured call. All three transfer probes
include the same wrapper overhead. The probes do not establish mixed-load tail
latency or a cross-database performance ranking.

VACUUM, REINDEX and source DDL use separate node insert-LSN intervals. Those
intervals can include commit and background WAL. They are not exact backend
attribution and must stay separate from the EXPLAIN counters.

## Reusing a current publication

Publication now returns the existing row count when a fully validated binding
already matches the caller's snapshot. It holds the normal admission lock, checks
all source metadata, and validates the segment before returning. No catalog row
is replaced on this path. A damaged segment still causes a rebuild.

An assigned transaction ID disables this shortcut. A row trigger can see its own
source changes before the statement counter advances, so that case must capture
fresh data. The transaction still needs writable read committed mode and admin
privileges. Ordinary live readers keep their existing heap fallback rules.

Committed-session checks verify unchanged `ctid` and `xmin`, and no assigned XID,
after a repeated current publication. Nested row-trigger checks compare newly
published bytes with native SQL. Temporary mutations that removed the shortcut
or bypassed the XID check both failed those tests.

This reduces redundant rebuild work. Changed sources still need full capture and
publication. The [checkpoint API](durable-checkpoints.md) now applies captured
events without rescanning the source. It now rewrites affected partitions and a
manifest; unchanged maintenance and pruning still validate all partitions.

The [before/after findings](../benchmarks/refresh-costs-2026-10-27.md) include raw
reports, source archives, utility costs and a separate stress/recovery run.

## Durable checkpoint costs

The same opt-in probe now includes native and checkpoint all-row summaries,
two-event apply, unchanged apply and two-event prune. It uses the captured source
with at most 100,000 rows. Initial checkpoint creation, source changes and prior
cleanup stay outside each timing. Every operation is followed by a full aggregate
comparison with native SQL. Root EXPLAIN WAL samples use a separate setup and
execution, rather than instrumenting the timed request.

The report stores these cases under `refresh_costs.checkpoint_costs`. Changed
apply includes decoding, merging, encoding and replacing affected partitions,
plus publishing their manifest.
Prune includes validation and transactional event deletion. A no-op apply still
validates both representations. The [checkpoint findings](../benchmarks/durable-checkpoint-2026-10-27.md)
report those costs separately. They do not measure sustained concurrent replay.

## Reader-tail costs

The probe now adds native controls and public summaries after two, 256 and 257
unapplied source events. Two-event setup uses a balanced transfer. Larger cases
update one key repeatedly. Setup always applies and prunes earlier work first.
The read cases explicitly enable the three-argument overload. The first two API
cases must use replay; 257 events must select the heap. Every
result must match native SQL, including the expected route.

Read cases no longer query the capture head before starting their timer. Their
first head access belongs to the measured operation. Only apply cases query an
expected cursor outside timing. Separate EXPLAIN runs repeat the same setup.
Native and API read samples can include PostgreSQL page-maintenance WAL after
heavy HOT-update churn. No logical checkpoint write is implied by those counters.
