# Integer grouping benchmark

The manual `Integer grouping benchmark` workflow compares the explicit Rust SQL
API with native PostgreSQL GROUP BY on the same heap and backend. Start it from
GitHub Actions, or use:

```sh
gh workflow run grouping.yml --ref main -f source_ref=main -f rows=1000000 -f repetitions=7
```

For a local harness check:

```sh
bash scripts/grouping-benchmark.sh --rows 10000 --repetitions 3
```

The workflow lives on the default branch so GitHub can dispatch it. Its source
ref selects the checkout to build and measure. It defaults to `main`.
The artifact records the checked-out
commit, which can differ from the workflow's own commit.

## Workload

The matrix has two layouts, `int4/int4` and `int8/int8`, and group budgets of
16, 1,024 and 65,536. Budgets include the NULL group. Keys cycle through the
other slots; every 29th key is NULL. Every 17th value is NULL. Signed values
repeat over 2,001 integers. The wide layout multiplies those values by 10^12.
The report records the actual number of groups, which may be below the budget
on small datasets.

Each dataset runs with parallel workers per gather set to zero, then to
PostgreSQL's built-in default. The rest of this harness configures zero globally,
so the benchmark reads `pg_settings.boot_val` to recover that default. Native
SQL may use parallel workers; the Rust function is parallel unsafe. The saved
plans show whether PostgreSQL launched workers. JIT stays off for both routes.
The report includes work memory, parallel costs, heap size and input row count.

There are no concurrent writers or captured sources in this comparison. The
table is analyzed after bulk insertion. Both queries return ordered groups with
identical key, count, sum, minimum and maximum types. SUM follows
`sum(value::bigint)` and is serialized as text in both routes.

## Measurements and checks

Each timing method gives each route one warmup. Later rounds alternate which
route runs first. Raw prepared-request times include receipt of all result rows.
The harness compares every field of every group against native SQL after each
request. It refuses a timing report on any mismatch or wrong input row count.

Separate EXPLAIN ANALYZE samples record server execution time, buffers and the
last complete plan for each route. EXPLAIN instruments native plan nodes and
the opaque function differently; use prepared requests for the direct latency
comparison. The report preserves raw samples rather than averaging ratios.
Seven samples do not establish a population tail or a broad speedup.

The workflow saves the source commit, runner details, load samples, logs and JSON
for seven days. Its final check requires the complete matrix and sample counts.
Compare repeated runs on the same runner type before drawing conclusions.
This workload covers two integer layouts with one grouping key. It does not
establish performance for skewed data, filters, multiple keys, cold caches,
concurrent writes, other databases or automatic planner routing.

## Harness verification

A local million-row run completed all 12 cases with three timed samples per
route. Every dataset filled its intended group budget, all groups matched native
SQL, and the native default-parallel plans launched two workers. This validates
the harness; Linux performance results are still pending.

Changing the Rust query's sums deliberately caused a result mismatch. The command
failed and wrote no timing report. That change was restored. The workflow's
report verifier also rejected a missing case, a missing timing sample and a
wrong row count. The NULL-key cadence is coprime with each key cycle, so it does
not permanently remove key slots.
The generic missing-test mapper expects a separate `.test.rs` file for this
module. Here the real database run exercises the measurement and comparison
logic directly. The SQL API also has separate PostgreSQL regression tests.
