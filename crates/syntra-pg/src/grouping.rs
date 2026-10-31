//! Explicit fresh grouping over integer heap columns at the caller's snapshot.
use pgrx::prelude::*;
use pgrx::PgRelation;
use syntra_exec::GroupingError;

fn grouping_error(error: GroupingError) -> ! {
    let code = match error {
        GroupingError::InvalidLimit => PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
        GroupingError::GroupLimitExceeded { .. } => PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
        GroupingError::AllocationFailed => PgSqlErrorCode::ERRCODE_OUT_OF_MEMORY,
        GroupingError::Cancelled => PgSqlErrorCode::ERRCODE_QUERY_CANCELED,
        GroupingError::Aggregate(_) => PgSqlErrorCode::ERRCODE_NUMERIC_VALUE_OUT_OF_RANGE,
    };
    pgrx::ereport!(ERROR, code, error.to_string());
}

/// SUM widens every supported integer value to bigint, then uses PostgreSQL's
/// numeric sum range. No floating-point conversion or source mutation occurs.
#[allow(clippy::type_complexity)]
#[pg_extern(stable, parallel_unsafe)]
fn syntra_heap_group_integer_summary(
    source: PgRelation,
    key_column: &str,
    value_column: &str,
    max_groups: default!(i32, 1024),
) -> TableIterator<
    'static,
    (
        name!(group_key, Option<i64>),
        name!(rows, i64),
        name!(non_nulls, i64),
        name!(sum, Option<AnyNumeric>),
        name!(min, Option<i64>),
        name!(max, Option<i64>),
    ),
> {
    let max_groups = usize::try_from(max_groups)
        .ok()
        .filter(|limit| (1..=syntra_exec::MAX_GROUPS).contains(limit))
        .unwrap_or_else(|| grouping_error(GroupingError::InvalidLimit));
    let groups = crate::source::group_integer(&source, key_column, value_column, max_groups)
        .unwrap_or_else(|error| grouping_error(error));
    TableIterator::new(groups.into_iter().map(|group| {
        pgrx::check_for_interrupts!();
        let s = group.summary;
        (
            group.key,
            s.rows(),
            s.non_nulls(),
            s.sum().map(AnyNumeric::from),
            s.min(),
            s.max(),
        )
    }))
}
