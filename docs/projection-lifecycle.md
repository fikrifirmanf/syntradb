# Fixed source projections

`syntra_capture_int4(source regclass, column text)` scans a source table directly
and returns one segment together with its provenance. Save that row in a table
you own, along with a name and generation number. The extension does not maintain
a global projection catalog in this milestone.

Use the [SQL lifecycle example](../examples/projection_lifecycle.sql) to create,
read, refresh and delete a named projection. A refresh replaces the segment and
all provenance fields in one `UPDATE`. PostgreSQL publishes the destination row
atomically and rolls it back with its transaction. Existing readers retain their
normal PostgreSQL view of the previous destination row.

## Capture contract

The function uses the caller's active MVCC snapshot. Capture holds `AccessShareLock`
while inspecting the source descriptor and scanning its rows. The scan uses PostgreSQL's
table access API and tuple slots; it never interpolates table or column names into
SQL. Only owned integer values cross into the Rust encoder.

The function is stable and runs outside parallel workers. Capture does not use SPI to
fetch source rows or provenance, so an internal SQL call cannot replace the
snapshot between metadata capture and the scan. PostgreSQL supplies visibility
of committed versions, the caller's earlier writes and aborted changes.

An ordinary read-committed statement sees its statement snapshot. A repeatable-read
transaction retains its transaction snapshot across captures. Serializable capture
is rejected for now. Snapshot checks run with independent reader and writer
sessions before the benchmark reports any timing.

Capture supports ordinary heap tables and non-generated `int4` columns, including
NULLs. It rejects views, foreign tables, non-heap access methods, partitions,
inheritance parents and tables with row-level security enabled. PostgreSQL can
retain the inheritance-parent flag after descendants disappear; capture rejects
such a table conservatively. A capture reads only the specified physical relation.

The effective caller must have schema `USAGE` and table-level `SELECT`. A grant
on only the selected column is insufficient in this prototype. The function runs
with invoker permissions and rejects RLS even for a role that can bypass it.
Saved copies follow destination-table permissions. Revoking access to the source
does not revoke copies that were already saved.

## Provenance fields

| Field | Meaning |
|---|---|
| `source_database` | Database OID at capture time |
| `source_relation` | Locked source relation OID |
| `source_schema`, `source_table` | Source names at capture time |
| `source_column`, `source_attribute` | Column name and positive attribute number |
| `source_type` | PostgreSQL `int4` type OID |
| `source_snapshot` | Text from the active PostgreSQL snapshot |
| `source_transaction` | Assigned full transaction ID, or NULL if none was assigned |
| `source_command` | Command visibility ID from the active snapshot |
| `captured_at` | Start timestamp of the capture statement |
| `source_rows` | Visible source rows encoded, including NULLs |

Capture does not assign a transaction ID just to fill metadata. It can include the
caller's uncommitted writes, as a normal PostgreSQL read can. Persisting the result
in the same transaction rolls back the destination copy if that transaction aborts.

These fields describe a completed fixed copy. They are not an exported snapshot,
a replay token or an eligibility proof. Snapshot text omits some internal visibility
state, and relation OIDs can be reused. A dropped or renamed source does not change
existing bytes. Destination owners can edit their own records; provenance is not
authenticated. No automatic optimizer path trusts this metadata.

## Limits and remaining work

Each capture currently produces one segment, capped at 1,000,000 visible rows.
The encoder streams through one group buffer rather than materializing an SQL array. The
encoded output and PostgreSQL's returned `bytea` still consume memory. Cancellation
and cleanup release the scan and tuple slot before the source relation is closed.

This milestone uses ordinary PostgreSQL rows for destination durability and backup.
This layer does not implement external manifests, background refresh, source invalidation,
retained version histories or committed-change ingestion. Those require a separate
projection manager before transparent live-table acceleration is safe.

The [managed fixed-copy catalog](managed-projections.md) now supplies named
publication, refresh and delete operations with owner isolation. It retains the
fixed snapshot contract described here. [Recovery checks](recovery.md) cover
process crashes and native dump/restore within the same cluster.
