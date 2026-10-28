# Upgrades

SyntraDB ships upgrade scripts as `crates/syntra-pg/sql/syntradb--OLD--NEW.sql`.
`cargo pgrx install` copies them next to the generated install script.

```sql
-- After installing the new library, in each database that has the extension:
ALTER EXTENSION syntradb UPDATE;
```

## 0.1.0 to 0.2.0

0.2.0 changes how capture writers record their position, to stop a long-held
snapshot from slowing them down. See
[transactional capture](transactional-capture.md#base-boundary-and-write-contract).

The script:

- adds `syntra.capture_bindings`, a copy of each feed's configuration, and fills
  it from `capture_heads`;
- adds `syntra.capture_marks`, empty, so each feed's position is its head row
  until its next write;
- points the base, event, checkpoint and chunk foreign keys at the bindings;
- revokes public access to both new tables;
- adds `syntra_capture_lag(source)`, which reports how far a checkpoint trails
  its feed;
- adds `syntra_capture_status()`, one row per captured source the caller can
  inspect, and its helper `syntra_captured_sources()`.

Existing feeds, unapplied history and checkpoints carry over. No reseed is needed.

Order of operations:

1. Install the 0.2.0 library and scripts.
2. Restart PostgreSQL, or at least end every session that loaded the 0.1.0
   library. A session still running 0.1.0 code after the update keeps reserving
   ordinals from the head row and can pick one a 0.2.0 writer already used.
   Mixing library versions is not supported and was not tested.
3. Run `ALTER EXTENSION syntradb UPDATE` in each database.

From step 1 until the update in step 3 commits, capture writes on the new
library fail with `capture configuration is missing or changed; reseed before
writing`, and their source changes roll back. That includes writes that arrive
while the update transaction is still open; they do not wait for it. Do not
reseed: run the update, then retry the writes. Plan the restart and update for a
window when writers to captured tables can pause or retry.

The update itself is short. `scripts/upgrade-scale-check.sh [sources] [rows]
[pending]` upgrades a 0.1.0 database with that many sources, each at the row
limit with unapplied history, and times `ALTER EXTENSION UPDATE` on the server
clock. On one laptop (`benchmarks/results/upgrade-scale-2026-10-28.log`):

| Sources | Rows each | Pending events each | Update |
|---|---|---|---|
| 2 | 1,000 | 100 | 308, 567 ms |
| 8 | 100,000 | 3,000 | 118 ms |
| 16 | 100,000 | 4,000 | 164, 166, 183 ms |

The two small runs were slower than the large ones, so on this laptop run-to-run
noise outweighed the source count. Treat these as "well under a second", not as a
scaling curve.

The update copies one binding per source and adds empty tables. It does not read
the source tables, but re-adding the foreign keys validates every base row,
event, checkpoint and chunk row, so its time grows with the capture catalogs.
After it, every source still reported its
pending events and drained to a checkpoint that matched its heap. The outage
writers see also includes the restart and the time to reach each database, which
depend on the deployment and were not measured.

## 0.2.0 to 0.3.0

0.3.0 adds capture of `int8` key and value columns. See the
[int8 design](int8-design.md).

The script:

- adds `key_width` and `value_width` to `syntra.capture_bindings`, set to 4 for
  every existing feed;
- changes the stored keys and values in `syntra.capture_base_rows` and
  `syntra.capture_events` to `bigint`, which rewrites both tables;
- adds `syntra_checkpoint_int8_summary(source, column [, replay_tail])`.

Existing `int4` feeds keep their checkpoint and segment bytes, so no reseed is
needed. Their unapplied history and checkpoints carry over.

Order of operations is the same as for 0.2.0: install the library, restart or end
every session that loaded 0.2.0, then run `ALTER EXTENSION syntradb UPDATE` in
each database.

Between the restart and the update, capture writes fail closed with `capture
configuration is missing or changed; reseed before writing`, because the new
library needs the width columns. A write that arrives while the update
transaction is open waits for it: the update holds an exclusive lock on
`capture_bindings`. After commit the write goes through and is captured. Updating
straight from 0.1.0 runs both scripts in one transaction, and there such a write
fails closed instead, as in the 0.1.0 section above.

This update is slower than 0.2.0's because of the rewrite. Its time follows the
number of base rows (rows per source times sources) plus retained events. On one
laptop (`benchmarks/results/upgrade-scale-0.3.0-2026-10-28.log`):

| From | Sources | Rows each | Pending events each | Update |
|---|---|---|---|---|
| 0.2.0 | 8 | 100,000 | 1 | 683 ms |
| 0.2.0 | 16 | 100,000 | 3,000 | 1,256, 1,289, 1,511 ms |
| 0.1.0 | 16 | 100,000 | 3,000 | 1,394, 2,539 ms |

Writers to captured tables block or fail for that long. The rewrite also needs
free disk space for a second copy of both tables until it commits.

## 0.3.0 to 0.4.0

0.4.0 stores small checkpoint changes as partition deltas instead of rewriting
whole partitions. See the [delta design](checkpoint-delta-design.md).

The script:

- adds a nullable `delta` column to `syntra.capture_chunks`. NULL means no delta,
  which is true of every existing partition, so no checkpoint is rewritten;
- lets `syntra.capture_checkpoints.manifest` hold the 8,272-byte manifest
  version 2 as well as the 5,200-byte version 1.

Existing feeds keep their checkpoint bytes and need no reseed. A checkpoint with
no deltas is still written as manifest version 1.

Install the library, restart or end every session that loaded 0.3.0, then run
`ALTER EXTENSION syntradb UPDATE` in each database. Unlike the earlier updates,
capture writes neither fail nor wait: 0.4.0 does not change the event format, and
the update locks only the two checkpoint tables. Between the restart and the update:

- capture writes succeed and are captured;
- apply and prune fail, because the new library writes the `delta` column, so
  history accumulates until the update. A full 4,096-event log then refuses
  writes, as at any other time;
- checkpoint reads fall back to the heap, because the new library cannot find the
  `delta` column. Results stay exact.

The update adds a column without a default and replaces one check constraint, so
it does not grow with source size: 93 ms for 16 sources of 100,000 rows with
3,000 pending events each (`benchmarks/results/upgrade-scale-0.4.0-2026-10-28.log`).
From 0.2.0 the same sources took 1,863 ms, nearly all of it the 0.3.0 step.

The script also adds `syntra_fold_capture(regclass)`, used by the downgrade below.

## 0.4.0 to 0.3.0 (downgrade)

0.3.0 cannot read partition deltas or version 2 manifests, so a downgrade first
rewrites every checkpoint in the 0.3.0 format. With the 0.4.0 library still
installed:

1. Run `SELECT syntra_fold_capture(source_relation::regclass) FROM
   syntra.capture_checkpoints` in each database. For each feed it applies pending
   history, writes a new base for every partition that has a delta, checks the
   stored bytes and returns the number of partitions folded. The result is a
   version 1 manifest with no deltas.
2. Run `ALTER EXTENSION syntradb UPDATE TO '0.3.0'`. The script locks the two
   checkpoint tables, refuses with `partition deltas remain` if any feed still
   has a delta or a version 2 manifest, and with `numeric capture feeds remain`
   if any feed has a `numeric` value column, which 0.3.0 cannot capture; stop
   those feeds first. It then puts back the 0.3.0 manifest length check and
   drops `syntra_fold_capture` and `syntra_checkpoint_numeric_summary`.
3. Install the 0.3.0 library, then restart or end every session that loaded
   0.4.0.

Maintenance that runs between steps 1 and 2 can write new deltas, and the
update then refuses; fold again. Capture writes do not wait for the update,
because it locks only the checkpoint tables. Between steps 2 and 3, apply and
prune on the 0.4.0 library fail and history accumulates, as between the
library swap and the update in the other direction.

The `delta` column stays, NULL in every row. 0.3.0 neither reads nor writes
it, and 0.4.0 reads chunk rows by attribute number: a first version of the
script dropped the column, and after a second update to 0.4.0 every
checkpoint read and apply failed closed with `invalid_checkpoint`, because the
re-added column had a new attribute number. The 0.3.0 to 0.4.0 script now
adds the column only if it is missing.

`scripts/downgrade-check.sh` creates two feeds on 0.3.0, one `int4` and one
`int8`, updates to 0.4.0 and writes deltas. It then checks that:

- the downgrade refuses while deltas remain and changes nothing;
- a seeded `numeric(10,2)` feed blocks the downgrade until it is stopped;
- after the fold, a capture writer does not wait for the downgrade, which is
  held open for two seconds;
- the catalog then matches a fresh 0.3.0 install except for the kept `delta`
  column and its length check;
- history captured on 0.4.0, including that write, replays exactly on the 0.3.0
  library, and apply and prune drain both feeds;
- a second update to 0.4.0 reads the checkpoints 0.3.0 wrote and writes deltas
  again.

Two runs passed, with the writer waiting 84 and 83 ms
(`benchmarks/results/downgrade-check-2026-10-28.log`). With the delta check
disabled, the downgrade was still refused, by the manifest length check, and
the check reported the other reason. A third run, with the `numeric` step,
passed with an 81 ms wait, and a guard that matched `money` instead of
`numeric` failed it (`benchmarks/results/numeric-checks-2026-10-28.log`).

Only this one step back has a script. Going below 0.3.0 would need a script
that undoes the 0.3.0 rewrite of the capture tables, and there is none.

## Upgrade check

```sh
bash scripts/upgrade-check.sh          # from 0.1.0
bash scripts/upgrade-check.sh 0.2.0    # from 0.2.0
bash scripts/upgrade-check.sh 0.3.0    # from 0.3.0
```

The check builds the archived source
(`build-support/upgrade-baselines/syntradb-<from>-src.tgz`) and this tree into a
private copy of the production PostgreSQL install. On the old release it seeds a
feed, applies and prunes some history, and leaves five events unapplied. It then
swaps in the new library and checks that:

- from 0.1.0 and 0.2.0, a capture write is rejected before the update, with its
  source change rolled back; from 0.3.0 it is captured, maintenance fails, and a
  read falls back to the heap and matches it;
- a write sent while the update transaction is open fails closed from 0.1.0, from
  0.2.0 waits at least a second for the update and is then captured, and from
  0.3.0 is captured in under a second;
- after the update, `extversion` is current and the bindings match the heads;
- the checkpoint read replays the old history and matches the heap exactly;
- new writes, apply and prune, lag and status counts, headroom, stop and reseed
  work;
- an `int8` feed seeded after the update replays and checkpoints a sum past the
  `bigint` range;
- the drain after the update writes partition deltas, and the drained feed reads
  from its checkpoint;
- the upgraded catalog equals a fresh install of the current version: columns,
  constraints, indexes, ACLs, functions, event triggers and extension membership.
  Omitting one index from the script made this comparison fail.

`scripts/upgrade-scale-check.sh` takes the starting version as a fourth argument
(default 0.1.0). On 2026-10-28 the check passed from 0.1.0, 0.2.0 and 0.3.0 to
0.4.0, with 500 catalog entries identical to a fresh install
(`benchmarks/results/upgrade-check-0.4.0-2026-10-28.log`).

## Upgrade under load

```sh
bash scripts/upgrade-load-check.sh [from-version] [seconds] [tps]
```

The check installs the new library over a running 0.1.0, 0.2.0 or 0.3.0 database and
starts four writers on a captured 5,000-row feed at once, with a loop that applies
and prunes twice a second in place of the maintenance worker. Four seconds later it
runs `ALTER EXTENSION UPDATE`. Every write is counted as it succeeds or fails. The
check passes only if:

- from 0.1.0 or 0.2.0, some writes failed closed before the update commits, so the
  load overlapped it, and all of them failed with the `capture configuration is
  missing or changed` message; from 0.3.0, no write failed;
- writes succeeded after the update, and none failed with another error, "capture
  log is full" included;
- the heap sum equals the starting sum plus the number of writes that succeeded, so
  no acknowledged write was lost and none was applied twice;
- the number of captured events equals the writes that succeeded plus the history
  that was pending;
- a checkpoint read after the load matches the heap, and after a drain the feed
  reads from its checkpoint with nothing pending.

Two 60-second runs passed, from 0.2.0 and from 0.1.0: about 3,800 writes
committed and 250 to 280 failed closed in each
(`benchmarks/results/upgrade-load-2026-10-28.log`). The update took 15 to 25 ms
because the feed has 5,000 rows; the 0.3.0 rewrite time at scale is in the table
above. Without the apply-and-prune loop, a 60-second run at this rate filled the
4,096-event log, and writers failed closed with "capture log is full" until
someone drained it. A deployment that upgrades under load needs the worker or an
equivalent running.

To 0.4.0 (`benchmarks/results/upgrade-load-0.4.0-2026-10-28.log`), 60 s at
100 tps: from 0.3.0, 3,956 writes committed and none failed, with the update
taking 13 ms; from 0.2.0, 3,776 committed and 252 failed closed, 15 ms.

This is four writers on one small feed with one psql per write. It does not
cover many writers, large feeds under load, or the maintenance worker
itself surviving the library swap.

The 0.1.0 baseline is the source as of 2026-10-28, before `capture_bindings`.
The 0.2.0 baseline is the 0.2.0 release tree as of 2026-10-28, before the width
columns. The 0.3.0 baseline is the 0.3.0 tree as of 2026-10-28, with the
admission retry and before partition deltas. It
already had `syntra_checkpoint_input_bytes`, `syntra_cleanup_horizon_age` and
`syntra_capture_headroom`. Development databases built from earlier source lack
those functions and are not covered; reinstall them.

Not covered: downgrades below 0.3.0, downgrades under load, `pg_upgrade` across PostgreSQL major versions, upgrades
with many writers or large feeds under load (see above for what is covered), and physical backups taken before the update and restored
after it. Physical restore within one release has [its own check](recovery.md#physical-backup-and-point-in-time-restore).
