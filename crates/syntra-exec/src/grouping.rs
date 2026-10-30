//! Bounded, streaming GROUP BY for nullable int4 and int8 keys and values.
use crate::INTERRUPT_INTERVAL;
use std::{collections::HashMap, fmt, hash::Hash};
use syntra_core::{AggregateError, Int4Summary, Int8Summary};

/// Hard cap on distinct keys, including the SQL NULL group.
pub const MAX_GROUPS: usize = 65_536;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupingError {
    InvalidLimit,
    GroupLimitExceeded { limit: usize },
    AllocationFailed,
    Cancelled,
    Aggregate(AggregateError),
}

impl fmt::Display for GroupingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLimit => write!(f, "group limit must be between 1 and {MAX_GROUPS}"),
            Self::GroupLimitExceeded { limit } => write!(f, "group count exceeds limit {limit}"),
            Self::AllocationFailed => f.write_str("cannot allocate bounded grouping state"),
            Self::Cancelled => f.write_str("grouped aggregation cancelled"),
            Self::Aggregate(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for GroupingError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GroupedSummary<K, S> {
    pub key: Option<K>,
    pub summary: S,
}

pub type GroupedInt4Summary = GroupedSummary<i32, Int4Summary>;
pub type GroupedInt8Summary = GroupedSummary<i64, Int8Summary>;

/// Consumes `(key, value)` pairs without retaining input rows. NULL keys share
/// one group; values use PostgreSQL COUNT/SUM/MIN/MAX semantics. Empty input
/// produces no groups. Results are ordered by key, with NULL first.
///
/// `max_groups` must be in 1..=MAX_GROUPS. Exceeding it returns an error without
/// partial output, so a future SQL caller must discard this attempt or fall back
/// using the same snapshot. The hash table reserves space before reading input;
/// result storage is bounded by the actual group count. This is a group-count
/// limit, not a PostgreSQL backend RSS quota.
///
/// Cancellation is checked before allocation, at most every 1024 examined rows,
/// before output allocation and after sorting. No partial result is returned.
pub fn group_int4(
    input: impl IntoIterator<Item = (Option<i32>, Option<i32>)>,
    max_groups: usize,
    keep_running: impl FnMut() -> bool,
) -> Result<Vec<GroupedInt4Summary>, GroupingError> {
    group_nullable(input, max_groups, keep_running, Int4Summary::push)
}

/// Wide counterpart of `group_int4`, with the same limits, NULL semantics,
/// ordering and cancellation contract. Sums use i128, matching PostgreSQL's
/// numeric result for sum(bigint) throughout the admitted bigint count range.
pub fn group_int8(
    input: impl IntoIterator<Item = (Option<i64>, Option<i64>)>,
    max_groups: usize,
    keep_running: impl FnMut() -> bool,
) -> Result<Vec<GroupedInt8Summary>, GroupingError> {
    group_nullable(input, max_groups, keep_running, Int8Summary::push)
}

fn group_nullable<K: Copy + Eq + Hash + Ord, V, S: Default>(
    input: impl IntoIterator<Item = (Option<K>, Option<V>)>,
    max_groups: usize,
    mut keep_running: impl FnMut() -> bool,
    push: fn(&mut S, Option<V>) -> Result<(), AggregateError>,
) -> Result<Vec<GroupedSummary<K, S>>, GroupingError> {
    if !(1..=MAX_GROUPS).contains(&max_groups) {
        return Err(GroupingError::InvalidLimit);
    }
    let mut poll = || {
        if keep_running() {
            Ok(())
        } else {
            Err(GroupingError::Cancelled)
        }
    };
    poll()?;
    let mut groups: HashMap<Option<K>, S> = HashMap::new();
    groups
        .try_reserve(max_groups)
        .map_err(|_| GroupingError::AllocationFailed)?;
    let mut until_check = INTERRUPT_INTERVAL;
    for (key, value) in input {
        if until_check == 0 {
            poll()?;
            until_check = INTERRUPT_INTERVAL;
        }
        // Check the cap only for a new key: repeated keys and NULLs must keep
        // accumulating even when all admitted group slots are occupied.
        if groups.len() >= max_groups && !groups.contains_key(&key) {
            return Err(GroupingError::GroupLimitExceeded { limit: max_groups });
        }
        push(groups.entry(key).or_default(), value).map_err(GroupingError::Aggregate)?;
        until_check -= 1;
    }
    poll()?;
    let mut output = Vec::new();
    output
        .try_reserve_exact(groups.len())
        .map_err(|_| GroupingError::AllocationFailed)?;
    output.extend(
        groups
            .into_iter()
            .map(|(key, summary)| GroupedSummary { key, summary }),
    );
    output.sort_unstable_by_key(|group| group.key);
    poll()?;
    Ok(output)
}
