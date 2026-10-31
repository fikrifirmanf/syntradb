# Final correctness checks after a soak limit failure

The soak now completes final apply/prune, keyed comparison, checkpoint reuse and
vacuum before evaluating the timed service limits. Exceeding the offered-load
limit therefore no longer skips those independent correctness checks.

The limits still use the original timed windows, WAL boundary and worker samples.
A failed limit still produces a nonzero exit and no passing JSON report.
Errors in the workers, sampled resource budgets or final correctness checks
still stop the run. The final success marker covers only the checks named in it;
it is not a passing service verdict.

`elapsed_including_drain_seconds` now reads the clock after final validation.
Previously it reused the workers-joined duration and omitted the drain.

## Regression evidence

Two local four-minute runs temporarily set the scheduled p99 ceiling to zero.
The old order failed that ceiling and skipped final validation. With the new
order, final apply/prune, keyed comparison, checkpoint reuse and vacuum passed
before the same service failure. Both commands failed and wrote no passing JSON.
Only the four expected p99 failures appeared in either verdict.

The production ceiling was restored to 1,000 ms. A subsequent four-minute run
at 30 offered cycles per worker per second passed all four windows, final
checks and service limits. This smoke run does not replace the failed two-hour
test or prove sustained production throughput.

The source fingerprint in the passing report is
`d708aff5ce9311f513eb2e0a0d1502801d386555770cfed91e0eefca452ef72f`.
Local logs and the passing report use the prefix
`benchmarks/results/soak-final-order-`: `before.log`, `after.log`, `normal.log`
and `normal.json`. Temporary fault injection was removed before the normal run.
Existing soak-limit unit tests and benchmark tests also passed. The generic
missing-test mapper does not recognize these embedded Rust tests; the real
database runs above exercise the changed orchestration order.

The [native Linux CI run](https://github.com/fikrifirmanf/syntradb/actions/runs/41172223169)
also passed on `a790c73`. It includes 218 PostgreSQL tests, benchmark tests and
six Python tests. This CI result supplements the local failure-order probes;
it does not replace a new long soak.
