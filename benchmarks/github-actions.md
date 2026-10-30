# Run the checkpoint soak on GitHub Actions

The `Checkpoint soak benchmark` workflow runs the production `int4` workload
with recovery checks. It defaults to 7,200 seconds: one hour with a held snapshot,
then one hour after release. Choose 1,200 seconds for a smoke run.

1. Push this checkout to your GitHub repository, including the workflow on its default branch.
2. Open **Actions → Checkpoint soak benchmark → Run workflow**.
3. Select the branch and duration, then start the run.
4. Download the `checkpoint-soak` artifact from the completed run.

The artifact contains the source commit, runner hardware, build log, load samples,
soak log and JSON report. Failed runs retain their logs; the harness writes its
JSON report only when it completes successfully. Artifacts expire after seven days.
The workflow runs only when started manually. It grants read access to repository contents.

The job allows five hours for setup, compilation and the soak. The production
benchmark command has a three-hour timeout, including its build. A timeout or
failed correctness gate fails the job. The upload step also runs after failure.

Standard GitHub-hosted runners have free compute for public repositories.
Private repositories use the owner's monthly compute allowance, then incur charges
if billing permits. Larger runners and artifact storage have separate billing rules.
See [GitHub Actions billing](https://docs.github.com/en/billing/concepts/product-billing/github-actions)
and [Actions limits](https://docs.github.com/en/actions/reference/limits).

Use a public repository if you intend to publish SyntraDB's source and history.
Keep the repository private if the source should stay private.

These runs provide Linux correctness and performance observations. Compare repeated
runs on the same runner type and workload. Check `environment.txt` and `load.log`
for differences. Results from this runner do not establish a speedup over the
earlier macOS runs. A comparison of 0.6.0 and 0.7.0 needs both versions measured
under the same conditions.

## Recovery and lifecycle verification

Start **Actions → Recovery and lifecycle verification → Run workflow** to run
21 independent checks on Ubuntu 24.04 x86_64. Four jobs run at a time. A failed
job does not cancel the other checks.

- Process crash and logical restore checks.
- Background maintenance, busy locks and broken-source isolation.
- Physical backup and point-in-time restore with four writers across two feeds,
  for each of `int2`, `int4`, `int8` and `numeric`.
- Standby reads and promotion for those four layouts.
- Synchronous cascading replication, rewind and quorum failover for `int4`
  and `numeric`.
- Upgrades from each archived release, 0.1.0 through 0.6.0, to the current release.
- Downgrade from 0.7.0 to 0.6.0 and update back to 0.7.0.

Each job builds from source. Its artifact retains the environment, setup and
check logs, plus server logs and catalog comparisons smaller than 10 MiB each.
Artifacts expire after seven days. Database directories are not uploaded.
The checks use disposable local clusters, private Unix sockets and existing
correctness assertions. They do not connect to a deployed database.

Passing this workflow provides evidence for the tested source commit and workloads.
It does not cover power loss, faulty storage, real network partitions or sustained
production traffic. Keep the [readiness gates](../docs/readiness.md) open until
their remaining requirements have evidence.
