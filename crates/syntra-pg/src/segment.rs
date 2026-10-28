//! Explicit value-based API; no association with live heap tables or their MVCC.
use pgrx::datum::Bytea;
use pgrx::prelude::*;
use syntra_exec::{summarize_segment, ExecutionError, Int4Predicate};
use syntra_storage::SegmentError;

fn keep_running() -> bool {
    pgrx::check_for_interrupts!();
    true
}

fn segment_error(error: SegmentError) -> ! {
    use pgrx::PgSqlErrorCode::*;
    let code = match error {
        SegmentError::UnsupportedVersion(_) => ERRCODE_FEATURE_NOT_SUPPORTED,
        SegmentError::LimitExceeded => ERRCODE_PROGRAM_LIMIT_EXCEEDED,
        SegmentError::AllocationFailed => ERRCODE_OUT_OF_MEMORY,
        SegmentError::Cancelled => ERRCODE_QUERY_CANCELED,
        _ => ERRCODE_INVALID_BINARY_REPRESENTATION,
    };
    pgrx::ereport!(ERROR, code, error.to_string());
}

fn execution_error(error: ExecutionError) -> ! {
    match error {
        ExecutionError::Segment(error) => segment_error(error),
        ExecutionError::Cancelled => segment_error(SegmentError::Cancelled),
        ExecutionError::Aggregate(error) => {
            pgrx::ereport!(
                ERROR,
                pgrx::PgSqlErrorCode::ERRCODE_NUMERIC_VALUE_OUT_OF_RANGE,
                error.to_string()
            );
        }
    }
}

#[pg_extern(immutable, parallel_safe)]
fn syntra_int4_segment(input: Array<'_, i32>) -> Vec<u8> {
    if input.len() > syntra_storage::MAX_ROWS {
        segment_error(SegmentError::LimitExceeded);
    }
    syntra_storage::encode_int4(input.iter(), keep_running)
        .unwrap_or_else(|error| segment_error(error))
}

type SummaryRow = (
    i64,
    i64,
    Option<i64>,
    Option<i32>,
    Option<i32>,
    i64,
    i64,
    i64,
    i64,
    i64,
);

fn summarize(bytes: &[u8], predicate: Int4Predicate) -> SummaryRow {
    let result = summarize_segment(bytes, predicate, keep_running)
        .unwrap_or_else(|error| execution_error(error));
    let s = result.summary;
    let stats = result.stats;
    (
        s.rows(),
        s.non_nulls(),
        s.sum(),
        s.min(),
        s.max(),
        i64::from(stats.groups_scanned),
        i64::from(stats.groups_pruned),
        i64::from(stats.rows_examined),
        i64::from(stats.scan_bytes),
        i64::from(stats.validation_bytes),
    )
}

// Named output columns must be inline for pgrx SQL schema generation.
#[allow(clippy::type_complexity)]
#[pg_extern(immutable, parallel_safe)]
fn syntra_segment_summary(
    segment: Bytea<'_>,
) -> TableIterator<
    'static,
    (
        name!(rows, i64),
        name!(non_nulls, i64),
        name!(sum, Option<i64>),
        name!(min, Option<i32>),
        name!(max, Option<i32>),
        name!(groups_scanned, i64),
        name!(groups_pruned, i64),
        name!(rows_examined, i64),
        name!(scan_bytes, i64),
        name!(validation_bytes, i64),
    ),
> {
    TableIterator::new(std::iter::once(summarize(
        segment.as_bytes(),
        Int4Predicate::All,
    )))
}

// Named output columns must be inline for pgrx SQL schema generation.
#[allow(clippy::type_complexity)]
#[pg_extern(immutable, parallel_safe)]
fn syntra_segment_range_summary(
    segment: Bytea<'_>,
    lower: i32,
    upper: i32,
) -> TableIterator<
    'static,
    (
        name!(rows, i64),
        name!(non_nulls, i64),
        name!(sum, Option<i64>),
        name!(min, Option<i32>),
        name!(max, Option<i32>),
        name!(groups_scanned, i64),
        name!(groups_pruned, i64),
        name!(rows_examined, i64),
        name!(scan_bytes, i64),
        name!(validation_bytes, i64),
    ),
> {
    TableIterator::new(std::iter::once(summarize(
        segment.as_bytes(),
        Int4Predicate::Range { lower, upper },
    )))
}
