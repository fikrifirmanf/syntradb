# Checkpoint concurrency stress

```sh
bash scripts/checkpoint-stress.sh --recovery-checks true
```

The script builds release Rust binaries and assertion-disabled PostgreSQL, then
runs in a disposable Unix-socket cluster. It uses 10,000 source rows and a
60-second worker phase by default. `--checkpoint-stress-seconds` accepts zero to
disable the phase, or 5–300 seconds. Source rows clamp to 4–100,000. Ordinary
benchmark phases and deterministic checkpoint checks run first.

## Workload and checks

Eight workers use independent database sessions. Reader queries explicitly enable
the experimental three-argument replay overload:

- Two writers perform transfers, NULL inserts, key moves and deletes. Each
  committed transaction produces six capture events. Every eleventh attempt
  rolls back intentionally. Periodic own-write reads must select the heap and
  match native SQL before the temporary row is deleted.
- A read-committed reader compares every checkpoint summary with native SQL in
  the same statement. It checks all five aggregate fields and the route, including bounded replay.
- A repeatable-read reader compares nine summaries within each transaction.
  Its native results must match, and its first and last summaries must agree.
- Two appliers alternate apply-only and apply-plus-prune transactions. This leaves
  work for the independent pruner. Every seventeenth attempt rolls back. Competing
  maintenance exercises nonblocking lock admission.
- A separate pruner repeatedly requests prefix deletion.
- A privileged monitor validates the manifest and all 64 partitions, reconstructs
  keyed rows, and applies the
  pending event suffix. It compares every key and nullable value with ordered
  native source rows in the same repeatable-read snapshot. It also runs VACUUM
  on events, manifests and partitions every tenth attempt.

A ninth reader pins the initial checkpoint snapshot before workers start. It
holds that snapshot until workers finish. Its final result must still select the
old checkpoint and match its original native result. The coordinator uses a
separate administrative connection for setup and terminal measurements.

Writers check affected row counts. The monitor checks manifest epoch and
cursor, partition revisions, digests, key routes, representation agreement, retained and pending boundaries, event presence flags, contiguous
ordinals and source row count. At the end, emitted events must equal six times
committed writer transactions. Deleted events must equal successful workers'
reported prune counts. Retained rows must match the head's retained interval.

Every worker must commit work. Writers and appliers must also complete intentional
rollbacks. The independent pruner must delete events. Keyed checks and all three catalog
vacuums must also make progress.
The cursor must exceed 4,096, proving that pruning freed capacity during the run.
These gates can reject a run too short for its source size and host.

## Timing and resource accounting

Each worker records separate histograms for completed cycles and expected
rejections. Writer cycles include BEGIN through COMMIT or intentional ROLLBACK,
source statements and occasional own-write checks. Reader cycles include the
native oracle. These are service latencies for this workload; they do not measure
production query latency in isolation.

By default, all workers run continuously without an arrival schedule.
The [paced variant](checkpoint-paced-load.md) adds absolute arrival slots, missed
counts and scheduled latencies. It can explicitly disable reader replay for a
comparison. The saturated defaults and progress gates remain unchanged. Maintenance lock
rejections can therefore occur frequently. The report retains their counts rather
than interpreting them as completed maintenance work. PostgreSQL lock timeout
(`55P03`) is expected. Mutating workers may also encounter a deadlock (`40P01`).
Only a writer's exact capture-capacity error is accepted as `54000`. Other errors,
including statement timeouts, fail the run. An error or panic stops peers, and
in-flight statements drain before reporting. Sessions use a 15-second statement
timeout and a 250 ms lock timeout. The coordinator uses a separate five-second
lock timeout for setup and terminal measurements.

The report records node WAL insert-LSN differences for the worker phase. This
includes commits, intentional aborts, VACUUM and background work. It excludes
setup, final catch-up and final VACUUM. No backend-level attribution is claimed.

Footprints include allocated source and capture catalog bytes, manifest and
partition bytes both separately and in the checkpoint total, checkpoint
generation, emitted/applied/pruned cursors and live retained-event counts. The
monitor samples maximum pending and retained head intervals. A zero final feed
requires draining apply/prune after workers stop. Final keyed reconstruction and
checkpoint reuse must both succeed.

Footprints are taken before workers, after workers while the old snapshot remains
open, after catch-up, and after VACUUM. Long snapshots preserve old versions even
when logical event retention stays bounded. VACUUM can make space reusable without
shrinking its allocated files. This test does not enforce an RSS or disk quota.

## Scope and next work

The source uses one int4 primary key and one nullable int4 value. Transient row
lifecycle changes occur within writer transactions; the committed row count and
sum remain constant. Transfers still change values and minima. The keyed oracle
checks details that aggregate totals can miss. No source DDL occurs during this
phase because unsupported DDL requires capture reseeding. Separate deterministic
checks cover DDL invalidation, crashes and restore.

The harness validates snapshot correctness and maintenance progress under a
saturated local workload. It does not compare databases, simulate power loss,
measure paced tail latency or establish production readiness. Use measured WAL, checkpoint allocation and fallback frequency to guide bounded
reader-tail admission and paced load before adding automatic maintenance or planner
routing.

The [combined maintenance option](atomic-checkpoint-maintenance.md) replaces the
apply-plus-prune half of applier cycles with one atomic call. Apply-only cycles
and the independent pruner remain active. It is off by default and uses the same
rollback, accounting and progress gates.
