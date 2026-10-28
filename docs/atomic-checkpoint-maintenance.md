# Atomic checkpoint apply and prune

```sql
SELECT applied, pruned
FROM syntra_apply_prune_capture('source_table'::regclass);
```

The new administrative function applies the visible captured prefix and prunes
through that applied ordinal in one PostgreSQL transaction. `applied` is the
capture epoch's durable event ordinal; `pruned` is the number of deleted event rows. Retrying a
settled source returns the same ordinal and zero deleted events. An error rolls
back checkpoint chunks, the manifest, pruning progress and event deletion.

Admission matches the separate functions: superuser only, writable read committed
transactions, a source relation lock and nonblocking maintenance serialization.
PUBLIC has no EXECUTE grant. Granting EXECUTE does not bypass the runtime
superuser check. Readers and their opt-in replay behavior are unchanged.

The function captures configuration, head and manifest once. If already applied,
it validates every persisted chunk, digest and representation once before pruning.
This proof is local to that function call. There is no cache across statements,
snapshots or sources.

After a changed publication, the function takes a fresh snapshot to read its own
stored header and chunks. It verifies the captured target, binding, row count,
digests and representations before pruning. This catches bytes altered by catalog
triggers during publication and damage in inherited clean partitions. The changed
path retains this validation even when it limits performance gains.

Pruning uses the original head's floor and target. Producers may commit later
events while publication runs. Those events remain pending, and the head's newer
emitted cursor is preserved. Old readers retain their source, checkpoint and event
versions through PostgreSQL MVCC. Other maintenance stays serialized until the
caller's transaction commits or rolls back.

Fresh extension installation creates the SQL function and its ACL. Existing
databases need an explicit extension migration or a fresh development installation;
automated release upgrades are not provided yet. The stored format is unchanged.

The default stress path still uses separate calls. To compare the combined path:

```sh
bash scripts/checkpoint-combined.sh --recovery-checks true
bash scripts/checkpoint-paced.sh --checkpoint-worker-rate 100 --checkpoint-combined-maintenance true
```

Appliers still alternate apply-only with apply-plus-prune cycles, leaving work for
the independent pruner. `--checkpoint-combined-maintenance true` selects the new
function only for the latter cycles. False is the default; parsing accepts only
`true` and `false`. All existing rollback, old-reader, keyed reconstruction,
pruning-accounting and progress gates remain active.

The cost probe adds changed and unchanged combined cases. Baselines call the two
separate functions through a materialized CTE that orders apply before prune.
Both use one prepared SQL round trip and one implicit transaction. Result checks
verify applied ordinals, deleted counts, the empty retained feed and native
summaries. Separate EXPLAIN probes collect statement WAL outside request timing.
This comparison measures shared work inside the database, without crediting the
combined API for eliminating a client round trip.

The [measured results](../benchmarks/atomic-maintenance-2026-10-27.md) show faster
unchanged-state maintenance and limited changed-state gains, with mixed-load
results and all source inputs retained.
