//! PostgreSQL-independent SQL aggregate semantics for int4 and int8 columns.
#![forbid(unsafe_code)]

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggregateError {
    CountOverflow,
    SumOverflow,
}

impl fmt::Display for AggregateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::CountOverflow => "aggregate count exceeds bigint range",
            Self::SumOverflow => "integer sum exceeds bigint range",
        })
    }
}

impl std::error::Error for AggregateError {}

/// COUNT(*) and COUNT(value) are zero on empty input; SUM/MIN/MAX are NULL.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Int4Summary {
    rows: i64,
    non_nulls: i64,
    sum: Option<i64>,
    min: Option<i32>,
    max: Option<i32>,
}

impl Int4Summary {
    /// Updates are atomic with respect to errors: an overflow leaves state intact.
    pub fn push(&mut self, value: Option<i32>) -> Result<(), AggregateError> {
        let rows = self
            .rows
            .checked_add(1)
            .ok_or(AggregateError::CountOverflow)?;
        if let Some(value) = value {
            let non_nulls = self
                .non_nulls
                .checked_add(1)
                .ok_or(AggregateError::CountOverflow)?;
            let sum = self
                .sum
                .unwrap_or(0)
                .checked_add(i64::from(value))
                .ok_or(AggregateError::SumOverflow)?;
            self.non_nulls = non_nulls;
            self.sum = Some(sum);
            self.min = Some(self.min.map_or(value, |old| old.min(value)));
            self.max = Some(self.max.map_or(value, |old| old.max(value)));
        }
        self.rows = rows;
        Ok(())
    }

    pub fn rows(&self) -> i64 {
        self.rows
    }
    pub fn non_nulls(&self) -> i64 {
        self.non_nulls
    }
    pub fn sum(&self) -> Option<i64> {
        self.sum
    }
    pub fn min(&self) -> Option<i32> {
        self.min
    }
    pub fn max(&self) -> Option<i32> {
        self.max
    }
}

/// int8 aggregates. PostgreSQL's `sum(bigint)` returns `numeric`, so the sum is
/// kept in `i128`: 2^63 rows of the largest `int8` still fit.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Int8Summary {
    rows: i64,
    non_nulls: i64,
    sum: Option<i128>,
    min: Option<i64>,
    max: Option<i64>,
}

impl Int8Summary {
    /// Updates are atomic with respect to errors: an overflow leaves state intact.
    pub fn push(&mut self, value: Option<i64>) -> Result<(), AggregateError> {
        let rows = self
            .rows
            .checked_add(1)
            .ok_or(AggregateError::CountOverflow)?;
        if let Some(value) = value {
            let non_nulls = self
                .non_nulls
                .checked_add(1)
                .ok_or(AggregateError::CountOverflow)?;
            let sum = self
                .sum
                .unwrap_or(0)
                .checked_add(i128::from(value))
                .ok_or(AggregateError::SumOverflow)?;
            self.non_nulls = non_nulls;
            self.sum = Some(sum);
            self.min = Some(self.min.map_or(value, |old| old.min(value)));
            self.max = Some(self.max.map_or(value, |old| old.max(value)));
        }
        self.rows = rows;
        Ok(())
    }

    pub fn rows(&self) -> i64 {
        self.rows
    }
    pub fn non_nulls(&self) -> i64 {
        self.non_nulls
    }
    pub fn sum(&self) -> Option<i128> {
        self.sum
    }
    pub fn min(&self) -> Option<i64> {
        self.min
    }
    pub fn max(&self) -> Option<i64> {
        self.max
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_has_sql_null_aggregates() {
        let s = Int4Summary::default();
        assert_eq!(
            (s.rows(), s.non_nulls(), s.sum(), s.min(), s.max()),
            (0, 0, None, None, None)
        );
    }

    #[test]
    fn nulls_contribute_only_to_row_count() {
        let mut s = Int4Summary::default();
        s.push(None).unwrap();
        s.push(None).unwrap();
        assert_eq!(
            (s.rows(), s.non_nulls(), s.sum(), s.min(), s.max()),
            (2, 0, None, None, None)
        );
    }

    #[test]
    fn mixed_values_match_sql_aggregates() {
        let mut s = Int4Summary::default();
        for value in [Some(-4), None, Some(7), Some(0), Some(7)] {
            s.push(value).unwrap();
        }
        assert_eq!(
            (s.rows(), s.non_nulls(), s.sum(), s.min(), s.max()),
            (5, 4, Some(10), Some(-4), Some(7))
        );
    }

    #[test]
    fn int4_sum_uses_bigint_range() {
        let mut s = Int4Summary::default();
        for value in [i32::MAX, i32::MAX, i32::MIN] {
            s.push(Some(value)).unwrap();
        }
        assert_eq!(s.sum(), Some(2_147_483_646));
        assert_eq!((s.min(), s.max()), (Some(i32::MIN), Some(i32::MAX)));
    }

    #[test]
    fn int8_sum_exceeds_bigint_like_postgresql_numeric() {
        let mut s = Int8Summary::default();
        for value in [Some(i64::MAX), None, Some(i64::MAX), Some(i64::MIN)] {
            s.push(value).unwrap();
        }
        assert_eq!(
            (s.rows(), s.non_nulls(), s.sum(), s.min(), s.max()),
            (
                4,
                3,
                Some(i128::from(i64::MAX) - 1),
                Some(i64::MIN),
                Some(i64::MAX)
            )
        );
        let mut big = Int8Summary::default();
        big.push(Some(i64::MAX)).unwrap();
        big.push(Some(i64::MAX)).unwrap();
        assert_eq!(big.sum(), Some(2 * i128::from(i64::MAX)));
        assert_eq!(Int8Summary::default().sum(), None);
    }

    #[test]
    fn int8_sum_overflow_does_not_change_state() {
        let mut s = Int8Summary {
            rows: 1,
            non_nulls: 1,
            sum: Some(i128::MAX),
            min: Some(1),
            max: Some(1),
        };
        let before = s;
        assert_eq!(s.push(Some(1)), Err(AggregateError::SumOverflow));
        assert_eq!(s, before);
    }

    #[test]
    fn count_overflow_does_not_change_state() {
        // Seed the boundary directly instead of iterating i64::MAX input rows.
        let mut s = Int4Summary {
            rows: i64::MAX,
            ..Default::default()
        };
        let before = s;
        assert_eq!(s.push(Some(1)), Err(AggregateError::CountOverflow));
        assert_eq!(s, before);
    }

    #[test]
    fn sum_overflow_and_underflow_do_not_change_state() {
        for (sum, next) in [(i64::MAX, 1), (i64::MIN, -1)] {
            let mut s = Int4Summary {
                rows: 1,
                non_nulls: 1,
                sum: Some(sum),
                min: Some(next),
                max: Some(next),
            };
            let before = s;
            assert_eq!(s.push(Some(next)), Err(AggregateError::SumOverflow));
            assert_eq!(s, before);
        }
    }
}
