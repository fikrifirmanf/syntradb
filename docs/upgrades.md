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

## 0.4.0 to 0.5.0

0.5.0 writers to a captured source stop updating its statement-tracking counter
rows. A held snapshot kept every version of those rows, and each write walked
them ([findings](../benchmarks/held-snapshot-writers-2026-10-29.md)). Trusted
reuse of a captured source now compares its feed position, which changes with
every committed row change. See the [proposal](tracking-counter-proposal.md),
option A.

The script adds a nullable `capture_position` column to
`syntra.trusted_int4_projections`. Existing projections get NULL, which matches
a source that is only tracked and never matches a captured one, so captured
projections fall back to the heap until they are published again.

`syntra_source_tracking_status` no longer counts statements on a captured
source; its `generation` stays where seeding left it until capture is retired.

Install the library, restart or end every session that loaded 0.4.0, then run
`ALTER EXTENSION syntradb UPDATE`. Capture writes neither fail nor wait: the
update locks only the trusted catalog. Between the restart and the update,
capture, maintenance and checkpoint reads work as on 0.4.0, and every trusted
read falls back to the heap with `metadata_unavailable`, because the library
cannot find the new column.

## 0.5.0 to 0.4.0 (downgrade)

0.4.0 reuses a projection whose counters match, and 0.5.0 stopped moving them
for captured sources. With the 0.5.0 library still installed:

1. Run `ALTER EXTENSION syntradb UPDATE TO '0.4.0'`. The script deletes every
   trusted projection of a captured source, including those published before
   0.5.0, and adds a check that keeps `capture_position` NULL, so the 0.5.0
   library cannot publish another before the restart. Projections of sources
   that are only tracked stay.
2. Install the 0.4.0 library, then restart or end every session that loaded
   0.5.0.

The column stays, NULL in every row, for the reason the `delta` column stays
in the 0.4.0 downgrade. A later update to 0.5.0 drops the extra check.
Captured sources need `syntra_publish_trusted_int4` again after the downgrade.

`scripts/downgrade-0.5.0-check.sh` creates three sources on 0.4.0: a captured
one published only on 0.4.0, a captured one published again on 0.5.0, and one
that is only tracked. It updates to 0.5.0, writes to all three, downgrades and
restarts on 0.4.0, then checks that:

- the downgrade keeps only the tracked-only projection, and the 0.5.0 library
  is refused when it tries to publish a captured one;
- on 0.4.0 neither captured source is reused, the tracked-only one is, and new
  writes invalidate projections published there;
- history captured on 0.5.0 applies on 0.4.0 and reads back exactly;
- the catalog matches a fresh 0.4.0 install apart from the kept column and its
  two checks, and after a second update matches a fresh 0.5.0 install.

One run passed (`benchmarks/results/downgrade-0.5.0-check-2026-10-29.log`). A
downgrade script that removed only projections with a stored position kept the
one published on 0.4.0, and the check failed.

`scripts/downgrade-check.sh` still checks 0.4.0 to 0.3.0, now building 0.4.0
from its archived source; it passed again.

## 0.5.0 to 0.6.0

0.6.0 fires the capture row trigger at commit. The trigger locks the feed head
row and keeps the lock to commit, so that ordinals stay gap-free and follow
commit order. Fired per statement, it took the lock at a transaction's first
captured change, and every later statement of that transaction ran while other
captured writers and pruning waited. Under a held snapshot those statements
slowed, and the queue on the lock grew until writers missed their arrivals
([findings](../benchmarks/held-snapshot-writers-2026-10-29.md#050-what-still-slows-the-soak)).
A deferred trigger takes the lock only when the transaction commits.

The row trigger is now a constraint trigger, `DEFERRABLE INITIALLY DEFERRED`.
The event, mark and checkpoint formats are unchanged. What a writer sees changes:

- a full capture log, a changed registration and the other capture errors are
  raised at `COMMIT`, not by the statement, and roll back the whole
  transaction; a savepoint around the statement no longer catches them;
- events for a transaction's changes are written at commit, so its own
  uncommitted changes are never in the feed (readers inside a writing
  transaction already fall back to the heap);
- `SET CONSTRAINTS ALL IMMEDIATE`, or naming `_syntra_capture_row`, fires the
  trigger per statement again, with the 0.5.0 lock behavior;
- the recorded `command_id` is the command that fired the trigger at commit.

The script replaces each feed's row trigger and records the new OID in
`syntra.capture_heads` and `syntra.capture_bindings`. It replaces only a trigger
that is still the feed's valid one; a disabled or missing trigger may already
have missed writes, so it stays invalid and capture keeps failing closed until
the feed is reseeded. The DDL guards would treat the replacement as a source
change and retire the feed, so the script turns `event_triggers` off for its
own duration only.

Install the library, restart or end every session that loaded 0.5.0, then run
`ALTER EXTENSION syntradb UPDATE`. Between the restart and the update, the 0.6.0
library accepts the immediate trigger 0.5.0 installed, so capture,
maintenance, checkpoint and trusted reads work as on 0.5.0. The update holds a
table lock on each captured source to commit: a write sent while it is open
waits, then reads the new bindings and is captured.

Each of these planted bugs made a check fail: replacing a disabled trigger
(the gap feed kept a working trigger), leaving the bindings on the old OID and
running with the DDL guards active (a write during the update failed closed),
skipping the swap in the downgrade (it no longer waited for the writer), and a
seed that installed an immediate trigger (two PostgreSQL tests).

## 0.6.0 to 0.5.0 (downgrade)

With the 0.6.0 library still installed, run `ALTER EXTENSION syntradb UPDATE TO
'0.5.0'`, then install the 0.5.0 library and restart. The script replaces each
deferred row trigger with the immediate one 0.5.0 installs. A writer holds its
source's lock until commit, and its queued changes fire before it releases the
lock, so the downgrade waits for it and loses none.

`scripts/downgrade-0.6.0-check.sh` seeds a feed on 0.6.0, leaves history pending,
and starts a writer that holds an update and an insert uncommitted for three
seconds. It runs the downgrade while that writer is open, then checks that:

- the downgrade waited for the writer, and the writer's two changes are in the
  feed;
- the row trigger is immediate after the downgrade, and the 0.6.0 library still
  reads the pending history exactly;
- on the 0.5.0 library new writes are captured, and the history from both
  releases replays and checkpoints to match the heap;
- the catalog matches a fresh 0.5.0 install, and after a second update the
  trigger is deferred again and the catalog matches a fresh 0.6.0 install.

## 0.6.0 to 0.7.0

0.7.0 stores each partition's delta as its own row of
`syntra.capture_chunk_deltas`, keyed by source, epoch, partition and revision.
0.6.0 updated the partition's chunk row on every apply. With a snapshot held,
each update left a version of that row, which every later apply walked twice:
once to find the row and once in the unique check of its new index entry. On
0.6.0 the delta update took nearly all of an applier's time late in a long hold,
and pruning, which waits for the same maintenance lock, stalled behind it
([findings](../benchmarks/held-snapshot-writers-2026-10-29.md#060-what-slows-the-soak-now)).
An apply now inserts the new delta row and deletes the one it replaces by its
physical position; the chunk row is written only when a fold rewrites the base.
The manifest, chunk, delta and event formats are unchanged.

The update only creates the table. Chunk rows keep the inline deltas 0.6.0
stored, and 0.7.0 reads them as 0.6.0 did. The next apply that changes such a
partition moves its delta to a row of its own and puts the chunk row back at its
base revision; a fold rewrites it as a base.

Install the library, restart, then run `ALTER EXTENSION syntradb UPDATE`.
Between the restart and the update, writes are captured, reads replay the inline
deltas, and dropping tables works. An apply that would append a delta fails
with `relation "syntra.capture_chunk_deltas" does not exist` and publishes
nothing, so the maintenance worker logs that error for each source until the
update. The update takes no lock on sources, and a write sent while it is open
is captured at once.

## 0.7.0 to 0.6.0 (downgrade)

With the 0.7.0 library still installed, run `ALTER EXTENSION syntradb UPDATE TO
'0.6.0'`, then install the 0.6.0 library and restart. The script takes the
maintenance lock, so it waits for a running apply or prune; moves each
partition's newest delta row back into its chunk row; and drops the table. Event
triggers are off for the script's own duration, because the 0.7.0 drop guard
would read the table it drops.

`scripts/downgrade-0.7.0-check.sh` appends deltas twice over the same
partitions on 0.7.0, leaves history pending, and starts an apply that holds the
maintenance lock for three seconds. It runs the downgrade while that apply is
open, then checks that:

- the downgrade waited for the apply, and every appended delta is inline;
- the 0.7.0 library and then the 0.6.0 library read the pending history exactly,
  and 0.6.0 applies it to a checkpoint that matches the heap;
- the catalog matches a fresh 0.6.0 install, and after a second update a fresh
  0.7.0 install, and applies append deltas again.

## 0.7.0 to 0.8.0

0.8.0 adds the explicit [fresh integer grouping API](heap-grouped-aggregates.md).
The update creates one SQL function. It preserves capture catalogs, pending
events and checkpoint bytes. Install the new library, restart sessions that
loaded the old library, then update each database. Capture writes, reads and
maintenance keep working before the SQL update. Grouping becomes available
after it commits.

## 0.8.0 to 0.7.0 (downgrade)

With the 0.8.0 library still installed, run
`ALTER EXTENSION syntradb UPDATE TO '0.7.0'`. This removes the grouping function.
Dependent user views block the entire downgrade. Remove those dependencies
explicitly before retrying. Then install the 0.7.0 library and restart sessions
that loaded 0.8.0. Capture storage is unchanged and continues across both steps.

`scripts/downgrade-0.8.0-check.sh` checks refusal with a dependent view, pending
capture replay, writes with each library, checkpoint reuse, a second upgrade,
and catalog parity with fresh installs of both releases.

The 0.7.0 source archive comes from commit `d23f65b`. Its SHA-256 is
`3b737c3f519c20df6012e2c1dac21739c13825418089cd560ee7c5cd6ebf7dc3`.

## 0.8.0 to 0.9.0

0.9.0 enables [serializable fresh heap reads](serializable-heap-reads.md).
The SQL update changes only the extension version marker. Function signatures,
capture catalogs and stored bytes stay unchanged. Capture writers and maintenance
continue before and during the update.

Install the new library, restart sessions that loaded the old library, then run
`ALTER EXTENSION syntradb UPDATE`. Fresh SSI support follows the loaded library,
including before the catalog update. Capture and cached-state calls remain
unsupported under serializable isolation. Existing trusted projections fail
their library-version check and fall back through the established route.
If the library is preloaded, restart PostgreSQL rather than only its client sessions.

## 0.9.0 to 0.8.0 (downgrade)

With 0.9.0 still installed, run
`ALTER EXTENSION syntradb UPDATE TO '0.8.0'`. Then install the 0.8.0 library and
restart sessions. That library restores fresh heap SSI rejection with `0A000`.
No byte or catalog conversion is needed. Pending capture history remains exact.

`scripts/downgrade-0.9.0-check.sh` swaps both libraries, verifies fresh SSI support
and its old-library rejection, and compares both catalogs with fresh installs.
It also replays pending events and captures new writes with each library.

The 0.8.0 baseline comes from commit `28145d0`. Its archive SHA-256 is
`a36943d0515f662a759e68c39ce161eb166a4031fcd8be348b4155825778a63f`.
Unlike earlier archives, it omits nested baseline `.tgz` files to avoid recursive
growth. All 260 retained regular files match that Git tree's bytes and modes.
Repeated packing produced identical bytes. The old library's source fingerprint
still matches the verified build, as checked during the downgrade.
The [provenance record](../build-support/upgrade-baselines/syntradb-0.8.0-src.json)
lists the exclusions. This archive is a baseline library build input; older
baseline archives remain separate in the repository.

## 0.9.0 to 0.10.0

0.10.0 adds three [time-window aggregate functions](temporal-aggregates.md).
The SQL update adds their public declarations. Capture catalogs, stored bytes
and existing function signatures do not change. Writers and maintenance keep
working before and during this additive SQL update.

Install the new library, restart sessions that loaded the older library, then
run `ALTER EXTENSION syntradb UPDATE` in each database. Restart PostgreSQL if
the library is preloaded. The new APIs become callable after the SQL update;
loading the library alone does not add their declarations.

To downgrade, run `ALTER EXTENSION syntradb UPDATE TO '0.9.0'` while the 0.10.0
library remains installed. Dependent views block removal of the temporal APIs
and the update rolls back. Drop those dependencies explicitly before retrying.
Then install the 0.9.0 library and restart sessions, or PostgreSQL if preloaded.
The downgrade removes only these three APIs and keeps capture history.

`scripts/downgrade-0.10.0-check.sh` exercises both libraries, pending history,
atomic dependency refusal and catalog equality with fresh installs. The
[verification record](verification-0.10.0.md) distinguishes local results from
passing native Linux checks.

## 0.10.0 to 0.11.0

0.11.0 adds two [text/UUID grouping functions](text-uuid-grouping.md). It changes
no capture bytes or existing function signatures. Install the new library,
restart old sessions (or PostgreSQL if preloaded), then run
`ALTER EXTENSION syntradb UPDATE` in each database. Capture writes and
maintenance keep working before and during this additive SQL update.

To downgrade, run `ALTER EXTENSION syntradb UPDATE TO '0.10.0'` while 0.11.0
remains installed. Dependent views block removal atomically. Resolve those
objects explicitly, then retry. Install the old library afterward and restart
sessions, or PostgreSQL if preloaded. The temporal and integer APIs remain.

`scripts/downgrade-0.11.0-check.sh` swaps both libraries and checks pending
capture history, dependency refusal and fresh-catalog parity. The
[verification record](verification-0.11.0.md) distinguishes local checks from
pending native Linux results.

## Upgrade check

```sh
bash scripts/upgrade-check.sh          # from 0.1.0
bash scripts/upgrade-check.sh 0.2.0    # from 0.2.0
bash scripts/upgrade-check.sh 0.3.0    # from 0.3.0
bash scripts/upgrade-check.sh 0.4.0    # from 0.4.0
bash scripts/upgrade-check.sh 0.5.0    # from 0.5.0
bash scripts/upgrade-check.sh 0.6.0    # from 0.6.0
bash scripts/upgrade-check.sh 0.7.0    # from 0.7.0
bash scripts/upgrade-check.sh 0.8.0    # from 0.8.0
bash scripts/upgrade-check.sh 0.9.0    # from 0.9.0
bash scripts/upgrade-check.sh 0.10.0   # from 0.10.0
```

The check builds the archived source
(`build-support/upgrade-baselines/syntradb-<from>-src.tgz`) and this tree into a
private copy of the production PostgreSQL install. On the old release it seeds a
feed, applies and prunes some history, and leaves five events unapplied. It then
swaps in the new library and checks that:

- from 0.1.0 and 0.2.0, a capture write is rejected before the update, with its
  source change rolled back; from 0.3.0 and 0.4.0 it is captured, maintenance
  fails, and a read falls back to the heap and matches it; from 0.5.0 it is
  captured and reads replay;
- a write sent while the update transaction is open waits at least a second for
  the update, which holds each captured source's lock to commit while it
  replaces the row trigger, and is then captured; from 0.1.0 it may instead fail
  closed; from 0.6.0, where the update replaces no trigger, it is captured in
  under a second; from 0.7.0 the additive grouping update also permits the write;
- from 0.6.0, an apply before the update fails and publishes nothing, and after
  the drain the replaced deltas have rows of their own;
- from 0.5.0, a feed whose row trigger was disabled keeps the disabled trigger
  and maintenance on it fails, and every other feed's row trigger is the
  deferred one;
- after the update, `extversion` is current and the bindings match the heads;
- the checkpoint read replays the old history and matches the heap exactly;
- new writes, apply and prune, lag and status counts, headroom, stop and reseed
  work;
- a trusted projection published on the old release, after its last write, is
  not reused after the update and captured writes, and a projection published
  again is, until the next captured write;
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
(`benchmarks/results/upgrade-check-0.4.0-2026-10-28.log`). On 2026-10-29 it
passed from 0.1.0, 0.2.0, 0.3.0 and 0.4.0 to 0.5.0, with 506 entries
(`benchmarks/results/upgrade-check-0.5.0-2026-10-29.log`). A reader that treated
a projection with no stored position as matching served the stale 0.4.0
projection after captured writes, and the check failed.

Later on 2026-10-29 it passed from 0.1.0 through 0.5.0 to 0.6.0, with 506
entries, and the three downgrade checks passed on the same tree
(`benchmarks/results/upgrade-0.5.0-to-060-check-2026-10-29.log` and the files
beside it named `*-to-060-check-2026-10-29.log`; the downgrade-0.5.0 and
downgrade-0.4.0 logs are those older checks rerun on the 0.6.0 tree). The
0.6.0 downgrade waited 2,418 ms for a writer holding queued changes and
captured both of them.

## Upgrade under load

```sh
bash scripts/upgrade-load-check.sh [from-version] [seconds] [tps]
```

The check installs the new library over a running 0.1.0 to 0.5.0 database and
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

To 0.6.0 (`benchmarks/results/upgrade-load-0.6.0-2026-10-29.log`), 60 s at
100 tps: from 0.5.0, 0.4.0 and 0.3.0, 3,965 to 4,012 writes committed and none
failed, with the update taking 30, 5 and 9 ms. The update locks the one source
to commit while it replaces the row trigger, and no deadlock with the writers or
the apply loop showed up in these runs.

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
