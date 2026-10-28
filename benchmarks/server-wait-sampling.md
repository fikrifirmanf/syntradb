# Backend wait sampling

```sh
bash scripts/benchmark-server-waits.sh --rows 100000 --repetitions 3
```

The script adds a private observer connection to the disposable benchmark cluster.
Every ten milliseconds it samples the known transactional backend PIDs through
`pg_stat_activity`. It does not inspect unrelated sessions or record query text.
Five-second phases and three rounds keep this diagnostic bounded.

Set `--wait-sample-ms 0` to disable sampling. Active intervals are 5–1000 ms. One
phase holds at most 7,000 query samples, each with at most sixteen backends. Missed
polls and capacity drops are explicit in the report. A slow observer skips overdue
ticks instead of issuing a burst of catch-up queries.

Each sample includes the client's send and receive offsets. The server observation
occurred somewhere inside that round trip; the report does not assign an exact
instant. It retains rows overlapping the slow request traces plus one sampling
interval of context, and reports state counts across all collected samples.

State counts are observations, not wait durations. A short wait may occur entirely
between samples. An idle backend near a slow client call is not by itself proof
that the database already committed that request. Client and server states need
to be interpreted within the same request window.

Sampling adds client, server and statistics work. Compare runs with the same
observer settings. These are diagnostics, not a zero-overhead performance baseline.
The observer has a two-second statement timeout. Shutdown joins the observer and
the request workers; observer shutdown time is excluded from workload drain time.

Ordinary PostgreSQL extended-protocol Execute logging precedes implicit commit at
Sync. A short Execute log therefore cannot rule out a later commit wait. Backend
wait samples cover that blind spot without changing the transaction protocol.

The collector deliberately keeps the current durability settings. It does not
disable WAL flushes or synchronous commit to make a latency number smaller.
