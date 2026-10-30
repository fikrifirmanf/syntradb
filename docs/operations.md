# Operations runbook

SyntraDB is a prototype, and this runbook is for a pilot. It collects what the
other documents measured and checked, and links to them. Where a step depends on
a number, the number comes from one laptop. Measure again on the pilot's own
hardware before relying on it.

## Before enabling capture

- Run `bash scripts/check.sh` on the build you install. It has passed on macOS
  arm64 and Ubuntu 24.04 arm64 and x86_64. A native GitHub-hosted x86_64 run
  passed 193 PostgreSQL tests at commit `1baa167`; see the
  [verification record](verification-2026-10-31.md).
- Capture accepts permanent heap tables with one int2, int4 or int8 primary key
  and one int2, int4, int8 or `numeric(p,s)` value column, with p ≤ 18. See
  [transactional capture](transactional-capture.md) for the full list of
  rejected tables.
- Each database allows 16 captured sources. A base holds up to 100,000 rows and
  a feed up to 4,096 unpruned events. A key move uses two events.
- Set bounded `lock_timeout` and `statement_timeout` for the roles that run
  maintenance. Setup holds a catalog lock that can briefly block captured
  writers on other sources.
- Set `idle_in_transaction_session_timeout` and `max_slot_wal_keep_size`.
  SyntraDB does not end old snapshots or idle slots itself.

## Keep feeds drained

Something must run `syntra_apply_prune_capture(source)` regularly. Without it,
readers fall back to the heap, and writers fail once the feed holds 4,096
events. There are two ways to run it:

- The opt-in [background worker](background-maintenance.md), one per listed
  database and at most 8. It needs `shared_preload_libraries` and a restart.
  One 600-second run at 200 transactions per second kept one feed drained.
  Each source transaction has a one-second lock timeout so a source DDL lock
  cannot block unrelated feeds indefinitely.
- A scheduled call from a client session, as a superuser, in a writable read
  committed transaction.

## What to watch

| Signal | Query | Act when |
|---|---|---|
| Lag and headroom for every source | `SELECT * FROM syntra_capture_status()` | `pending_events` keeps growing, `free_events` falls toward 0, or `error` is set |
| Cleanup horizon | `SELECT syntra_cleanup_horizon_age()` | Alert above 5,000; page above 10,000 |
| Worker failures | Server log: `syntradb maintenance skipped relation` | It repeats on every pass |

`syntra_capture_status()` lists the sources the caller holds a privilege on. Its
lag and headroom reads are taken one after the other, not from one snapshot.
Headroom counts committed events only, so open transactions can still use the
free events. The horizon thresholds come from
[the snapshot age policy](resource-bounds.md#snapshot-age-policy).

## Errors and what to do

From 0.6.0 the capture errors below are raised when the transaction commits,
not by the statement that changed the row. `COMMIT` fails and the whole
transaction rolls back, so retry the whole transaction. A savepoint around the
statement does not catch them.

| Error | Meaning | Action |
|---|---|---|
| `capture log is full or registration changed; apply and prune or reseed before writing` (54000) | The feed holds 4,096 events, or capture changed under the writer | Run `syntra_apply_prune_capture`, then retry the write. If the feed does not drain, find out why before reseeding |
| `checkpoint maintenance is busy; retry after its transaction ends` (55P03) | Another apply or prune holds the maintenance lock, or a vacuum held it for over a second | Retry. The worker skips the source until its next pass |
| `capture configuration is missing or changed; reseed before writing` | A captured column changed, for example its `numeric` precision | Reseed with `syntra_seed_capture` |
| `source tracking registration is missing; stop writes and repair the registration` (55000) | A logical restore omitted tracking state, or private registration is damaged | Reseed a captured source with `syntra_seed_capture`; re-register a tracked-only source with `syntra_track_source` |
| `capture cannot store NaN in a numeric column` | A write tried to store `NaN` in a captured `numeric` column | Fix the writer. The transaction rolled back |
| Deadlock (40P01) or serialization failure (40001) | Captured writers to one source take the feed head lock in turn | Retry the whole transaction |
| `WARNING: syntradb maintenance skipped relation <oid>: <reason>` | Apply failed on that source; the worker moved on | Read the reason. A tampered or broken feed needs a reseed |

Reseeding discards the feed's history and takes a fresh base. Retiring with
`syntra_stop_capture` removes capture and keeps the source table. Both are
explicit. SyntraDB never drops events on its own to admit a write.

## Backups, standbys and restores

- Physical base backups and point-in-time restore keep capture history and
  checkpoints. A restored cluster keeps capturing, and an apply and prune drains
  it. See [recovery](recovery.md#physical-backup-and-point-in-time-restore).
- Standbys serve checkpoint reads and refuse maintenance. A promoted standby's
  worker drains new writes. Synchronous, cascading and quorum setups each
  passed their checks. For `synchronous_standby_names`, `ANY 1` with two stalled
  links lost acknowledged writes, so size the quorum for the failures you
  expect. See [recovery](recovery.md#quorum-synchronous-commit).
- Logical dumps omit capture history. After `pg_restore`, captured tables
  reject writes until you reseed them.

## Upgrades

Install the new library, then run `ALTER EXTENSION syntradb UPDATE` in every
database that has the extension. Local checks cover upgrades from 0.1.0 through
0.6.0 to 0.7.0. Downgrade checks cover 0.7.0 to 0.6.0 and earlier supported
paths. A downgrade from 0.4.0 to 0.3.0 refuses while a `numeric` feed exists.
See [upgrades](upgrades.md) and the current
[verification record](verification-2026-10-31.md).

## Not covered

These have not been tested, and this runbook gives no procedure for them:

- power loss, torn writes and faulty storage;
- real network partitions and multi-machine clusters;
- packages, and monitoring system integration beyond the SQL signals above;
- one 20-minute `numeric` soak stopped with a pruner error whose cause was not
  recorded. See [the soak](checkpoint-soak.md).
