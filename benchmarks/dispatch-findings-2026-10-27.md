# Longer-run dispatch findings: 2026-10-27

Three rounds used ten seconds per phase, 100,000 accounts, four transaction
clients, one saturated analytical client and 1,000 offered transfers per second.
The PostgreSQL build and pacing policy matched the prior run.
The new tracing code adds work. Run length is not the only changed factor.

All 180,000 offered transfers succeeded. There were no missed arrivals, SQL errors
or invariant failures. All eighteen phase records had the expected trace counts. Each retained trace
had valid times, exact component sums and a unique ID within its view.

[The archived JSON](results/dispatch-lag-2026-10-27.json) contains full histograms
and bounded correlated traces. See the [timing contract](dispatch-diagnostics.md).
Generated JSON stays local.

## Transaction p99

Each cell shows scheduled-to-commit / dispatch-to-commit p99 in milliseconds.
Those are separate distributions, not necessarily the same request.

| Phase | Round 1 | Round 2 | Round 3 |
|---|---:|---:|---:|
| PostgreSQL idle | 2.747 / 0.803 | 1.769 / 0.446 | 1.367 / 0.425 |
| Enabled idle | 2.113 / 1.003 | 1.186 / 0.377 | 1.234 / 0.427 |
| PostgreSQL heap | 1.047 / 0.224 | 1.031 / 0.083 | 1.128 / 0.266 |
| Enabled heap | 1.040 / 0.176 | 1.039 / 0.226 | 1.406 / 0.252 |
| Frozen copy | 1.039 / 0.082 | 1.039 / 0.104 | 1.085 / 0.221 |
| Fresh capture | 1.029 / 0.077 | 1.051 / 0.200 | 1.066 / 0.242 |

Most mixed analytical phases had scheduled p99 near 1 ms. Vanilla transaction-only
phases also showed dispatch lag. Longer windows did not eliminate request stalls.

## Same-request evidence

The table shows the largest scheduled-latency request from selected phases.
Every component in a row belongs to the same request. Times are milliseconds.

| Phase / round | Arrival | Pre-dispatch lag | Request duration | Total |
|---|---:|---:|---:|---:|
| Enabled heap / 3 | 6408 | 0.067 | 73.155 | 73.222 |
| PostgreSQL idle / 2 | 8651 | 0.767 | 57.865 | 58.632 |
| PostgreSQL idle / 3 | 2317 | 0.415 | 44.795 | 45.210 |
| Fresh capture / 3 | 3382 | 0.796 | 35.179 | 35.975 |
| PostgreSQL heap / 3 | 7648 | 0.796 | 27.875 | 28.671 |

The largest retained request had only 0.067 ms of pre-dispatch lag but spent
73.155 ms inside the client execution call. A vanilla transaction-only request
likewise spent 57.865 ms after dispatch. These records rule out a single
explanation that all long stalls came from pacing before dispatch.

Subsequent arrivals on a blocked worker can accumulate backlog. The retained
dispatch traces show previous completion and readiness times to expose that chain.
The full dispatch-lag histogram covers every attempt, while retained events are
only the sixteen worst examples in each category.

Request duration includes client driver scheduling, communication, server work
and commit. These traces cannot isolate which of those caused a stall. Backend
PIDs are retained for future server-side correlation, but no server wait sampler
or per-statement duration log was enabled in this run.

## Implication

This run kept the data correct and gave us paired request times. It does not
prove a stable tail-latency limit or a cause specific to SyntraDB. Stalls
also occurred without the extension. The earlier three-second outliers were not
traced individually, so their exact cause remains unknown.

The next diagnostic should correlate slow client requests with server durations
or wait events. Keep the current pacing policy until that evidence distinguishes
server delay from client-side delay after dispatch. Fresh incremental projection
maintenance and workload isolation remain separate implementation work.
