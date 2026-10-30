use syntra_exec::{group_int4, GroupingError, INTERRUPT_INTERVAL, MAX_GROUPS};

#[test]
fn groups_nullable_keys_and_values_with_postgresql_aggregate_semantics() {
    let groups = group_int4(
        [
            (Some(2), Some(i32::MAX)),
            (None, None),
            (Some(-1), None),
            (Some(2), Some(i32::MIN)),
            (None, Some(7)),
            (Some(2), None),
            (Some(-1), None),
        ],
        3,
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
            (None, 2, 1, Some(7), Some(7), Some(7)),
            (Some(-1), 2, 0, None, None, None),
            (Some(2), 3, 2, Some(-1), Some(i32::MIN), Some(i32::MAX)),
        ]
    );
}

#[test]
fn empty_input_has_no_groups_and_honors_cancellation() {
    assert_eq!(group_int4([], 1, || true), Ok(Vec::new()));
    assert_eq!(group_int4([], 1, || false), Err(GroupingError::Cancelled));
}

#[test]
fn invalid_limits_do_not_consume_input() {
    for limit in [0, MAX_GROUPS + 1, usize::MAX] {
        let input = std::iter::from_fn(|| panic!("invalid limit consumed input"));
        assert_eq!(
            group_int4(input, limit, || true),
            Err(GroupingError::InvalidLimit)
        );
    }
}

#[test]
fn new_key_past_limit_fails_without_consuming_the_remaining_stream() {
    let mut consumed = 0;
    let result = group_int4(
        (0..10000).map(|key| {
            consumed += 1;
            (Some(key), Some(1))
        }),
        2,
        || true,
    );
    assert_eq!(result, Err(GroupingError::GroupLimitExceeded { limit: 2 }));
    assert_eq!(consumed, 3);
}

#[test]
fn null_keys_share_one_admitted_group_and_repeated_keys_fit_at_the_cap() {
    let groups = group_int4([(None, Some(3)); 10000], 1, || true).unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].key, None);
    assert_eq!(groups[0].summary.rows(), 10000);
    assert_eq!(groups[0].summary.sum(), Some(30000));
    assert_eq!(
        group_int4([(None, None), (Some(0), None)], 1, || true),
        Err(GroupingError::GroupLimitExceeded { limit: 1 })
    );
}

#[test]
fn every_admitted_group_slot_is_usable() {
    let groups = group_int4(
        (0..MAX_GROUPS as i32).map(|key| (Some(key), Some(key))),
        MAX_GROUPS,
        || true,
    )
    .unwrap();
    assert_eq!(groups.len(), MAX_GROUPS);
    for (index, group) in groups.iter().enumerate() {
        assert_eq!(group.key, Some(index as i32));
        assert_eq!(group.summary.rows(), 1);
        assert_eq!(group.summary.non_nulls(), 1);
        assert_eq!(group.summary.sum(), Some(index as i64));
    }
}

#[test]
fn cancellation_stops_a_stream_of_one_repeated_key() {
    let mut consumed = 0;
    let mut polls = 0;
    let result = group_int4(
        (0..10000).map(|_| {
            consumed += 1;
            (Some(1), Some(2))
        }),
        1,
        || {
            polls += 1;
            polls < 2
        },
    );
    assert_eq!(result, Err(GroupingError::Cancelled));
    assert_eq!(consumed, INTERRUPT_INTERVAL + 1);
}

#[test]
fn cancellation_at_completion_returns_no_partial_groups() {
    let mut polls = 0;
    assert_eq!(
        group_int4([(Some(2), Some(9))], 1, || {
            polls += 1;
            polls < 2
        }),
        Err(GroupingError::Cancelled)
    );
}

#[test]
fn cancellation_after_output_sort_returns_no_partial_groups() {
    let mut polls = 0;
    assert_eq!(
        group_int4([(Some(2), Some(9))], 1, || {
            polls += 1;
            polls < 3
        }),
        Err(GroupingError::Cancelled)
    );
    assert_eq!(polls, 3);
}

#[test]
fn grouping_matches_independent_postgresql_results_in_both_input_orders() {
    fn sql_value(value: Option<impl std::fmt::Display>) -> String {
        value.map_or_else(|| "NULL".to_owned(), |v| v.to_string())
    }
    for reverse in [false, true] {
        // The SQL fixture uses these arithmetic input expressions, but its
        // expected aggregates were computed by PostgreSQL 18.6, not this kernel.
        let input = (0..5000).map(|index| {
            let n = if reverse { 4999 - index } else { index };
            (
                (n % 17 != 0).then_some(n % 13 - 6),
                (n % 7 != 0).then_some((n * 7919) % 100003 - 50001),
            )
        });
        let groups = group_int4(input, 14, || true).unwrap();
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
            include_str!("fixtures/grouped_int4.txt"),
            "reverse={reverse}"
        );
    }
}
