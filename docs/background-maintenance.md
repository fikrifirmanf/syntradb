# Background maintenance

Captured sources need `syntra_apply_prune_capture` to run regularly. Without it,
readers fall back to the heap as the checkpoint lags, and writers stop once a
feed holds 4,096 unpruned events. SyntraDB can run this for you, with one
background worker per listed database. It is off by default.

```ini
# postgresql.conf; needs a restart
shared_preload_libraries = 'syntradb'
syntradb.maintenance_database = 'app'   # or 'app, reports'; at most 8
syntradb.maintenance_interval_ms = 1000   # reloadable; 100 to 3,600,000
```

Each worker connects to its database as the bootstrap superuser. Names are split
on commas and trimmed, so a database whose name contains a comma or starts or ends
with a space cannot be listed. Each worker needs a slot in `max_worker_processes`
(default 8), which it shares with parallel query. Every interval a worker lists the captured sources and, for each one, runs
`syntra_apply_prune_capture` in a transaction of its own when the source has no
checkpoint yet or has pending events. A source that is fully applied costs one
lag read.

## Failure behaviour

- If another session holds the maintenance lock, for example a manual apply, the
  worker skips that source until the next pass.
- Each source transaction sets a one-second `lock_timeout`. A source's DDL lock
  or another conflicting lock cannot leave the worker waiting indefinitely.
  On timeout the source's work rolls back, and the worker moves to the next
  source. The setting applies separately to each lock acquisition, rather than
  bounding the whole apply. Client sessions keep their own timeout settings.
- Any other error on one source is logged as
  `WARNING: syntradb maintenance skipped relation <oid>: <reason>` and the worker
  moves on to the next source. The warning repeats every pass until the source is
  fixed, usually by reseeding it.
- Until the database exists, the worker cannot connect. PostgreSQL logs its exit
  and restarts it every 10 seconds. Until the extension is installed, it idles.
- It starts once recovery has finished, so it does not run on a standby.
- On shutdown it finishes the source in progress and logs
  `syntradb maintenance stopped`.

An empty name, a name listed twice, a name over 63 bytes or more than 8 names
stop the server from starting, with `invalid syntradb.maintenance_database: ...`
in its log. That is deliberate: a typo should not leave a database unmaintained
without anyone noticing.

Without `shared_preload_libraries`, the settings do not exist and no worker
runs. Loading the library later, through `CREATE EXTENSION`, defines nothing.

## Check

```sh
bash scripts/maintenance-check.sh
```

The check first starts a private cluster with the library preloaded and no
maintenance database, and checks that no worker runs. It then restarts with the
worker at a 200 ms interval and checks that it:

- creates the first checkpoint for a newly seeded feed and drains later writes
  with nobody calling apply, ending at a checkpoint read that matches the heap;
- leaves events pending while another session holds the maintenance lock, then
  drains them once the lock is released;
- drains an unrelated feed while an earlier source holds an exclusive DDL lock,
  then drains the locked source after the lock is released;
- exits after SIGTERM received during a source lock wait, and starts again after
  the server restarts;
- keeps draining one feed while a second feed's tampered binding fails on every
  pass, and logs a warning naming the broken source;
- did not exit after it first connected, and logs a clean stop at shutdown;
- with `'feeds, reports'`, runs one worker in each database and drains a feed in
  both;
- refuses to start with `'feeds,feeds'` and logs why.

Removing the per-source error handler made the check fail, and so did pointing
the second worker at the first database.

## Under load

```sh
bash scripts/maintenance-load-check.sh [seconds] [tps]   # defaults: 300, 200
```

This check makes the worker the only applier. Four pgbench clients run at a
fixed rate against a 10,000-row feed with the default 1-second interval: nine in
ten transactions are captured single-row updates and one in ten is a checkpoint
read. A repeatable-read session holds an old snapshot for the first half. The
check fails if any client fails or aborts, if the feed fills, if pending events
reach 2,048, or if the worker does not drain the feed within 30 seconds of the
load ending.

One 600-second run at 200 transactions per second passed
(`benchmarks/results/maintenance-load-600s-2026-10-28*.log`):

| Measure | Result |
|---|---|
| Measured rate | 199.9 tps, every 30-second sample between 194 and 203 |
| Failed transactions | 0 |
| Latency average, including schedule lag | 5.1 ms |
| Most pending events, snapshot held / released | 196 / 203 |
| Cleanup horizon age before release | 54,775 |

With a 1-hour interval instead, the feed filled and pgbench failed, so the
check does detect a worker that falls behind.

## Limits

- At most 8 databases per server, one worker each. Sources in unlisted databases
  still need manual or scheduled calls. The load check below used one database.
- Sources run one after another. A slow apply delays the sources after it.
- The worker does not pace itself against load or snapshot age. Under a long-held
  snapshot each apply gets slower, as in the
  [soak](../benchmarks/checkpoint-soak-2026-10-27.md); watch
  `syntra_capture_lag` and `syntra_cleanup_horizon_age`.
- It has not been run under the paced soak workload, whose appliers are separate
  client sessions. The load check above is one run on one laptop, with one feed
  and a lighter mix than the soak.
