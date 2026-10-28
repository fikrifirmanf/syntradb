# Recovery checks

```sh
bash scripts/recovery-checks.sh
bash scripts/recovery-checks.sh --wal-init-zero false
```

Each run creates a private PostgreSQL cluster with fsync, synchronous commit and
full-page writes on. It uses the optimized server build and release extension.
System databases stay outside this test.

The runner first rejects capture of 1,000,001 rows with SQLSTATE 54000, then checks
that the source is intact and the connection still works. It checkpoints an
initial projection, commits a source change and refresh, and opens another source
change with an uncommitted refresh. A separate reader must still see committed
data. A repeatable-read reader must retain its earlier catalog generation.

The runner stops PostgreSQL immediately while that last transaction stays open.
After restart, the runner uses the same PGDATA and checks that committed source values and projection
bytes survived. Uncommitted changes must be absent. The final stage uses pg_dump and
pg_restore to create another database and compares the restored copy. Historical
source database identity must remain unchanged, rather than point at the new DB.

Initial caller-table checks succeeded with both WAL allocation modes. The managed
catalog run also succeeded, including extension configuration data in the dump.
Raw local reports are in `benchmarks/results/recovery-zeroed-2026-10-27.json`,
`recovery-sparse-2026-10-27.json`, and `managed-recovery-2026-10-27.json`.

These are process crash and logical restore checks. They do not simulate power
loss, torn writes, faulty hardware, or cross-cluster role mapping. A standby and
one clean failover are checked [below](#streaming-standby-and-failover).
Storage corruption tests are separate from durability tests. Release upgrades
have [their own check](upgrades.md#upgrade-check).

## Physical backup and point-in-time restore

```sh
bash scripts/physical-restore-check.sh        # int4 feed
bash scripts/physical-restore-check.sh int8   # int8 feed
bash scripts/physical-restore-check.sh int4 16   # 16 concurrent writers
bash scripts/physical-restore-check.sh numeric   # numeric(12,2) values
```

The check builds the current extension into a private copy of the production
PostgreSQL install and runs a primary with `wal_level = replica` and WAL archived
by `cp`. It seeds a feed of 1,000 rows, applies and prunes, and leaves captured
history unapplied. It then takes a base backup with `pg_basebackup -X stream`
while a session commits 3,000 captured updates, applying and pruning every 100.
The feed position must move during the backup; in the recorded run 2,537 events
were committed while it ran.

After the backup the primary deletes, inserts, applies and prunes, and leaves more
history pending. It records the heap aggregates and `syntra_capture_lag` at named
restore point A. It then changes, applies and moves keys again for point B.

The same base backup is restored twice, once to restore point A and once to the
end of the archive. Each restored cluster must:

- match the primary's heap aggregates and lag at that point exactly;
- answer the opt-in checkpoint read by replaying the pending history, matching
  the heap;
- keep capturing, then drain with `syntra_apply_prune_capture` to a checkpoint read
  that matches the heap and an empty feed.

Making `capture_marks` unlogged, so restored clusters lose their marks, failed
the check at the first lag comparison.

With `int8` the feed's values start at 5,000,000,000 and point A adds 20 keys past
6,000,000,000 with values near -9×10^18. The check asserts that the binding
records 8-byte widths. Two `int8` runs passed on 0.3.0
(`benchmarks/results/int8-recovery-2026-10-28.log`).

With a second argument, that many sessions (1 to 64; 16 when first measured) each commit 3,000 captured
updates to overlapping rows while the backup runs, and one more session applies and
prunes every 20 ms instead of the writer doing it. Runs with 8 and 16 `int4` writers
and 8 `int8` writers passed the same checks, with 2,039 to 4,254 events committed
during the backup (`benchmarks/results/int8-recovery-2026-10-28.log`). The writers
share a few hundred rows, so they contend; the check does not measure throughput.

With `int2`, point A also adds keys and values at -32,768 and 32,767. Runs
with 1, 16 and 64 writers (64 on 16 feeds) passed on 0.4.0
(`benchmarks/results/int2-recovery-2026-10-28.FAILED-script-overflow.log`, whose
last run is a standby check that failed in the script, below).

With `numeric`, keys are `int4` and values `numeric(12,2)`, seeded with
fractions; writers add 0.01, and point A adds 9,999,999,999.99,
-9,999,999,999.99 and -0.01. The binding must record widths 4/8, and reads go
through `syntra_checkpoint_numeric_summary`. Runs with 1 writer on 1 feed, 16
on 4 and 64 on 16 passed on 0.4.0, with 2,568, 9,104 and 58,793 events
committed during the backup (`benchmarks/results/numeric-recovery-2026-10-28.log`).

A third argument spreads the writers over that many captured tables (1 to 16),
and every check above runs on each of them.
On 0.4.0, on 2026-10-28, these runs passed
(`benchmarks/results/restore-writers-feeds-2026-10-28.log`):

| Type | Writers | Feeds | Events during the backup |
| --- | ---: | ---: | ---: |
| `int4` | 1 | 1 | 1,704 |
| `int8` | 1 | 1 | 1,725 |
| `int4` | 32 | 1 | 3,573 |
| `int4` | 64 | 1 | 1,894 and 1,752 (two runs) |
| `int4` | 32 | 8 | 31,741 |
| `int4` | 64 | 16 | 66,823 |
| `int8` | 64 | 16 | 51,653 and 62,846 (two runs) |

With 64 writers on one feed, fewer events committed during the backup than with
32, because the writers queue on a few hundred shared rows. The check does not
measure throughput, so this says nothing about write rates. The first version
of the multi-feed script created three feeds when asked for one, because macOS
`seq 2 1` counts down. The binding-count check caught it, and those runs are not
in the log.

This covers one cluster restored from its own base backup and archive on the same
machine and PostgreSQL build, with up to 64 writers and 16 feeds of 1,000 rows. It
does not cover failover from a restored cluster, large feeds, restore onto a
different PostgreSQL minor version, or backup tools other than `pg_basebackup`.

## Streaming standby and failover

```sh
bash scripts/replica-check.sh        # int4 feed
bash scripts/replica-check.sh int8   # int8 feed
bash scripts/replica-check.sh int2   # int2 feed, stored at 4 bytes
bash scripts/replica-check.sh numeric   # numeric(12,2) values, int4 keys
```

The check runs a primary with the maintenance worker and a streaming standby
made with `pg_basebackup -R`, on one machine. It checks that:

- no maintenance worker runs on the standby;
- once the standby has replayed the primary's WAL, a checkpoint read on it
  matches its heap, through the checkpoint when history is applied and through
  replay while history is pending (111 events, with the primary's maintenance
  lock held so the worker could not apply them);
- `syntra_capture_lag` and `syntra_capture_status()` work on the standby;
- apply is refused there (`checkpoint maintenance requires a writable read
  committed transaction`), and so are capture writes;
- while pgbench commits 200 captured updates per second on the primary, every
  checkpoint read on the standby, run back to back for 15 seconds, matches the
  standby's heap in the same statement;
- after the primary is stopped in immediate mode with that load still running
  and the standby is promoted, the worker starts on it, reads still match, and
  new writes are captured and drained.

Two runs made 2,917 and 2,971 standby reads under load. All matched; most went
through replay, because the worker leaves up to a second of events pending.

`replica-check.sh int2` adds keys and values at the `int2` limits to the
pending history. Its first run failed with `smallint out of range` in the
check itself: the update after promotion doubled every value with a key up to
300, including 32,767 at key -32,768. That update now covers keys 1 to 300,
which is every key it touched before for `int4` and `int8`. Then two `int2`
runs passed with 2,560 and 2,535 standby reads, and one run each of `int4`
and `int8` passed (`benchmarks/results/int2-recovery-2026-10-28.log`).

`replica-check.sh numeric` seeds `numeric(12,2)` values with fractions, adds
values at the column's limits to the pending history and has pgbench add 0.01.
Two runs passed with 2,397 and 2,270 standby reads, all matching. After the
script change, one `int4` restore run and one `int8` standby run also passed
(`benchmarks/results/numeric-recovery-2026-10-28.log`).

Starting the worker at consistent state instead of after recovery made the check
fail, because a worker then ran on the standby. Result:
`benchmarks/results/replica-check-2026-10-28.log`. One `int8` run, with the same
value offset and 20 large keys added to the pending history, made 2,932 standby
reads under load, all matching (`benchmarks/results/int8-recovery-2026-10-28.log`).

Not covered by this check: a standby lagging far behind and logical replication of
captured tables. Writes the standby had not received when the primary stopped are
lost, as with any asynchronous standby; this check does not count them, but the
topology check below does for a synchronous one. A standby only reads; its feeds
fill and drain with the primary's.

## Synchronous standby, cascading standby and failback

```sh
bash scripts/replica-topology-check.sh           # int4 feed
bash scripts/replica-topology-check.sh numeric   # numeric(12,2) values
```

The check builds a primary with a synchronous standby `s1` and a second standby
`s2` that streams from `s1`. Four writers update a captured feed on the primary,
counting a write as acknowledged only when `psql` returned success. After 8 s of
reads on `s2` under that load, the primary is stopped in immediate mode and `s1`
is promoted. The check requires that:

- every acknowledged write is present on the promoted node, and no more than one
  unacknowledged write per writer is (4,827 and 5,089 acknowledged in two runs,
  exactly that many present both times);
- `s2` stays in recovery, follows the new timeline, has the same rows as the
  promoted node and answers checkpoint reads that match its heap (868 and 979
  reads under load, all matching), with no worker running on it;
- the promoted node's worker takes over, drains the feed and drains new writes;
- the old primary is started once for crash recovery with replication and its
  worker off, stopped cleanly, rewound with `pg_rewind` to the promoted node and
  started as its standby. It then has the promoted node's rows, answers matching
  reads, runs no worker and follows a further write.

Failback has prerequisites that the first versions of the check tripped over. The
old primary needs data checksums or `wal_log_hints`, and it must still hold the
WAL from before the divergence: its shutdown checkpoint recycled that WAL until
`wal_keep_size` was set, and `pg_rewind` then failed with "could not find previous
WAL record". `pg_rewind` also copies the source's configuration files, port
included.

With `numeric`, the feed's values are `numeric(12,2)`, writers add 0.01, and
acknowledged writes are counted in hundredths, so a lost write still lowers the
count by one. Two `numeric` runs passed on 0.4.0 with 4,680 and 5,471
acknowledged writes, exactly that many present, and 833 and 952 matching
cascading reads; one `int4` run of the edited script passed with 5,620
(`benchmarks/results/numeric-sync-quorum-2026-10-29.log`).

Results: `benchmarks/results/replica-topology-2026-10-28.log`. This is one
synchronous standby and one cascading standby on one machine, an immediate
process stop rather than a power loss or a network partition, and four writers
on a small feed. It does not cover a synchronous standby that falls behind
(the primary then waits) or promoting `s2` instead; quorum commit has its own
check below.

## Quorum synchronous commit

```sh
bash scripts/quorum-check.sh           # int4 feed
bash scripts/quorum-check.sh numeric   # numeric(12,2) values
```

The check builds a primary with three standbys and
`synchronous_standby_names = 'ANY 2 (q1,q2,q3)'`. Four writers update a captured
feed throughout, counting a write as acknowledged only when `psql` returned
success. It requires that:

- with `q3` stopped, writes are still acknowledged;
- with `q2` stopped as well, none is acknowledged for 3 s, and writes resume
  once both return;
- with the WAL receiver of `q3` stopped by `SIGSTOP`, so its link stalls without
  closing, writes are still acknowledged;
- after an immediate stop of the primary and of the standby that had received
  the most WAL, then the stalled link recovering, the more advanced of the other
  two holds every acknowledged write, and at most one more per writer, once
  promoted;
- the promoted node's worker drains the feed and new writes, and the last
  standby follows it on the new timeline with the same rows and matching
  checkpoint reads.

Two runs passed. With 5,881 and 6,250 acknowledged writes, the promoted
standby held exactly that many. No write was acknowledged with two standbys
down.

As a control, the same script with `ANY 1`, two stalled standbys and no
two-down assertion
(`benchmarks/results/quorum-check-control-any1-2026-10-28.sh`) failed both
times. The promoted node lacked 1,286 and 1,399 acknowledged writes, so the
check detects lost writes when the quorum does not cover them. Without the
stall, two `ANY 1` runs lost nothing: on one machine every standby keeps up,
so the failover step alone could not tell the settings apart.

`quorum-check.sh numeric` passed twice on 0.4.0: the promoted standby held
exactly the 7,820 and 7,732 acknowledged writes, and none was acknowledged with
two standbys down. One `int4` run of the edited script passed with 7,473
(`benchmarks/results/numeric-sync-quorum-2026-10-29.log`). The `ANY 1` control
was run only with `int4`; the `numeric` mode counts writes the same way, in
hundredths.

Results: `benchmarks/results/quorum-check-2026-10-28.log`. This is three
standbys on one machine, a stalled process rather than a network partition
between machines, and an immediate process stop rather than a power loss. It
does not cover choosing the promotion candidate automatically. The check picks
the standby with the highest received WAL position, as an operator or a
failover tool must.

The source-tracking run also checks counter visibility across sessions and crash
rollback. After logical restore, writes fail until an admin rebinds the source.
The report is `benchmarks/results/tracking-recovery-2026-10-27.json`. Rebind creates
a new epoch; the old fixed copy stays unchanged after a later source write.

The DDL guard run also checks read-only REINDEX and extension removal. Removing
the extension with CASCADE removes its guards and leaves normal source writes
working. Its report is `benchmarks/results/ddl-guard-recovery-2026-10-27.json`.

Trusted publication now has the same process-crash check. Committed bytes and
bindings survive; an uncommitted replacement disappears with its source writes.
An older reader keeps its visible publication before the crash. Logical restore
omits all trusted bindings, so the live API uses the restored heap. See
`benchmarks/results/trusted-live-2026-10-27.json` and the
[trusted live contract](trusted-live-projections.md).
