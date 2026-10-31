# Bounded integer grouping kernel

`syntra_exec::group_int4` and `group_int8` stream nullable integer keys and values
into grouped COUNT, SUM, MIN and MAX summaries. They retain group state rather
than input rows. The `int8` path keeps sums in `i128`, including results beyond
the `bigint` range, matching PostgreSQL's `sum(bigint)` numeric results.
The SQL planner uses PostgreSQL's existing GROUP BY implementation. This Rust
entry point also backs the explicit [fresh SQL grouping API](heap-grouped-aggregates.md)
in 0.8.0.

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
cargo test --locked -p syntra-exec --test grouping_int8
```

Tests cover empty input, NULL keys, NULL-only groups, negative keys, integer
endpoints, every admitted group slot, invalid limits, early stream termination
and cancellation. Independent PostgreSQL 18.6 oracles supply expected results
for 5,000 rows across 14 groups in each layout. Forward and reversed input both
match them. Wide values exercise sums beyond the `bigint` range, and wide keys
exercise both `bigint` endpoints.

The oracle's [SQL](../crates/syntra-exec/tests/fixtures/grouped_int4.sql) and
[results](../crates/syntra-exec/tests/fixtures/grouped_int4.txt) are committed.
The wide [SQL](../crates/syntra-exec/tests/fixtures/grouped_int8.sql) and
[results](../crates/syntra-exec/tests/fixtures/grouped_int8.txt) are committed too.
Regenerate the result against a disposable PostgreSQL 18.6 database with
`psql -XAtq -v ON_ERROR_STOP=1 --pset=null=NULL -f grouped_int4.sql`.

Allowing one extra group deliberately made the early-termination regression fail.
Restoring the limit check made it pass. These checks establish the stated kernel
semantics. Deliberately narrowing wide values to `int4` also failed the wide oracle
check. The SQL API has separate snapshot and permission tests. Planner routing,
range grouping, multiple columns and spill to disk remain future work.
