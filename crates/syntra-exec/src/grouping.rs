//! Bounded, streaming GROUP BY for nullable int4 and int8 keys and values.
use crate::INTERRUPT_INTERVAL;
use std::{collections::HashMap, fmt, hash::Hash};
use syntra_core::{AggregateError, Int4Summary, Int8Summary};

/// Hard cap on distinct keys, including the SQL NULL group.
pub const MAX_GROUPS: usize = 65_536;
/// Logical bytes retained in distinct non-NULL keys, separate from RSS.
pub const MAX_KEY_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupingError {
    InvalidLimit,
    InvalidKeyLimit,
    KeyBytesLimitExceeded { limit: usize },
    GroupLimitExceeded { limit: usize },
    AllocationFailed,
    Cancelled,
    Aggregate(AggregateError),
}

impl fmt::Display for GroupingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLimit => write!(f, "group limit must be between 1 and {MAX_GROUPS}"),
            Self::InvalidKeyLimit => {
                write!(f, "key byte limit must be between 1 and {MAX_KEY_BYTES}")
            }
            Self::KeyBytesLimitExceeded { limit } => {
                write!(f, "distinct key bytes exceed limit {limit}")
            }
            Self::GroupLimitExceeded { limit } => write!(f, "group count exceeds limit {limit}"),
            Self::AllocationFailed => f.write_str("cannot allocate bounded grouping state"),
            Self::Cancelled => f.write_str("grouped aggregation cancelled"),
            Self::Aggregate(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for GroupingError {}

/// Owned-key counterpart for text/UUID adapters. Equality is supplied by K;
/// adapters must establish that it matches their SQL type and collation.
/// Key bytes count distinct retained payloads only, not repeated input keys.
/// Input adapters must also bound individual keys before copying/detoasting.
/// NULL consumes a group slot and zero payload bytes. Output is sorted by K,
/// NULL first; no locale sort order is implied by a text adapter.
pub fn group_owned_int8<K: Eq + Hash + Ord>(
    input: impl IntoIterator<Item = (Option<K>, Option<i64>)>,
    max_groups: usize,
    max_key_bytes: usize,
    key_bytes: impl Fn(&K) -> usize,
    keep_running: impl FnMut() -> bool,
) -> Result<Vec<GroupedSummary<K, Int8Summary>>, GroupingError> {
    group_owned(
        input,
        max_groups,
        max_key_bytes,
        key_bytes,
        keep_running,
        Int8Summary::push,
    )
}

/// Exact scaled decimal input/result; NaN is distinct from SQL NULL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecimalResult<T> {
    Null,
    Finite(T),
    NaN,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DecimalSummary {
    finite: Int8Summary,
    nans: i64,
}

impl DecimalSummary {
    pub fn push(&mut self, value: DecimalResult<i64>) -> Result<(), AggregateError> {
        match value {
            DecimalResult::Null => self.finite.push(None),
            DecimalResult::Finite(v) => self.finite.push(Some(v)),
            DecimalResult::NaN => {
                let nans = self
                    .nans
                    .checked_add(1)
                    .ok_or(AggregateError::CountOverflow)?;
                self.finite.push(None)?;
                self.nans = nans;
                Ok(())
            }
        }
    }
    pub fn rows(&self) -> i64 {
        self.finite.rows()
    }
    pub fn non_nulls(&self) -> i64 {
        self.finite.non_nulls() + self.nans
    }
    pub fn sum(&self) -> DecimalResult<i128> {
        if self.nans > 0 {
            DecimalResult::NaN
        } else {
            self.finite
                .sum()
                .map_or(DecimalResult::Null, DecimalResult::Finite)
        }
    }
    pub fn min(&self) -> DecimalResult<i64> {
        self.finite.min().map_or(
            if self.nans > 0 {
                DecimalResult::NaN
            } else {
                DecimalResult::Null
            },
            DecimalResult::Finite,
        )
    }
    pub fn max(&self) -> DecimalResult<i64> {
        if self.nans > 0 {
            DecimalResult::NaN
        } else {
            self.finite
                .max()
                .map_or(DecimalResult::Null, DecimalResult::Finite)
        }
    }
}

/// All inputs share one fixed scale, validated by the PostgreSQL adapter.
pub fn group_owned_decimal<K: Eq + Hash + Ord>(
    input: impl IntoIterator<Item = (Option<K>, DecimalResult<i64>)>,
    max_groups: usize,
    max_key_bytes: usize,
    key_bytes: impl Fn(&K) -> usize,
    keep_running: impl FnMut() -> bool,
) -> Result<Vec<GroupedSummary<K, DecimalSummary>>, GroupingError> {
    group_owned(
        input,
        max_groups,
        max_key_bytes,
        key_bytes,
        keep_running,
        DecimalSummary::push,
    )
}

fn group_owned<K: Eq + Hash + Ord, V, S: Default>(
    input: impl IntoIterator<Item = (Option<K>, V)>,
    max_groups: usize,
    max_key_bytes: usize,
    key_bytes: impl Fn(&K) -> usize,
    mut keep_running: impl FnMut() -> bool,
    push: fn(&mut S, V) -> Result<(), AggregateError>,
) -> Result<Vec<GroupedSummary<K, S>>, GroupingError> {
    if !(1..=MAX_GROUPS).contains(&max_groups) {
        return Err(GroupingError::InvalidLimit);
    }
    if !(1..=MAX_KEY_BYTES).contains(&max_key_bytes) {
        return Err(GroupingError::InvalidKeyLimit);
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
    let mut bytes = 0usize;
    for (key, value) in input {
        // Keys can be large, so poll every input rather than every 1024 rows.
        poll()?;
        if let Some(summary) = groups.get_mut(&key) {
            push(summary, value).map_err(GroupingError::Aggregate)?;
            continue;
        }
        if groups.len() == max_groups {
            return Err(GroupingError::GroupLimitExceeded { limit: max_groups });
        }
        bytes = bytes
            .checked_add(key.as_ref().map_or(0, &key_bytes))
            .filter(|b| *b <= max_key_bytes)
            .ok_or(GroupingError::KeyBytesLimitExceeded {
                limit: max_key_bytes,
            })?;
        let mut summary = S::default();
        push(&mut summary, value).map_err(GroupingError::Aggregate)?;
        groups.insert(key, summary);
    }
    poll()?;
    let mut result = Vec::new();
    result
        .try_reserve_exact(groups.len())
        .map_err(|_| GroupingError::AllocationFailed)?;
    result.extend(
        groups
            .into_iter()
            .map(|(key, summary)| GroupedSummary { key, summary }),
    );
    result.sort_unstable_by(|a, b| a.key.cmp(&b.key));
    poll()?;
    Ok(result)
}

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
