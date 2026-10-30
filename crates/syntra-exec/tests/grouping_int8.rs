use syntra_exec::{group_int8, GroupingError};

#[test]
fn wide_grouping_preserves_key_endpoints_and_sums_beyond_bigint() {
    let groups = group_int8(
        [
            (Some(i64::MAX), Some(i64::MIN)),
            (None, None),
            (Some(i64::MIN), Some(i64::MAX)),
            (Some(i64::MAX), Some(i64::MIN)),
            (None, Some(0)),
            (Some(i64::MIN), Some(i64::MAX)),
            (Some(0), None),
        ],
        4,
        || true,
    )
    .unwrap();
    let actual: Vec<_> = groups
        .iter()
        .map(|g| {
            let s = g.summary;
            (g.key, s.rows(), s.non_nulls(), s.sum(), s.min(), s.max())
        })
        .collect();
    assert_eq!(
        actual,
        vec![
            (None, 2, 1, Some(0), Some(0), Some(0)),
            (
                Some(i64::MIN),
                2,
                2,
                Some(2 * i128::from(i64::MAX)),
                Some(i64::MAX),
                Some(i64::MAX)
            ),
            (Some(0), 1, 0, None, None, None),
            (
                Some(i64::MAX),
                2,
                2,
                Some(2 * i128::from(i64::MIN)),
                Some(i64::MIN),
                Some(i64::MIN)
            ),
        ]
    );
}

#[test]
fn wide_grouping_enforces_limits_and_cancellation() {
    assert_eq!(
        group_int8([(None, None), (Some(i64::MIN), Some(1))], 1, || true),
        Err(GroupingError::GroupLimitExceeded { limit: 1 })
    );
    assert_eq!(group_int8([], 0, || true), Err(GroupingError::InvalidLimit));
    assert_eq!(group_int8([], 1, || false), Err(GroupingError::Cancelled));
}

#[test]
fn wide_grouping_matches_independent_postgresql_numeric_sums_in_both_orders() {
    fn sql_value(value: Option<impl std::fmt::Display>) -> String {
        value.map_or_else(|| "NULL".to_owned(), |v| v.to_string())
    }
    for reverse in [false, true] {
        let input = (0..5000).map(|index| {
            let n = if reverse { 4999 - index } else { index };
            (
                (n % 17 != 0).then_some(i64::from(n % 13 - 6) + 5_000_000_000),
                (n % 7 != 0).then_some(
                    i64::from((n * 7919) % 100003 - 50001) * 1_000_000_000_000
                        + 3_000_000_000_000_000_000,
                ),
            )
        });
        let groups = group_int8(input, 14, || true).unwrap();
        assert!(groups.iter().all(|g| g
            .summary
            .sum()
            .is_some_and(|sum| sum > i128::from(i64::MAX))));
        let actual = groups
            .iter()
            .map(|g| {
                let s = g.summary;
                format!(
                    "{}|{}|{}|{}|{}|{}\n",
                    sql_value(g.key),
                    s.rows(),
                    s.non_nulls(),
                    sql_value(s.sum()),
                    sql_value(s.min()),
                    sql_value(s.max()),
                )
            })
            .collect::<String>();
        assert_eq!(
            actual,
            include_str!("fixtures/grouped_int8.txt"),
            "reverse={reverse}"
        );
    }
}
