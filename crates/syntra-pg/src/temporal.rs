//! Explicit fresh aggregates over native PostgreSQL temporal scan keys.
use pgrx::prelude::*;
use pgrx::PgRelation;

type Summary = (
    i64,
    i64,
    Option<AnyNumeric>,
    Option<AnyNumeric>,
    Option<AnyNumeric>,
);

fn summarize<T: IntoDatum>(
    source: &PgRelation,
    value: &str,
    time: &str,
    lower: T,
    upper: T,
    column_type: pg_sys::Oid,
) -> Summary {
    let datum = |v: T| {
        v.into_datum()
            .unwrap_or_else(|| crate::source::unsupported("time bounds must not be NULL"))
    };
    let (s, nans, scale) = crate::source::time_summary(
        source,
        value,
        time,
        crate::source::TimeRange {
            column_type,
            lower: datum(lower),
            upper: datum(upper),
        },
    );
    let nan =
        || AnyNumeric::try_from("NaN").unwrap_or_else(|_| pgrx::error!("invalid numeric NaN"));
    (
        s.0,
        s.1,
        if nans > 0 {
            Some(nan())
        } else {
            s.2.map(|v| crate::numeric::to_numeric(v, scale))
        },
        s.3.map(|v| crate::numeric::to_numeric(i128::from(v), scale))
            .or_else(|| (nans > 0).then(nan)),
        if nans > 0 {
            Some(nan())
        } else {
            s.4.map(|v| crate::numeric::to_numeric(i128::from(v), scale))
        },
    )
}

#[allow(clippy::type_complexity)]
#[pg_extern(stable, parallel_unsafe)]
fn syntra_heap_date_summary(
    source: PgRelation,
    value_column: &str,
    time_column: &str,
    lower: Date,
    upper: Date,
) -> TableIterator<
    'static,
    (
        name!(rows, i64),
        name!(non_nulls, i64),
        name!(sum, Option<AnyNumeric>),
        name!(min, Option<AnyNumeric>),
        name!(max, Option<AnyNumeric>),
    ),
> {
    TableIterator::new(std::iter::once(summarize(
        &source,
        value_column,
        time_column,
        lower,
        upper,
        pg_sys::DATEOID,
    )))
}

#[allow(clippy::type_complexity)]
#[pg_extern(stable, parallel_unsafe)]
fn syntra_heap_timestamp_summary(
    source: PgRelation,
    value_column: &str,
    time_column: &str,
    lower: Timestamp,
    upper: Timestamp,
) -> TableIterator<
    'static,
    (
        name!(rows, i64),
        name!(non_nulls, i64),
        name!(sum, Option<AnyNumeric>),
        name!(min, Option<AnyNumeric>),
        name!(max, Option<AnyNumeric>),
    ),
> {
    TableIterator::new(std::iter::once(summarize(
        &source,
        value_column,
        time_column,
        lower,
        upper,
        pg_sys::TIMESTAMPOID,
    )))
}

#[allow(clippy::type_complexity)]
#[pg_extern(stable, parallel_unsafe)]
fn syntra_heap_timestamptz_summary(
    source: PgRelation,
    value_column: &str,
    time_column: &str,
    lower: TimestampWithTimeZone,
    upper: TimestampWithTimeZone,
) -> TableIterator<
    'static,
    (
        name!(rows, i64),
        name!(non_nulls, i64),
        name!(sum, Option<AnyNumeric>),
        name!(min, Option<AnyNumeric>),
        name!(max, Option<AnyNumeric>),
    ),
> {
    TableIterator::new(std::iter::once(summarize(
        &source,
        value_column,
        time_column,
        lower,
        upper,
        pg_sys::TIMESTAMPTZOID,
    )))
}
