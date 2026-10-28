# Managed fixed projections

Publish a named int4 snapshot, read its summary, refresh it, and delete it:

```sql
SELECT syntra_publish_fixed_projection('balances', 'public.accounts', 'balance');
SELECT generation, rows, non_nulls, sum, min, max
FROM syntra_fixed_projection_summary('balances');
SELECT syntra_publish_fixed_projection('balances', 'public.accounts', 'balance');
SELECT syntra_delete_fixed_projection('balances');
```

The first publication returns generation 1. Refresh replaces bytes and all source
metadata in one row update, then returns the new generation. Concurrent refreshes
use PostgreSQL row locking. Generation records publication order. It does not
claim that the source snapshot is newer than every earlier capture.

Readers see committed row versions under normal PostgreSQL isolation rules.
Refreshes roll back with their transaction. A repeatable-read reader keeps its
old generation after another session commits a refresh. The publisher can read
its own uncommitted refresh. A missing name returns no summary; delete returns
false when nothing matched. NULL arguments retain SQL strict behavior.

The source capture rules still apply: schema USAGE and table SELECT, ordinary
heap tables, one stored int4 column, no RLS, no partitions or inheritance parents,
and no serializable capture. Each artifact holds at most one million rows and
eight MiB. Names contain 1–128 bytes and cannot contain only spaces. Quoted names
are values passed as parameters, never SQL fragments.

## Ownership and storage

The catalog is `syntra.fixed_projections`, keyed by role OID and name. Forced row
security limits ordinary roles to their own records. API calls use invoker rights,
fully qualified objects, and a fixed search path. The extension installs into
`public` and creates the separate `syntra` schema. Reserved schema conflicts make
installation fail rather than adopting existing objects.

Superusers can inspect and change all rows. Role membership and SET ROLE can grant
access to another role's copies under normal PostgreSQL rules. Existing fixed
copies remain readable after source access is revoked, just like exported data.
Revoke access to copies separately when required.

Owners can edit their catalog rows directly. Metadata is therefore descriptive,
not authenticated proof of source identity or freshness. Segment readers validate
bytes before returning results. Neither catalog rows nor matching snapshot text
authorize automatic substitution for a live heap query.

The current install grants catalog DML and function access to PUBLIC. An operator
can revoke those grants for a controlled deployment. Per-role disk quotas and a
total catalog size cap are not implemented. Per-artifact bounds do not make this
a shared service for untrusted tenants.

## Restore and compatibility

The catalog is registered with PostgreSQL's extension configuration dump support.
Native pg_dump/pg_restore preserves its data. Source database and relation OIDs
remain historical after restore; they must never be treated as valid live bindings.
The tested restore creates another database in the same cluster. Cross-cluster
restore needs an explicit owner-OID mapping and remains unsupported. Delete a
role's artifacts before dropping the role; there is no role-drop cleanup hook yet.

The schema is pre-release version 0.1.0. Existing prototype installations need a
fresh test database. No deployed-version upgrade contract is available yet.

These managed copies are the lifecycle foundation for HTAP. Transactional heap
queries remain available for current data. Change tracking, retained source
versions, compaction, and live optimizer eligibility are separate unfinished work.
