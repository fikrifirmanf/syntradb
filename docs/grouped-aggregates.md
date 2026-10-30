# Bounded integer grouping kernel

`syntra_exec::group_int4` streams nullable integer keys and values into grouped
COUNT, SUM, MIN and MAX summaries. It retains group state rather than input rows.
The SQL planner uses PostgreSQL's existing GROUP BY implementation. This Rust
entry point will support a later analytical route.

```rust
use syntra_exec::group_int4;

let groups = group_int4(
    [(Some(1), Some(8)), (None, Some(3)), (Some(1), None)],
    16,
    || true,
)?;
// NULL key: rows=1, non_nulls=1, sum=3, min=3, max=3
// Key 1:    rows=2, non_nulls=1, sum=8, min=8, max=8
```

## Contract

- NULL keys share one group. NULL values contribute to COUNT(*) and are excluded
  from COUNT(value), SUM, MIN and MAX.
- A group containing only NULL values has zero non-NULL values and NULL aggregates.
- Empty input produces no groups.
- Results use ascending integer key order with NULL first, matching an explicit
  `ORDER BY key NULLS FIRST`.
- The caller supplies a group limit from 1 to 65,536. NULL occupies one admitted
  slot. Repeated keys continue accumulating when every slot is occupied.
- A new key beyond the limit fails the whole operation. No partial groups escape.
- Counts and sums use the core kernel's checked integer arithmetic.
- Cancellation is checked before allocation, at most every 1,024 examined rows,
  before output allocation and after sorting.

The hash table reserves capacity before consuming input, and allocation errors
return `GroupingError::AllocationFailed`. Output storage holds at most the admitted
group count. The group cap bounds state cardinality; it is not a backend memory
quota. Hash table capacity and allocator overhead exceed the stored payload size.

The caller supplies snapshot-correct input. The kernel does not acquire snapshots
or restart a consumed stream. A future SQL fallback after a limit error must use
the original pinned snapshot and discard the failed attempt's state.

## Verification

```sh
cargo test --locked -p syntra-exec --test grouping
```

Tests cover empty input, NULL keys, NULL-only groups, negative keys, integer
endpoints, every admitted group slot, invalid limits, early stream termination
and cancellation. An independent PostgreSQL 18.6 oracle supplies expected results
for 5,000 rows across 14 groups. Forward and reversed input both match it.

The oracle's [SQL](../crates/syntra-exec/tests/fixtures/grouped_int4.sql) and
[results](../crates/syntra-exec/tests/fixtures/grouped_int4.txt) are committed.
Regenerate the result against a disposable PostgreSQL 18.6 database with
`psql -XAtq -v ON_ERROR_STOP=1 --pset=null=NULL -f grouped_int4.sql`.

Allowing one extra group deliberately made the early-termination regression fail.
Restoring the limit check made it pass. These checks establish the stated kernel
semantics. SQL grouping acceleration, range grouping, multiple columns and spill
to disk remain future work.
