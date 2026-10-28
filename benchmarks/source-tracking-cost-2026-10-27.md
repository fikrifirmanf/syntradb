# Source counter cost

The first tracked-source run completed 90,000 transfers without missed arrivals,
SQL errors or balance invariant failures. All twelve enabled phases recorded
exactly 5,000 source mutations, matching their successful transactions. Native
PostgreSQL phases had no tracker.

This proves the counter was exercised. It does not establish acceptable overhead.
The transaction-only request p99 values were:

| Round | Native PostgreSQL, ms | Enabled with tracker, ms |
|---|---:|---:|
| 1 | 0.546 | 0.855 |
| 2 | 0.596 | 0.759 |
| 3 | 0.637 | 0.908 |
| Median across rounds | 0.596 | 0.855 |

The median increased about 43%. These values include the driver, server and
commit round trip. They are not server-only timings or merged population p99s.
One tracked phase also had a 19.199 ms scheduled-to-commit p99. Client scheduling
and storage spikes still affect the wider latency distribution.

Each phase offered 1,000 transfers per second for five seconds. Four clients
updated two accounts in one prepared statement. Analytical phases used one
saturated client. The source had 100,000 rows. Rust used release builds and
PostgreSQL assertions were off. WAL zero filling was off in this APFS experiment;
fsync, synchronous commit and full-page writes remained on. Ten-millisecond wait
sampling ran in all phases. No compile work overlapped the timings.

The shared counter adds a row update and can serialize writers. Its cost does
not meet the provisional mixed-latency budget in this run. The 1,000 TPS offered
load also does not measure either system's maximum throughput. Tracking is off
by default. Future work must address both this write path and the DDL/history
gaps before using it for live analytical routing.

Raw report: `results/source-tracking-cost-2026-10-27.json`. The report records
tracking state and the final generation per phase. Reproduce with:

```sh
bash scripts/benchmark-server-waits.sh --track-source true --wal-init-zero false
```
