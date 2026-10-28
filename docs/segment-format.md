# Experimental int4 segment format

Version 1 stores one nullable PostgreSQL `int4` column. It uses plain encoding,
little-endian integers and groups of at most 1024 rows. Every nonfinal group has
exactly 1024 rows. An empty segment has a header and no groups.

This is an experimental value format. It contains no table identity, snapshot,
transaction IDs or source lineage. Saving its bytes does not register a live-table
projection. Compatibility beyond version 1 is not promised yet.

## File header

The header is 40 bytes. Offsets are from the start of the segment.

| Offset | Width | Field |
|---|---|---|
| 0 | 8 | ASCII magic `SYNTRAS1` |
| 8 | 2 | Format version, 1 |
| 10 | 2 | Type tag, 1 for nullable int4 |
| 12 | 2 | Encoding tag, 0 for plain |
| 14 | 2 | Flags, zero |
| 16 | 4 | Total row count |
| 20 | 4 | Group capacity, 1024 |
| 24 | 4 | Group count, ceil(rows / 1024) |
| 28 | 4 | Reserved, zero |
| 32 | 4 | Total encoded byte length |
| 36 | 4 | IEEE CRC-32 of header bytes 0 through 35 |

Type tag 2 is nullable `int8`, added in 0.3.0 for `int8` capture feeds. It uses
the same header and rules with eight-byte values; see [type tag 2](#type-tag-2-nullable-int8).
The SQL segment functions still produce and read only type tag 1.

Unsigned fields use `u16` or `u32` according to width. Signed values use two's
complement `i32`. Readers reject unknown versions, type tags, encodings and flags.
They also reject inconsistent counts, trailing bytes and truncated records.

## Group record

Groups follow the header in order. Each record has a 24-byte header followed by
one validity bitmap and one contiguous value buffer. There are no alignment gaps.

| Offset | Width | Field |
|---|---|---|
| 0 | 4 | Group row count |
| 4 | 4 | NULL count |
| 8 | 4 | Minimum non-NULL value, signed |
| 12 | 4 | Maximum non-NULL value, signed |
| 16 | 4 | Bitmap byte length, ceil(group rows / 8) |
| 20 | 4 | IEEE CRC-32 of group header bytes 0–19, then bitmap and value buffer |

Bitmap bit `i % 8` in byte `i / 8` is one for a non-NULL value. Unused bits in
the final bitmap byte must be zero. Every row has a four-byte value slot, including
NULL rows. NULL slots must contain zero. All-NULL groups have zero min/max fields;
the reader exposes their bounds as absent.

For example, `[INT_MIN, NULL, 7]` has bitmap `05` in hex. Its 77-byte reference
fixture is checked into the storage tests. Python `struct` and `zlib.crc32`
generated the fixture independently of the Rust encoder.

## Type tag 2: nullable int8

A type-2 group header is 32 bytes: row count (4), NULL count (4), minimum (8),
maximum (8), bitmap length (4), CRC-32 (4). Every row has an eight-byte `i64`
slot. All other rules match type 1. A type-1 reader rejects a type-2 segment as
an invalid header, and the reverse. The reference fixture
`crates/syntra-storage/tests/fixtures/int8-v1.bin` comes from
`build-support/fixtures/gen_int8_fixtures.py`, which uses Python `struct` and
`zlib.crc32` only.

## Validation and memory limits

The prototype caps a segment at 1,000,000 rows and 8 MiB of encoded bytes, for
both type tags. 1,000,000 `int8` rows encode to about 8.16 MB, inside the cap. The
writer stages one group and grows a bounded output buffer. The reader borrows
the encoded input and allocates no decoded column. Both poll cancellation between
groups. Execution emits its single summary only after the entire operation succeeds.

Checksums detect accidental damage. They do not establish that user-supplied
metadata is truthful. The reader therefore recomputes NULL counts and min/max
from all values before permitting pruning. It also enforces canonical NULL slots
and bitmap padding, even when checksums are valid.

The SQL adapter may materialize or detoast its input before these limits apply.
Encoding also copies the result into PostgreSQL memory. These limits do not cap
the whole backend's memory, and Rust allocations are not governed by `work_mem`.

## Scan semantics and counters

Each group is one bounded scan batch. The initial kernels support all rows,
`IS NULL`, `IS NOT NULL` and inclusive integer ranges. NULL never matches a range.
An inverted range matches nothing. Aggregates follow the existing int4 kernel's
NULL, empty-input and overflow rules.

The SQL API exposes all-row and range scans. It returns aggregate values and:

- `groups_scanned`: groups whose values entered predicate evaluation.
- `groups_pruned`: groups rejected by verified min/max or NULL metadata.
- `rows_examined`: rows in scanned groups, before the row predicate.
- `scan_bytes`: encoded group bytes selected by the scan, including group headers.
- `validation_bytes`: the complete encoded segment length validated beforehand.

These counters describe logical work on a `bytea` value. Validation reads every
group, so a small `scan_bytes` value does not establish reduced physical IO or
total read traffic. The kernel uses scalar loops within bounded groups; it has
no SIMD, compression, parallelism or general expression pipeline yet.

## Persistence boundary

SQL callers can save segments in ordinary PostgreSQL `bytea` columns. PostgreSQL
owns those table rows and their transaction, WAL and backup behavior. The Rust
reader also accepts bytes loaded from a file, but this milestone provides no
external-file catalog, manifest publication protocol, compaction or crash recovery.

An encoded value remains a fixed dataset after its source changes. It cannot
replace a live-table read. A future projection loader must establish source
provenance and snapshot eligibility before planner integration. A trusted
immutable-generation cache may later avoid repeated full validation, but it
needs a defined lifetime, invalidation and integrity contract first.
