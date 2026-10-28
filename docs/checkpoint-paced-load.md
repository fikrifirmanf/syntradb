# Paced checkpoint load

The checkpoint harness can offer a fixed number of cycles per worker each second.
Replay can be enabled or disabled for a comparison using the same source, workers
and correctness checks.

```sh
bash scripts/checkpoint-paced.sh --checkpoint-worker-rate 100 --checkpoint-reader-replay false
bash scripts/checkpoint-paced.sh --checkpoint-worker-rate 100 --checkpoint-reader-replay true
```

Both commands use a disposable cluster, 10,000 rows and a 30-second worker phase.
Choose distinct `--output` paths to retain both reports. The wrapper defaults to
replay off. The older saturated stress wrapper retains its explicit replay mode.
The public checkpoint API still defaults to heap fallback on lag.

`--checkpoint-worker-rate` accepts zero for continuous saturation, or 10–1,000
cycles per second per worker. A nonzero rate requires an enabled checkpoint
stress phase. Duration remains 5–300 seconds. `--checkpoint-reader-replay` accepts
only `true` or `false`. It controls the prepared read parameter for all stress
sessions, including own-write and retained-snapshot checks.

Each of eight workers owns an absolute arrival schedule starting at slot zero.
A delayed worker drops older due slots and starts the latest due slot. It never
queues an unbounded backlog or sends a catch-up burst. Waiting occurs outside
transactions. Workers check the stop flag while waiting. An in-flight cycle can
finish after the deadline; no new cycle starts after it.

Reports include planned arrivals, attempted cycles and missed arrivals for each
worker. In paced mode, planned arrivals equal rate times duration. Missed arrivals
are planned minus attempted cycles, including skipped slots and work unstarted
at the deadline. Expected lock or capacity rejections count as attempted cycles
and have separate histograms. Saturated mode has no planned arrival count; its
planned and missed fields are zero.

Service histograms measure cycle start through completion. Scheduled histograms
measure the assigned arrival through completion, including dispatch delay.
Rejected cycles have both histograms too. Always read missed counts alongside
percentiles: latency histograms exclude dropped arrivals. There is one synchronous
connection per worker, so this is a finite-load test with loss under overload.
It does not model an unlimited external request queue.

The [existing stress contract](checkpoint-concurrency-stress.md) still defines
all transactions, native aggregate and keyed oracles, rollback checks, retained
snapshots, prune accounting and final drain. Every progress gate still applies,
including more than 4,096 emitted events. A short or slow run can fail that gate.
Equal cycle rates differ from equal SQL request rates: repeatable-read cycles
perform nine checks, while writes and maintenance use other transaction shapes.
The aggregate oracle itself adds native SQL work to reader timings.

Use two replay modes at the same offered rate to assess admission. Keep process
builds, other tests and compiler jobs out of the timed worker phase. The reports
retain WAL, allocated storage, service tails and missed work. A single local pair
cannot establish production capacity or broad performance superiority.

The [first paired results](../benchmarks/checkpoint-paced-2026-10-27.md) retain
all six raw reports, missed counts, source inputs and limits of interpretation.
