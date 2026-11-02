# Design-partner pilot brief

SyntraDB is not production-ready (see [readiness](readiness.md)). This brief
describes a limited pilot on a workload where a mistake costs nothing. It is
tracked by issue #15.

## Who fits

- You run PostgreSQL 18.6 and can add an extension on a non-critical database
  or a replica restored from a backup.
- A table has an integer primary key and an integer or fixed-scale numeric
  measure that you aggregate often (sums, counts, min, max, grouped by an
  integer, text or UUID key, or filtered by a date range).
- Writers on that table stay below a few hundred transactions per second.
- Someone can read the [operations runbook](operations.md) and watch the feed.

## Who does not

- Anyone who needs PostgreSQL 16 or 17, partitioned or inherited tables,
  row-level security on the source, or TRUNCATE on it.
- Anyone who cannot tolerate writer errors. A full capture log refuses writes
  with error 54000 until the feed is drained (issue #17).

## What the pilot does

1. Install the [pilot bundle](pilot-packages.md) into a private PostgreSQL copy.
2. Seed capture on one table and keep the maintenance worker running.
3. Compare each SyntraDB summary against the same native SQL query. Any
   difference is a bug and goes straight to an issue.
4. Run for two weeks, recording lag, writer latency against an untouched control
   table, and every error.

## What we ask back

- Two short written reports (week one and week two): what worked, what failed,
  numbers from the feed status SQL.
- Permission to publish anonymized measurements.
- A plain go or no-go note.

## What we promise

- No claim of production readiness before the gates in [readiness](readiness.md)
  pass.
- Fixes for correctness bugs take priority over every other work.
- Reports of vulnerabilities go through the [security policy](../SECURITY.md).
