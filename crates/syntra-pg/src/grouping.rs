//! Explicit fresh grouping over integer heap columns at the caller's snapshot.
use pgrx::prelude::*;
use pgrx::PgRelation;
use syntra_exec::GroupingError;

pub(super) fn grouping_error(error: GroupingError) -> ! {
    let code = match error {
        GroupingError::InvalidLimit => PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
        GroupingError::InvalidKeyLimit => PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
        GroupingError::KeyBytesLimitExceeded { .. } => {
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED
        }
        GroupingError::GroupLimitExceeded { .. } => PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
        GroupingError::AllocationFailed => PgSqlErrorCode::ERRCODE_OUT_OF_MEMORY,
        GroupingError::Cancelled => PgSqlErrorCode::ERRCODE_QUERY_CANCELED,
        GroupingError::Aggregate(_) => PgSqlErrorCode::ERRCODE_NUMERIC_VALUE_OUT_OF_RANGE,
    };
    pgrx::ereport!(ERROR, code, error.to_string());
}

fn limits(groups: i32, bytes: i32) -> (usize, usize) {
    let groups = usize::try_from(groups)
        .ok()
        .filter(|n| (1..=syntra_exec::MAX_GROUPS).contains(n))
        .unwrap_or_else(|| grouping_error(GroupingError::InvalidLimit));
    let bytes = usize::try_from(bytes)
        .ok()
        .filter(|n| (1..=syntra_exec::MAX_KEY_BYTES).contains(n))
        .unwrap_or_else(|| grouping_error(GroupingError::InvalidKeyLimit));
    (groups, bytes)
}

#[allow(clippy::type_complexity)]
#[pg_extern(stable, parallel_unsafe)]
fn syntra_heap_group_text_summary(
    source: PgRelation,
    key_column: &str,
    value_column: &str,
    max_groups: default!(i32, 1024),
    max_key_bytes: default!(i32, 8388608),
) -> TableIterator<
    'static,
    (
        name!(group_key, Option<String>),
        name!(rows, i64),
        name!(non_nulls, i64),
        name!(sum, Option<AnyNumeric>),
        name!(min, Option<i64>),
        name!(max, Option<i64>),
    ),
> {
    let (groups, bytes) = limits(max_groups, max_key_bytes);
    let groups = crate::source::group_text(&source, key_column, value_column, groups, bytes)
        .unwrap_or_else(|e| grouping_error(e));
    TableIterator::new(groups.into_iter().map(|g| {
        pgrx::check_for_interrupts!();
        let s = g.summary;
        (
            g.key,
            s.rows(),
            s.non_nulls(),
            s.sum().map(AnyNumeric::from),
            s.min(),
            s.max(),
        )
    }))
}

#[allow(clippy::type_complexity)]
#[pg_extern(stable, parallel_unsafe)]
fn syntra_heap_group_uuid_summary(
    source: PgRelation,
    key_column: &str,
    value_column: &str,
    max_groups: default!(i32, 1024),
    max_key_bytes: default!(i32, 8388608),
) -> TableIterator<
    'static,
    (
        name!(group_key, Option<pgrx::Uuid>),
        name!(rows, i64),
        name!(non_nulls, i64),
        name!(sum, Option<AnyNumeric>),
        name!(min, Option<i64>),
        name!(max, Option<i64>),
    ),
> {
    let (groups, bytes) = limits(max_groups, max_key_bytes);
    let groups = crate::source::group_uuid(&source, key_column, value_column, groups, bytes)
        .unwrap_or_else(|e| grouping_error(e));
    TableIterator::new(groups.into_iter().map(|g| {
        pgrx::check_for_interrupts!();
        let s = g.summary;
        (
            g.key,
            s.rows(),
            s.non_nulls(),
            s.sum().map(AnyNumeric::from),
            s.min(),
            s.max(),
        )
    }))
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

fn decimal_result<T: Into<i128>>(
    value: syntra_exec::DecimalResult<T>,
    scale: crate::numeric::Scale,
) -> Option<AnyNumeric> {
    match value {
        syntra_exec::DecimalResult::Null => None,
        syntra_exec::DecimalResult::Finite(v) => Some(crate::numeric::to_numeric(v.into(), scale)),
        syntra_exec::DecimalResult::NaN => Some(
            AnyNumeric::try_from("NaN").unwrap_or_else(|_| pgrx::error!("invalid numeric NaN")),
        ),
    }
}

#[allow(clippy::type_complexity)]
#[pg_extern(stable, parallel_unsafe)]
fn syntra_heap_group_integer_numeric_summary(
    source: PgRelation,
    key_column: &str,
    value_column: &str,
    max_groups: default!(i32, 1024),
    max_key_bytes: default!(i32, 8388608),
) -> TableIterator<
    'static,
    (
        name!(group_key, Option<i64>),
        name!(rows, i64),
        name!(non_nulls, i64),
        name!(sum, Option<AnyNumeric>),
        name!(min, Option<AnyNumeric>),
        name!(max, Option<AnyNumeric>),
    ),
> {
    let (groups, bytes) = limits(max_groups, max_key_bytes);
    let (groups, scale) =
        crate::source::group_integer_numeric(&source, key_column, value_column, groups, bytes)
            .unwrap_or_else(|e| grouping_error(e));
    TableIterator::new(groups.into_iter().map(move |g| {
        pgrx::check_for_interrupts!();
        let s = g.summary;
        (
            g.key,
            s.rows(),
            s.non_nulls(),
            decimal_result(s.sum(), scale),
            decimal_result(s.min(), scale),
            decimal_result(s.max(), scale),
        )
    }))
}

#[allow(clippy::type_complexity)]
#[pg_extern(stable, parallel_unsafe)]
fn syntra_heap_group_text_numeric_summary(
    source: PgRelation,
    key_column: &str,
    value_column: &str,
    max_groups: default!(i32, 1024),
    max_key_bytes: default!(i32, 8388608),
) -> TableIterator<
    'static,
    (
        name!(group_key, Option<String>),
        name!(rows, i64),
        name!(non_nulls, i64),
        name!(sum, Option<AnyNumeric>),
        name!(min, Option<AnyNumeric>),
        name!(max, Option<AnyNumeric>),
    ),
> {
    let (groups, bytes) = limits(max_groups, max_key_bytes);
    let (groups, scale) =
        crate::source::group_text_numeric(&source, key_column, value_column, groups, bytes)
            .unwrap_or_else(|e| grouping_error(e));
    TableIterator::new(groups.into_iter().map(move |g| {
        pgrx::check_for_interrupts!();
        let s = g.summary;
        (
            g.key,
            s.rows(),
            s.non_nulls(),
            decimal_result(s.sum(), scale),
            decimal_result(s.min(), scale),
            decimal_result(s.max(), scale),
        )
    }))
}

#[allow(clippy::type_complexity)]
#[pg_extern(stable, parallel_unsafe)]
fn syntra_heap_group_uuid_numeric_summary(
    source: PgRelation,
    key_column: &str,
    value_column: &str,
    max_groups: default!(i32, 1024),
    max_key_bytes: default!(i32, 8388608),
) -> TableIterator<
    'static,
    (
        name!(group_key, Option<pgrx::Uuid>),
        name!(rows, i64),
        name!(non_nulls, i64),
        name!(sum, Option<AnyNumeric>),
        name!(min, Option<AnyNumeric>),
        name!(max, Option<AnyNumeric>),
    ),
> {
    let (groups, bytes) = limits(max_groups, max_key_bytes);
    let (groups, scale) =
        crate::source::group_uuid_numeric(&source, key_column, value_column, groups, bytes)
            .unwrap_or_else(|e| grouping_error(e));
    TableIterator::new(groups.into_iter().map(move |g| {
        pgrx::check_for_interrupts!();
        let s = g.summary;
        (
            g.key,
            s.rows(),
            s.non_nulls(),
            decimal_result(s.sum(), scale),
            decimal_result(s.min(), scale),
            decimal_result(s.max(), scale),
        )
    }))
}
