# Checkpoint resource bounds

This page records which checkpoint resources have enforced limits, where the code
enforces them, and which costs are still only observed. It is an inventory for a
limited pilot. It is not a backend memory quota.

## Enforced limits

| Resource | Limit | Enforcement |
|---|---|---|
| Captured sources | 16 | Seed admission, `capture.rs` |
| Live source rows | 1,000,000 | Capture trigger head update |
| Initial capture rows | 100,000 | Seed and head validation |
| Retained events per source | `max_events`, at most 4,096 | Capture trigger; writers fail with SQLSTATE 54000 |
| Reader replay | 256 events, 65,536 affected rows, 8 MiB stored input | `chunks::replay` |
| Stored bytea read | 8 MiB per field, checked before detoasting | `native_catalog::Row::get` |
| Stored chunk read | Exact length for the manifest row count, checked before detoasting | `native_catalog::Row::bytes_exact` |
| Decoded keyed state | 1,000,000 rows, 32 MiB of row buffers | Replay kernel `Limits` |
| Partition delta | 64 KiB; apply folds past `max(64, base rows / 16)` entries | `delta::MAX_DELTA_BYTES`, `Delta::fold_due` |
| Maintenance concurrency | One publisher; others fail with SQLSTATE 55P03 at once, and a vacuum of the catalog is waited for up to one second | Conditional lock in `admission`, retried by `native_catalog::try_admission_lock` |

Long loops poll PostgreSQL interrupts between row groups, every 1,024 rows and
every 64 KiB of checksum input. `statement_timeout` and query cancellation
therefore stop validation, replay and encoding. The error rolls back
publication and pruning together.

## Chunk input is streamed

Checkpoint reads used to load every referenced partition before decoding any of
them. Full validation therefore held all 64 states and segments at once. Reader
summaries held all 64 segments. Stored values were only capped at 8 MiB per field.

Validation, reader summaries, replay and changed apply now visit partitions one
at a time. Each partition's bytes are released before the next partition is
fetched. Only dirty partitions' new encodings are kept for the single bulk
publication statement.

Before fetching a state or segment, the reader computes that format's exact
encoded length from the manifest row count. The stored length comes from the
varlena or TOAST pointer header. A mismatch fails as `invalid_checkpoint` before
decompression, an out-of-line fetch, digesting or copying into Rust memory.
Padded, truncated and replaced oversized values are therefore rejected without
being read. Readers fall back to the heap; maintenance rolls back and does not
prune.

`syntra_checkpoint_input_bytes()` reports the latest chunk pass in the calling
backend: partitions passed on, stored bytes fetched and the largest amount held at
once. A pass that fails still reports the bytes it fetched. The peak is the
largest single partition's fetched bytes, not process memory.
The function exists for tests and operational checks. It reveals sizes that
already follow from the caller's visible row counts.

Tests cover a compressed 20,000-row checkpoint, segment-only reader passes, a
padded segment and truncated state. Removing the exact-length check, fetching
states for reader summaries, or summing instead of taking the peak each made a
focused test fail.

## Growth past the seed cap

`scripts/capture-growth-check.sh 1000000 4000` seeds 100,000 rows, then grows an
`int4` source to 1,000,000 rows by captured inserts, 4,000 per batch, applying and
pruning after each batch (2026-10-28, one laptop, one writer, log in
`benchmarks/results/capture-growth-1m-2026-10-28.log`). Every checkpoint read
matched the heap, before and after an immediate crash restart.

- Apply and prune time grew about linearly with rows: 17 ms at 104,000 rows,
  63 ms at 440,000, 144 ms at 1,000,000. Median 88 ms, p95 166 ms, max 224 ms over
  225 batches. Each batch rewrites the whole checkpoint because sequential keys
  dirty all 64 partitions, so cost follows source size, not batch size.
- The whole run took 85 s and wrote 1,817 MB of WAL for 900,000 inserted rows.
- A checkpoint summary read at 1,000,000 rows took 8 ms.
- Sizes at the end: checkpoint chunks 963.5 MB, events 119.1 MB, base rows 9.4 MB,
  source 56.0 MB. The chunk table was not vacuumed or measured for live bytes, so
  it includes dead versions from the per-batch rewrites.

This shows the 1,000,000-row cap is reachable by growth with correct reads. It
does not cover updates or deletes at that size, concurrent writers, `int8` feeds,
or a held snapshot at that size, where the dead versions above would not be
reclaimed.

### Churn at 1,000,000 rows

With a third argument, the same script runs `capture-growth-check.sh 1000000 4000 120`:
after growth, four writers each own a quarter of the keys and run transactions of
about 50 row updates, a delete with reinsert, and a key move, while one loop
applies and prunes every 0.2 s (log `benchmarks/results/capture-churn-1m-2026-10-28.log`).

Three runs passed, all on 2026-10-28. In each, the only writer error was the
full-log refusal (SQLSTATE 54000). The row count was unchanged, and the checkpoint
read matched the heap after the drain and after a crash restart.

| Run | Committed | Refused, log full | Growth apply median / p95 / max | WAL | Chunk table at end |
|---|---|---|---|---|---|
| `capture-churn-1m` | 9,344 | 2,146 | 95 / 162 / 191 ms | 6,611 MB | 2,077.8 MB |
| `capture-churn-bloat-1m` | 2,548 | 1,656 | 189 / 436 / 2,049 ms | 3,725 MB | 658.0 MB |
| `capture-churn-bloat2-1m` | 6,085 | 12 | 138 / 432 / 1,288 ms | 5,399 MB | 981.0 MB |

Throughput and apply time varied by up to three times between identical runs on
the same laptop. The cause is not known; nothing else was measured on the machine
during the runs, so these timings are not a capacity figure.

- **Chunk table size is almost all dead space.** After the run, `VACUUM FULL`
  shrank the chunk table from 658.0 MB to 8.5 MB, and from 981.0 MB to 9.2 MB.
  The checkpoint read still matched after the rewrite. In the last run, read
  before the crash restart, the table held 64 live rows (one per partition) and
  2,655 dead ones, and autovacuum had run 4 times. Autovacuum does run, but the
  table stayed about 100 times its live size. Plain vacuum makes space reusable
  without returning it, so this may be a plateau rather than growth. These 120 s
  runs cannot tell the two apart. The statistics in the second run read zero
  because the crash restart reset them; the script now reads them first.
- Each apply rewrites every partition a batch touches, and these writers touch
  all 64, so churn at this size costs gigabytes of WAL.
- Sustained write rate is bounded by apply speed. At this size an apply takes
  100 to 300 ms and the log holds 4,096 events, so a producer that outruns it is
  refused rather than slowed. Producers need retry logic.

Two earlier attempts failed and are kept in `benchmarks/results/`. The second
failed because of a bug in the script: writers shared rows, and PostgreSQL
reported 8 deadlocks. The script is fixed. The first logged one `55P03`
"checkpoint maintenance is busy" to the apply loop, the only publisher. A later
run hit the same error in the single-session growth phase. The cause was
autovacuum: VACUUM and ANALYZE of `syntra.capture_checkpoints` hold SHARE UPDATE
EXCLUSIVE, which conflicts with the admission lock, and a conditional lock request
never cancels autovacuum. Admission now retries for up to one second; see
[admission and vacuum](#admission-and-vacuum).
Not covered: more than four writers, other machines, `int8`, and a snapshot held
open at this size.

### Chunk table growth under sustained churn

Three 600 s churn runs at 1,000,000 rows sampled the chunk table every 30 s (logs
`capture-churn-600s-1m-2026-10-28.log`, `capture-churn-600s-eager-1m-2026-10-28.log`
and `capture-churn-600s-delta-1m-2026-10-28.log`), and one 1,800 s run sampled it
the same way (`capture-churn-1800s-delta-1m-2026-10-28.log`). All four passed every
correctness check, including the crash restart. The first two ran on 0.3.0, the
others on 0.4.0 with [partition deltas](chunked-checkpoints.md#partition-deltas-040).

| Version, autovacuum on the chunk table | Committed | Failed closed on a full log | Size at churn start → end | Live after `VACUUM FULL` | WAL | WAL per transaction |
|---|---|---|---|---|---|---|
| 0.3.0, defaults | 28,453 | 3,842 | 465 MB → 3,895 MB, rising steadily | 12.4 MB | 18.9 GB | 0.66 MB |
| 0.3.0, cost delay 0 on the table and its TOAST table | 48,751 | 14,121 | 776 MB → 1,403 MB, trimmed by each vacuum | 12.4 MB | 31.3 GB | 0.64 MB |
| 0.4.0, defaults | 63,251 | 0 | 83 MB → 198 MB, trimmed by each vacuum | 12.8 MB | 6.5 GB | 0.10 MB |
| 0.4.0, defaults, 1,800 s | 211,297 | 0 | 125 MB → 152 MB, between 92 and 236 MB throughout | 12.9 MB | 18.5 GB | 0.09 MB |

WAL covers the whole run, including growth from 100,000 rows, so the per-transaction
figure is an upper bound for churn alone.

- On 0.3.0, over 98% of the size is the TOAST table, where the checkpoint states and
  segments are stored. Between vacuums its dead rows reach 300,000 to 565,000.
  The cause is write amplification: an apply rewrote every partition a batch
  touched in full, and random keys touch all 64. At 1,000,000 rows that is about
  12 MB of new checkpoint per apply, whatever the batch size.
- On 0.4.0 an apply writes only each touched partition's delta and leaves the base
  in place until the delta passes its fold bound. With default autovacuum the
  table stayed between 104 and 198 MB after the first minute, and dead TOAST rows
  between vacuums stayed under 80,000. Its low points rose from 127 MB at 180 s
  to 146 MB at 420 s. In the 1,800 s run the table never passed 236 MB, its last
  sample was 152 MB, and its low points did not trend upward: 119 MB at 90 s,
  151 MB at 871 s, 92 MB at 1,321 s and 105 MB at 1,472 s. That run is the longest measured.
- With the same four writers, 0.4.0 committed 2.2 times the transactions of 0.3.0,
  and no write failed on a full log. Apply during growth took a median 54 ms
  against 180 ms. Each figure comes from one run on one laptop, and three earlier
  120 s runs of the same script varied by a factor of 3.7 in committed
  transactions.
- Removing the cost delay is an operator choice, not a default: it makes vacuum
  of this table run at full I/O speed. The extension does not set it.

A source that takes sustained random writes at this size still needs storage
headroom above its live checkpoint size: up to about 18 times on 0.4.0 in these
runs, against 300 times after 10 minutes on 0.3.0. Runs longer than 30 minutes,
and other hardware, have not been measured.

## Admission and vacuum

Checkpoint maintenance, capture setup and trusted publication each admit one
caller with a SHARE ROW EXCLUSIVE lock on a catalog (`capture_checkpoints`,
`capture_heads`, `trusted_int4_projections`). The request never queues. VACUUM and
ANALYZE hold SHARE UPDATE EXCLUSIVE, which conflicts, and an autovacuum worker is
only cancelled for a lock request that waits. Before this fix, autovacuum visiting
a catalog made the caller fail as busy. Two churn runs at 1,000,000 rows did.

Each admission now retries every 10 ms for up to one second before failing with
the same `55P03` error. `scripts/admission-vacuum-check.sh` holds the vacuum lock
mode on each catalog from another session: a 300 ms hold now admits the caller,
and a 3 s hold still fails as busy after about 1.2 s. Without the retry, all three
300 ms cases fail.

The first version of the retry also waited for concurrent callers. In a 20-minute
soak with two appliers and a pruner, busy rejections fell from about 80% of apply
attempts to 0.1% and applies committed 4.4 times as often. The retry now probes
with a conditional ROW EXCLUSIVE lock, which conflicts with another admission and
with DDL but not with vacuum: when the probe fails, the caller is busy at once.
The check holds SHARE ROW EXCLUSIVE from another session and requires the busy
error within 200 ms; it took 78 to 107 ms, and the earlier retry fails the case
after 1.2 s. A vacuum that holds the lock longer
than a second, such as an anti-wraparound vacuum of a bloated catalog, can still
fail admission; the error is retryable.

## Not yet bounded

- Rust allocations are outside `work_mem` and PostgreSQL memory contexts. Limits
  above bound inputs and decoded rows, not total backend memory.
- Changed apply keeps every dirty partition's new encoding until the bulk
  publication statement. SPI then copies those arrays into PostgreSQL memory.
  With all 64 partitions dirty this is about two copies of a full checkpoint.
- Seeding holds the base rows, the applied prefix and all 64 new encodings.
- A full feed rejects writers with a hard error. `syntra_capture_headroom(source)`
  gives producers advance notice, but it is advisory: it counts committed events
  only, so concurrent open transactions can still use the free events.
- A long-held snapshot stops vacuum from removing old catalog versions. Storage
  grows for as long as it is held, and on 0.1.0 write throughput fell; see below.
- Background maintenance is opt-in and covers one database
  ([background maintenance](background-maintenance.md)). If nothing applies and
  prunes, writers stop once the feed fills. `syntra_capture_lag(source)` returns `emitted`, `applied`,
  `pending_events` and the checkpoint `generation`, so a monitor can alert when
  pending events keep growing. `applied` and `generation` are NULL before the
  first apply. Like the headroom, it needs only a privilege on the source table.
  `syntra_capture_status()` returns both for every captured source the caller has
  a privilege on, one row each. A source whose lag or headroom read fails keeps
  its row, with the reason in `error` and the counts NULL. The two reads are taken
  one after the other, not from one snapshot.

The [checkpoint soak](checkpoint-soak.md) observes storage, WAL, xmin horizon and
backend RSS over a longer paced run with a retained snapshot.

## Snapshot age policy

An old snapshot, prepared transaction or replication slot stops vacuum from
removing dead versions of the capture catalogs and the source table, so storage
grows while it is held. On 0.1.0, in the [failed 20-minute soak](../benchmarks/checkpoint-soak-2026-10-27.md),
writer throughput also fell about 70% over ten minutes at roughly 55 writes per second,
and recovered in the first minute after the snapshot ended. A diagnostic run at
twice the rate fell the same way in about a third of the time.

`syntra_cleanup_horizon_age()` returns how many transactions have started since the
oldest thing holding back cleanup of the capture catalogs. It needs no special
role and reveals only an age.

Pilot policy:

- Alert when the age passes 5,000 and treat 10,000 as the page threshold. Writers
  kept full speed up to about 8,500 (60 cycles per second) and 17,600 (30 cycles
  per second), and were 18% and 10% slower at 16,400 and 21,900. The thresholds
  leave about a factor of two before the first measurable loss.
- End long reporting transactions, prepared transactions and idle replication
  slots. SyntraDB does not do this for you; set `idle_in_transaction_session_timeout`
  and `max_slot_wal_keep_size` for the roles and slots in play.
- The thresholds come from one laptop at one load. They scale with write rate in
  transactions, not with minutes. Measure on the pilot's own hardware.

The thresholds above were measured on 0.1.0. The measured cause was the head row:
every captured write updated it, and each lookup had to step over every version
written since the snapshot began. 0.2.0 stops writers updating it and bounds the
pruner's mark delete; see the
[measured fixes](../benchmarks/checkpoint-soak-2026-10-27.md#fixes-measured-2026-10-28).
On 0.2.0 the 20-minute soak held full writer speed to an age of 42,872 at 30
cycles per second, and the 60-cycle diagnostics slowed only above about 41,000
with readers running. On 0.4.0 a two-hour soak held the snapshot for an hour:
writers kept full speed to an age near 89,000, then fell to a fifth of it by
188,000, and recovered within a minute of release
([two-hour hold](checkpoint-soak.md#two-hour-hold-int4-2026-10-29)). A snapshot
left open long enough still stops writers in practice. Storage still grows while
a snapshot is held, and these are one laptop's numbers, so keep the policy. Raise the thresholds only after a
held-snapshot soak passes on the pilot's hardware.
