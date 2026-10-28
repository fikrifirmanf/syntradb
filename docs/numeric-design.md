# numeric value columns (0.4.0)

Status: implemented in the 0.4.0 workspace, 2026-10-28. Not released.

## Scope

A capture feed's value column may be `numeric(p,s)` with 1 ≤ p ≤ 18 and
0 ≤ s ≤ p. Keys stay `int2`, `int4` or `int8`. Unconstrained `numeric`,
precision above 18 and negative scales are refused when the feed is seeded.

## Storage

A value v is stored as the integer v × 10^s in the 8-byte layout. With p ≤ 18
its magnitude is below 10^18, which fits `int8`, so the event, base and
checkpoint formats, the catalog and the replay kernel are unchanged. The catalog
records value width 8, as for `int8`. A feed learns p and s from the live column
type, as an `int2` feed learns its width.

The source identity already includes `atttypmod`, so changing a captured
column's precision or scale makes writes fail closed until the feed is reseeded,
as any other type change does.

## Conversions

- The capture trigger and the seed read each value as `numeric` and convert its
  text form exactly: no rounding, no exponent. `NaN` cannot be stored as a
  scaled integer, so a write that stores `NaN` in a captured column is refused.
- Readers and apply hold stored values to |v × 10^s| < 10^p, the column's own
  range. A stored value outside it is corruption, as an out-of-range `int2` is.
- `syntra_checkpoint_numeric_summary(source, column, replay_tail)` returns
  `numeric` sum, min and max at scale s. The sum is exact: the wide accumulator
  holds 1,000,000 values below 10^18. Its heap fallback scans the table in the
  caller's snapshot with the same exact conversion. It does not use SPI, because
  a query from a transaction that has an XID would take a new snapshot. A
  stored `NaN` makes the sum and the maximum `NaN`, and the minimum `NaN` only
  when every non-null value is `NaN`, as PostgreSQL's aggregates do.

## Downgrade

0.3.0 cannot capture `numeric`, so `syntradb--0.4.0--0.3.0.sql` refuses while any
capture binding's value column is `numeric`. Stop those feeds first.

## Tests

Eleven PostgreSQL tests compare checkpoint reads with PostgreSQL's own
aggregates. They cover fractions and the `numeric(10,2)` limits, `numeric(18,0)`
sums past the `bigint` range, 40 rounds of churn with deltas and folds, `NaN` on
the heap route, and refusals: a `NaN` write or seed, unconstrained `numeric`,
`numeric(19,0)`, a `numeric` key, a stored value past the precision, and a
widened column. Four unit tests cover typmods, text conversion and wide sums.
The suite passed with 184 PostgreSQL tests, the upgrade checks from 0.1.0,
0.2.0 and 0.3.0 passed with 504 identical catalog entries, and the downgrade
check passed, including its refusal with a `numeric` feed. Three planted bugs
each failed tests: no precision bound, a dropped sign, and an output scale off
by one. A downgrade guard that matched `money` instead of `numeric` failed the
downgrade check. Results: `benchmarks/results/numeric-checks-2026-10-28.log`.

The physical restore check passed with `numeric(12,2)` feeds at 1, 16 and 64
writers, and the standby check passed twice
([recovery](recovery.md#physical-backup-and-point-in-time-restore),
`benchmarks/results/numeric-recovery-2026-10-28.log`).

The synchronous, cascading and failback check and the quorum check each passed
twice with a `numeric(12,2)` feed
([recovery](recovery.md#synchronous-standby-cascading-standby-and-failback)).

Five of six 20-minute soaks with a `numeric(18,2)` feed passed; the first
stopped with a pruner error whose cause was not recorded. Writer tail latency
varied from run to run as it does for `int4`
([checkpoint soak](checkpoint-soak.md#numeric-2026-10-28)).

## Not covered

`numeric` keys, precision above 18, `NaN`, the cause of the failed soak, a
downgrade under load with a `numeric` feed (the downgrade refuses while one
exists), and other aggregates such as `avg`.
