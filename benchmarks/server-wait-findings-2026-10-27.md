# WAL allocation and latency stalls

Two diagnostic runs completed 90,000 transfers each with no missed arrivals,
transaction errors, or balance invariant failures. Each used 100,000 rows, four
transaction workers, a 1,000 TPS offered load, five-second phases, and three rounds.
A ten-millisecond observer sampled the transactional backends. The host uses APFS.

| Measurement across 18 phases | WAL zero filling on | WAL zero filling off |
|---|---:|---:|
| WAL initialization wait observations | 16 | 0 |
| Largest scheduled-to-commit p99, ms | 38.783 | 17.407 |
| Largest request-to-commit maximum, ms | 86.463 | 65.791 |

The first run used PostgreSQL's fresh-cluster default for `wal_init_zero`. That
early report did not record the setting explicitly. The second report records
`wal_init_zero=off`, `wal_recycle=on`, and `wal_sync_method=open_datasync`. Both kept
fsync, full-page writes and synchronous commit on. The runner now records all
these settings. Its default still uses WAL zero filling.

The largest vanilla PostgreSQL request in the first run overlapped seven
`WalInitWrite` samples and one `WalInitSync` sample. Fresh capture phases also
overlapped those waits. These samples identify WAL creation as a contributor to
the stalls, including a case without the extension installed.

With zero filling off, the longest enabled heap request overlapped six
`DataFileExtend` samples. A frozen-copy phase overlapped six `WalWrite` samples.
Other storage stalls therefore remain. Sampling records observations, not their
duration. It cannot assign every part of a client request to one server wait.

PostgreSQL documents that skipping WAL zero filling may help copy-on-write file
systems. It writes the last byte to establish the new file size instead. See the
[PostgreSQL WAL settings](https://www.postgresql.org/docs/18/runtime-config-wal.html).
This local experiment supports testing that option on this host. It does not
establish a deployment default or a repeatable latency guarantee.

Raw reports: `results/server-waits-zeroed-2026-10-27.json` and
`results/server-waits-sparse-2026-10-27.json`. Reproduce with
`scripts/benchmark-server-waits.sh`, then repeat with `--wal-init-zero false`.
The reports retain timings, backend PIDs, wait observations and durability settings.
No compile jobs ran alongside the timed phases. Wider data, longer runs and
equivalent fresh-data semantics remain necessary for engine comparisons.
