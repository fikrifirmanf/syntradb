# Serializable fresh heap reads

Version 0.9.0 enables serializable transactions for the explicit fresh heap APIs:

- `syntra_heap_int4_summary` and `syntra_heap_int4_range_summary`.
- `syntra_heap_group_integer_summary` for its supported integer layouts.

These calls use the caller's active MVCC snapshot. Their native PostgreSQL table
scan acquires a source relation predicate lock and checks outgoing serialization
conflicts while examining tuple visibility. Empty results and unmatched ranges
still acquire the relation predicate lock in a read-write transaction.

```sql
BEGIN ISOLATION LEVEL SERIALIZABLE READ ONLY;
SELECT group_key, rows, non_nulls, sum, min, max
FROM syntra_heap_group_integer_summary('readings','category','value')
ORDER BY group_key NULLS FIRST;
COMMIT;
```

## Conflicts and retries

A serialization failure can occur during the fresh scan, a later statement or
commit. It surfaces as SQLSTATE `40001`. Roll back and retry the entire
transaction, including its reads. The extension does not take a newer snapshot
or retry part of a transaction internally.

The sequential scan's relation predicate lock is conservative. A selective
native index plan can have finer predicate locks and different abort rates.
This support does not establish equal concurrency or throughput for every plan.
Normal SQL remains on PostgreSQL's executor.

Read-only and read-only deferrable serializable transactions also work.
PostgreSQL can avoid predicate locks after obtaining a safe snapshot; tests do
not require a lock that this optimization removes. Fresh reads alone do not
assign a transaction ID. Schema privileges, RLS rejection, source DDL locks and
cancellation retain their existing contracts.

## Restricted paths

Source capture, live summary reuse and checkpoint summary reuse still reject
serializable calls with `0A000`. They can read metadata or stored state without
scanning the source heap. Their source dependencies need separate proof before
that restriction can change. Fresh heap support does not enable those paths.

## Version lifecycle

Install the 0.9.0 library, restart sessions that loaded 0.8.0, then run
`ALTER EXTENSION syntradb UPDATE` in each database. SQL signatures, capture
catalogs and stored bytes are unchanged. The update advances the version marker.
If the library is preloaded, restart PostgreSQL to replace the postmaster's loaded copy.
Fresh SSI behavior follows the loaded library, including the window before the
catalog update. Existing trusted projections fail their version check and use
the established fallback.

To downgrade, update the extension to 0.8.0 while 0.9.0 is installed, then install
the 0.8.0 library and restart sessions. That library restores the earlier fresh
SSI rejection. Do not mix loaded library versions across sessions.

## Verification

The integration driver uses real independent PostgreSQL sessions. It checks
nine combinations of fresh API and transaction mode, 21 write-skew and phantom
transaction pairs, three read-time aborts with backend reuse, and three retained
capture/reuse guards. It checks native invariants after participant transactions;
reading a native heap oracle inside a custom participant could mask a missing
dependency and is deliberately excluded from conflict cases.

The old implementation failed acceptance with `0A000`. Bypassing a heap read
made the predicate-lock regression fail. With that separate lock assertion
disabled for a negative control, both writers committed and the independent
conflict regression failed. The correct source read and assertion were restored.
Inverting the expected read-time SQLSTATE also failed its regression.

The [design](serializable-heap-design.md) records the native scan contract.
The [local and native Linux checks](verification-0.9.0.md) pass for 0.9.0.
The earlier held-snapshot throughput limit remains open.
