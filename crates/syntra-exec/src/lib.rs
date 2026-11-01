//! Streaming execution kernels with bounded state and no PostgreSQL APIs.
#![forbid(unsafe_code)]

use std::fmt;
use syntra_core::{AggregateError, Int4Summary};
use syntra_storage::{Int4Group, Int4Segment, SegmentError};

mod grouping;
pub use grouping::{
    group_int4, group_int8, group_owned_int8, GroupedInt4Summary, GroupedInt8Summary,
    GroupedSummary, GroupingError, MAX_GROUPS, MAX_KEY_BYTES,
};
pub use grouping::{group_owned_decimal, DecimalResult, DecimalSummary};

pub const INTERRUPT_INTERVAL: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionError {
    Cancelled,
    Aggregate(AggregateError),
    Segment(SegmentError),
}

impl fmt::Display for ExecutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => f.write_str("analytical execution cancelled"),
            Self::Aggregate(error) => error.fmt(f),
            Self::Segment(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for ExecutionError {}

impl From<SegmentError> for ExecutionError {
    fn from(error: SegmentError) -> Self {
        match error {
            SegmentError::Cancelled => Self::Cancelled,
            error => Self::Segment(error),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Int4Predicate {
    All,
    IsNull,
    IsNotNull,
    /// Inclusive integer range, matching SQL BETWEEN. NULL never matches.
    Range {
        lower: i32,
        upper: i32,
    },
}

impl Int4Predicate {
    fn may_match(self, group: &Int4Group<'_>) -> bool {
        match self {
            Self::All => true,
            Self::IsNull => group.nulls() > 0,
            Self::IsNotNull => group.nulls() < group.rows(),
            Self::Range { lower, upper } => {
                lower <= upper
                    && group.max().is_some_and(|max| max >= lower)
                    && group.min().is_some_and(|min| min <= upper)
            }
        }
    }

    fn matches(self, value: Option<i32>) -> bool {
        match self {
            Self::All => true,
            Self::IsNull => value.is_none(),
            Self::IsNotNull => value.is_some(),
            Self::Range { lower, upper } => {
                value.is_some_and(|value| value >= lower && value <= upper)
            }
        }
    }
}

/// Scan counters exclude validation. Validation reads the entire encoded value.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ScanStats {
    pub groups_scanned: u32,
    pub groups_pruned: u32,
    pub rows_examined: u32,
    pub scan_bytes: u32,
    pub validation_bytes: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanResult {
    pub summary: Int4Summary,
    pub stats: ScanStats,
}

/// Validates before producing any results, then scans one bounded group at a time.
/// No decoded column or selection vector is allocated for this initial kernel.
pub fn summarize_segment(
    bytes: &[u8],
    predicate: Int4Predicate,
    mut keep_running: impl FnMut() -> bool,
) -> Result<ScanResult, ExecutionError> {
    let segment = Int4Segment::open(bytes, &mut keep_running)?;
    let mut summary = Int4Summary::default();
    let mut stats = ScanStats {
        validation_bytes: segment.byte_len(),
        ..Default::default()
    };
    for group in segment.groups() {
        if !keep_running() {
            return Err(ExecutionError::Cancelled);
        }
        let group = group?;
        if !predicate.may_match(&group) {
            stats.groups_pruned += 1;
            continue;
        }
        stats.groups_scanned += 1;
        stats.rows_examined += group.rows();
        stats.scan_bytes += group.byte_len();
        for value in group.values() {
            if predicate.matches(value) {
                summary.push(value).map_err(ExecutionError::Aggregate)?;
            }
        }
    }
    if !keep_running() {
        return Err(ExecutionError::Cancelled);
    }
    Ok(ScanResult { summary, stats })
}

/// `keep_running` is checked before processing the first row and every 1024 rows.
/// The PostgreSQL adapter uses this boundary to service backend interrupts.
pub fn summarize_int4(
    input: impl IntoIterator<Item = Option<i32>>,
    keep_running: impl FnMut() -> bool,
) -> Result<Int4Summary, ExecutionError> {
    summarize_int4_where(input, Int4Predicate::All, keep_running)
}

/// Streaming predicates poll by examined rows, even when no input matches.
pub fn summarize_int4_where(
    input: impl IntoIterator<Item = Option<i32>>,
    predicate: Int4Predicate,
    mut keep_running: impl FnMut() -> bool,
) -> Result<Int4Summary, ExecutionError> {
    let mut summary = Int4Summary::default();
    let mut until_check = 0;
    for value in input {
        if until_check == 0 {
            if !keep_running() {
                return Err(ExecutionError::Cancelled);
            }
            until_check = INTERRUPT_INTERVAL;
        }
        if predicate.matches(value) {
            summary.push(value).map_err(ExecutionError::Aggregate)?;
        }
        until_check -= 1;
    }
    // Also service cancellation for empty inputs and at completion.
    if !keep_running() {
        return Err(ExecutionError::Cancelled);
    }
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streams_without_collecting_and_handles_boundaries() {
        let s = summarize_int4((0..2050).map(Some), || true).unwrap();
        assert_eq!(
            (s.rows(), s.non_nulls(), s.sum(), s.min(), s.max()),
            (2050, 2050, Some(2_100_225), Some(0), Some(2049))
        );
    }

    #[test]
    fn empty_input_still_checks_cancellation() {
        assert_eq!(summarize_int4([], || false), Err(ExecutionError::Cancelled));
    }

    #[test]
    fn cancellation_stops_a_long_stream() {
        let mut polls = 0;
        let mut consumed = 0;
        let result = summarize_int4(
            (0..10_000).map(|n| {
                consumed += 1;
                Some(n)
            }),
            || {
                polls += 1;
                polls < 2
            },
        );
        assert_eq!(result, Err(ExecutionError::Cancelled));
        assert_eq!(consumed, INTERRUPT_INTERVAL + 1);
    }

    #[test]
    fn nullable_stream_preserves_sql_semantics() {
        let s = summarize_int4([None, Some(-1), Some(0), None, Some(5)], || true).unwrap();
        assert_eq!(
            (s.rows(), s.non_nulls(), s.sum(), s.min(), s.max()),
            (5, 3, Some(4), Some(-1), Some(5))
        );
    }
}
