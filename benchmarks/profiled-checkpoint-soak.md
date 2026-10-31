# Profile the held-snapshot soak

The optional profiler records CPU stacks and worker activity without changing
the soak's correctness, resource or service limits. Profiled timings include
measurement overhead. Use them to find costs, then validate a fix with an
unprofiled run under the original limits.

Start with the four-minute integration smoke:

```sh
gh workflow run benchmark.yml --ref perf/held-snapshot-profiles -f duration=240 -f profile=true
```

After that passes, run the two-hour investigation:

```sh
gh workflow run benchmark.yml --ref perf/held-snapshot-profiles -f duration=7200 -f profile=true
```

The branch supplies the workflow and source checkout. The environment artifact
records its exact commit. Profiling defaults to off for ordinary soak runs.

## Capture windows

Each CPU window lasts 15 seconds. The two-hour schedule samples minutes 8, 28
and 58 while the snapshot is retained, then minute 65 after release. The
20-minute schedule uses minutes 2, 8, 12 and 18. The smoke uses minutes 1 and 3.
Schedules start when the observer first sees all nine named workers. Discovery
polling adds a small offset; the metadata records that reference explicitly.

The workflow installs Linux perf and runs a preflight before building PostgreSQL.
The preflight must deliver actual CPU samples from a disposable busy process.
Missing permissions, unsupported events or empty samples fail the job early.
The collector checks again before waiting for the soak workers.

Recording uses the userspace `cpu-clock` event at 49 Hz, with a 16 KiB DWARF
stack dump. [Perf's recording manual](https://man7.org/linux/man-pages/man1/perf-record.1.html)
describes this mode for optimized programs that omit frame pointers. PostgreSQL
and Rust retain their existing production build flags. There is no debug-build
substitute for the measured workload.

## Scope and artifacts

The collector attaches only to the nine named client workers in an owned
`/tmp/syntra-bench-*` cluster. Duplicate actors, missing workers or changing PIDs
fail collection. It does not attach system-wide or connect to a deployed database.
All data comes from the generated benchmark fixture.

During each window, one bounded PostgreSQL activity query per second records
worker states, wait events, snapshot horizons and current fixture SQL. CPU
profiles exclude kernel time and background processes such as autovacuum.
The normal soak samples and vmstat log provide additional context. CPU sample
percentages are not wall-clock latency percentages.

Artifacts retain raw `perf.data`, PID-to-actor mappings, activity JSON, recording
commands and errors. Report generation runs after the timed workload to avoid
adding its CPU cost to the soak. It produces self-cost and call-chain reports,
sample counts, and copies the PostgreSQL and extension binaries for symbol lookup.
Unresolved symbols and lost-sample warnings stay visible. Inspect them before
using a profile to assign costs.

Collection errors fail the job and preserve available files. A service-limit
failure still fails the benchmark, even when the CPU profiles are complete.
Artifacts expire after seven days. This diagnostic run does not waive the open
throughput gate or establish production readiness.

## Tooling verification

Six offline tests cover the capture schedules, actor and PID validation,
userspace-only command arguments, PID sample counting, kernel-denial propagation
and incomplete collection. They stub external process calls. Accepting boolean
PIDs deliberately failed the identity regression; the strict check was restored.
The explicit unittest suite runs these alongside the six existing Python tests.
The generic test connector does not detect this standalone unittest layout.
The native Linux preflight and smoke below verify actual kernel sampling.

## Native Linux integration evidence

The [four-minute smoke](https://github.com/fikrifirmanf/syntradb/actions/runs/41208524399)
passed at `b808942`. Its preflight, collection, report generation and every soak
gate passed. The minute 1 and 3 windows contain 409 and 309 CPU samples, with
zero lost samples. Both retain 16 activity observations covering all nine actors.
The idle vacuum worker had no CPU samples. PostgreSQL and SyntraDB symbols and
call chains are readable; 14 and 16 self samples have unnamed libc or vDSO frames.

Both reader workers missed zero arrivals. Each writer missed 15 of 7,200 arrivals
(0.21%). Final keyed comparison and checkpoint reuse passed, with zero unexpected
errors or invariant failures. This short smoke does not reproduce the earlier
two-hour throughput failure.

The [regular CI run](https://github.com/fikrifirmanf/syntradb/actions/runs/41208519872)
also passed 218 PostgreSQL tests, 12 Python tests and the Rust checks.
The [compact evidence](evidence/profile-smoke-linux-2026-10-31.json) preserves
job results, sample counts, quality limits, service measurements and the original
report checksum. Full raw files remain in the seven-day workflow artifact.

The first attempt stopped before PostgreSQL was built because this runner image
lacked `rg`. The retry installs `ripgrep` explicitly and passed without changing
the workload or its limits.
