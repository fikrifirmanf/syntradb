# int8 capture design (0.3.0)

Status: implemented in the 0.3.0 workspace, 2026-10-28. Not released.

## Goal

Capture sources whose key column, value column or both are `bigint`, with the
same guarantees as `int4`: exact checkpoint reads, replay of pending history,
heap fallback, crash and restore behaviour, and the same limits.

Existing `int4` feeds must keep working after the upgrade without a reseed. Their
stored bytes must not change: the v1 golden fixtures stay as they are.

## Layout

A feed has a layout: the width of its key and of its value, each 4 or 8 bytes.
`int4`/`int4` is layout v1 and keeps every existing encoding. Any layout with an
8-byte column uses the formats below.

The layout is chosen at seed from the column types and stored in the binding.
Reseeding is the only way to change it, so a feed never holds two layouts.

## Kernel

The replay kernel is generic over the key and value types (`i32` or `i64`). It
does not widen everything to `i64`: that would double an `int4` row in memory
from 12 to 24 bytes, and a 1,000,000-row `int4` apply would no longer fit the
32 MiB row-buffer limit. Each feed therefore keeps its own row size.

- Batch digests hash each key and value at the layout's width. For v1 this is
  the same bytes as today, so a retry of the last batch applied before the
  upgrade is still recognised.
- Partition routing for 4-byte keys is the frozen v1 function. 8-byte keys use
  Murmur3's 64-bit finalizer, then the low six bits.

## Formats

- **Keyed state** (`SYNTRACP`): version 1 is unchanged. Version 2 stores the key
  and value widths in the two bytes at offset 108, which v1 requires to be zero,
  and uses rows of key width plus value width. A reader refuses a state whose
  version or widths differ from its own layout, before reading any row, so a
  state cannot be read under another layout. Chunk bindings stay as in v1.
- **Segment** (`SYNTRAS1`): type code 1 is nullable `int4`, unchanged. Type code 2
  is nullable `int8`, with 8-byte values and a 32-byte group header holding
  8-byte minimum and maximum.
- **Manifest** (`SYNTRACM`): unchanged. It refers to states and segments by
  digest and does not depend on the layout.

## SQL

- `syntra.capture_base_rows` and `syntra.capture_events` store keys and values as
  `bigint`. The upgrade widens the columns; every `int4` value fits.
- `syntra.capture_bindings` gains `key_width` and `value_width`, 4 for existing
  feeds.
- `syntra_seed_capture` accepts `int4` or `int8` key and value columns.
- `syntra_checkpoint_int8_summary(source, column [, replay_tail])` returns
  `rows`, `non_nulls`, `sum numeric`, `min bigint`, `max bigint`, `route` and
  `reason`, matching PostgreSQL's `sum(bigint)`, which returns `numeric`. The
  `int4` function rejects an `int8` value column, and the reverse.

## Order of work

1. Storage crate: layout, kernel, both state versions, `int8` segments,
   summaries, golden v1 fixtures unchanged, new v2 fixtures. **Done 2026-10-28.**
   The v1 fixtures pass unchanged. `int8` fixtures come from
   `build-support/fixtures/gen_int8_fixtures.py`, which uses only the format
   description, and the Rust encoder reproduces them byte for byte.
2. PostgreSQL side: catalogs, seed, trigger, apply, readers, heap fallback.
   **Done 2026-10-28.** `crates/syntra-pg/src/int8_tests.rs` compares `int8`
   and mixed-width feeds with PostgreSQL's own aggregates through seed, replay
   and prune. A change that truncated values to `int4` failed three of them.
3. Upgrade script 0.2.0 to 0.3.0 and the upgrade check. **Done 2026-10-28.**
   `scripts/upgrade-check.sh` passes from 0.1.0 and from 0.2.0 with identical
   catalogs; see [upgrades](upgrades.md#020-to-030) for the write behaviour
   during the update and its timing.
4. Soak and recovery checks with an `int8` feed. **Recovery done 2026-10-28:**
   `physical-restore-check.sh int8` and `replica-check.sh int8` pass
   ([recovery](recovery.md)). The 20-minute held-snapshot soak passed twice with an `int8` feed,
   with more missed arrivals than `int4` and an unexplained dip at the midpoint;
   see [the soak](checkpoint-soak.md#layouts-and-recorded-runs).

## int2 columns (0.4.0)

Added in the 0.4.0 workspace, 2026-10-28. Not released.

An `int2` key or value column is stored at 4 bytes and uses the `int4` encodings,
so no format, fixture or catalog changes. `syntra.capture_bindings` records the
stored width, 4, and the feed's configuration takes the column's own width, 2,
from the live column type. Changing that type rewrites the table, which changes
the binding identity, so writes then fail closed until a reseed. Keeping the
catalog as it is also keeps the 0.3.0 update from locking `capture_bindings`,
which every captured write reads; an earlier attempt that widened its check
constraint made a write wait 1.4 s for the update in `upgrade-check.sh`.

- Stored keys and values are held to the `int2` range when apply, prune or a
  reader loads them, as other widths are held to theirs. A tampered event value
  of 40,000 fails with `invalid_capture_value`.
- `syntra_checkpoint_int4_summary` reads an `int2` value column. `sum` is
  `bigint`, as PostgreSQL's `sum(smallint)`; `min` and `max` are returned as
  `integer`, where PostgreSQL returns `smallint`. The values are equal.
- `crates/syntra-pg/src/int2_tests.rs` compares `int2` and mixed feeds with
  PostgreSQL's aggregates at -32,768 and 32,767, with sums past the `int2` range,
  through seed, replay, prune and 40 rounds of churn with partition deltas and
  folds. Three planted bugs each failed tests: no `int2` range check, a trigger
  that read values as unsigned, and a configuration that treated `int2` as width 4.
- `physical-restore-check.sh int2` and `replica-check.sh int2` add keys and
  values at the `int2` limits to the pending history. Restore passed with 1
  writer, 16 writers, and 64 writers on 16 feeds
  (`benchmarks/results/int2-recovery-2026-10-28.FAILED-script-overflow.log`; the
  standby run in that log failed because the check doubled a value of 32,767).
  After that fix the standby check passed twice with `int2`, and once each
  with `int4` and `int8` (`benchmarks/results/int2-recovery-2026-10-28.log`).
- The soak is not run with `int2`. It uses 1,000,000 keys, more than `int2`
  can hold, and a smaller soak would not compare with the recorded runs.
