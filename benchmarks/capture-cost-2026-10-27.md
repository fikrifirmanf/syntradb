# Fresh capture cost

The source adapter now gets its row count directly from the bounded writer.
Previously, it reopened the new segment and validated every byte to recover that
count. Removing this extra pass reduced median capture request time in four local
comparisons. Every later segment read still performs full validation.

Each run used 100,000 rows and 51 requests per layout. Requests returned the row
count and encoded length; they did not write a destination table. The source was
unchanged and warm. PostgreSQL assertions were off, Rust used release builds, and
durability settings stayed on. No compile work overlapped timed phases.

| Run | Layout | Before median, ms | After median, ms | Reduction |
|---|---|---:|---:|---:|
| First | Ordered | 2.993375 | 2.881791 | 3.73% |
| First | Shuffled | 3.033042 | 2.866375 | 5.50% |
| Repeat | Ordered | 2.998042 | 2.900875 | 3.24% |
| Repeat | Shuffled | 3.015375 | 2.923417 | 3.05% |

These request times include heap reads, encoding and the client round trip. The
result is a narrow local gain. It does not predict mixed-workload tail latency,
cold storage behavior, or another engine's performance. Runs happened in sequence,
so host drift remains a limit even with a repeated before/after pair.

Raw reports are under `benchmarks/results/`: `capture-before-2026-10-27.json`,
`capture-after-2026-10-27.json`, `capture-before-repeat-2026-10-27.json`, and
`capture-after-repeat-2026-10-27.json`. The runner retains all capture samples and
checks result dimensions on every request. Reproduce the current path with:

```sh
bash scripts/benchmark-production.sh --mixed-seconds 0 --repetitions 51
```

Count tests cover empty data, NULLs, group boundaries and the one-million-row cap.
The independent format fixture remains unchanged. Mutating the returned count and
removing the final cancellation poll each caused a test failure before restoration.
