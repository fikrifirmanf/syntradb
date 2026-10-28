# Correlated dispatch diagnostics

Run the longer local benchmark with the existing production-style PostgreSQL build:

```sh
bash scripts/benchmark-diagnostics.sh --rows 100000 --repetitions 7
```

The script runs three rounds with ten seconds per mixed phase. It keeps the
existing 1,000 offered transfers per second, four transaction clients and one
analytical client. Six phases still separate native heap reads, fresh captures
and frozen projections. Override those bounded options as needed.

Reports go to `benchmarks/results/dispatch-lag-latest.json`. The pacing algorithm
is unchanged. Instrumentation adds client bookkeeping, so this run should not be
treated as a controlled duration-only comparison with older reports.

## Timing boundaries

Every transfer attempt records when the worker is ready, when it should send,
when it sends, and when the call ends. Times use microseconds from the phase
start. Readiness before that start is clamped to zero. Each trace names the
worker, arrival index, backend PID and time when the prior request ended.

| Field | Meaning |
|---|---|
| `scheduled_us` | Declared arrival time |
| `ready_us` | Worker begins handling this arrival, before pacing |
| `dispatched_us` | Immediately before the client executes the prepared transfer |
| `completed_us` | Immediately after the execution call returns |
| `previous_completed_us` | Previous attempted request's completion on this worker |
| `sqlstate` | NULL on success, SQLSTATE on database error, or `client_error` |

Derived values refer to the same request:

- `dispatch_lag_us` is dispatch minus scheduled arrival.
- `backlog_us` is the positive part of readiness minus scheduled arrival.
- `after_ready_us` is dispatch minus the later of readiness and scheduled arrival.
- `request_to_commit_us` is completion minus dispatch.
- `scheduled_to_commit_us` is completion minus scheduled arrival.

Dispatch lag equals backlog plus the delay after readiness. Total scheduled
latency equals dispatch lag plus request duration. Trace decompositions use
integer microseconds. Histograms round separate duration measurements, so they
can differ by a microsecond at a boundary.

Backlog can include a previous in-flight request, client bookkeeping or time when
the worker was not scheduled. If the worker was ready before arrival but dispatched
late, the delay occurred during pacing or local preparation. This tells us
where a delay occurred. It does not tell us why.

The request call includes client work, network IO, server waits and commit.
It is not a timer for the server alone. Backend PIDs can link a trace to future
server logs. This adds no SQL call to a timed transfer.

## Bounded retention

The histograms cover all attempts, including errors. They track send lag,
backlog and delay after readiness. Counts show arrivals delayed by at least one,
five and ten milliseconds. The existing successful-request latency distributions
and completeness checks remain unchanged.

Each worker keeps the sixteen largest dispatch delays, request durations and total
scheduled durations. Merging retains the global sixteen per category and removes
duplicate identities within each view. Counts still cover every attempt. These
are tail examples. Use the full histograms to assess the whole run.

Invalid time order stops the run. The code cannot silently emit a false trace. Failed requests retain their error code, while transaction errors and missed
arrivals continue to suppress incomplete p99 comparisons.

## Interpretation limits

The clients and PostgreSQL still share one local host. Longer windows reduce one
source of noise but do not isolate client scheduling, server contention or hardware
power behavior. The script does not change thread priorities, busy-spin between
arrivals or move the client to a separate machine. Frozen copies still omit change
propagation; their throughput cannot establish fresh HTAP performance.
