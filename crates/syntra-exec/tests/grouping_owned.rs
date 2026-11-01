use syntra_exec::{group_owned_int8, GroupingError};

#[test]
fn owned_text_keys_preserve_bytes_nulls_and_wide_sums() {
    let input = [
        (Some("é".to_owned()), Some(i64::MAX)),
        (None, None),
        (Some("é".to_owned()), Some(i64::MAX)),
        (Some("e\u{301}".to_owned()), Some(-3)),
        (Some(String::new()), None),
        (None, Some(7)),
    ];
    let groups = group_owned_int8(input, 4, 5, String::len, || true).unwrap();
    assert_eq!(
        groups.iter().map(|g| g.key.as_deref()).collect::<Vec<_>>(),
        vec![None, Some(""), Some("e\u{301}"), Some("é")]
    );
    assert_eq!(
        (
            groups[0].summary.rows(),
            groups[0].summary.non_nulls(),
            groups[0].summary.sum()
        ),
        (2, 1, Some(7))
    );
    assert_eq!(groups[3].summary.sum(), Some(i128::from(i64::MAX) * 2));
}

#[test]
fn key_budget_counts_distinct_bytes_and_admits_repeats_at_the_limit() {
    let result = group_owned_int8(
        [
            (Some("abc".to_owned()), Some(1)),
            (Some("abc".to_owned()), Some(2)),
        ],
        1,
        3,
        String::len,
        || true,
    )
    .unwrap();
    assert_eq!(
        (
            result.len(),
            result[0].summary.rows(),
            result[0].summary.sum()
        ),
        (1, 2, Some(3))
    );
    let result = group_owned_int8(
        [
            (Some("abc".to_owned()), Some(1)),
            (Some("x".to_owned()), Some(2)),
        ],
        2,
        3,
        String::len,
        || true,
    );
    assert_eq!(
        result,
        Err(GroupingError::KeyBytesLimitExceeded { limit: 3 })
    );
}

#[test]
fn byte_accounting_overflow_and_invalid_limits_are_errors() {
    assert_eq!(
        group_owned_int8::<String>([], 1, 0, String::len, || true),
        Err(GroupingError::InvalidKeyLimit)
    );
    assert_eq!(
        group_owned_int8::<String>([], 0, 8, String::len, || true),
        Err(GroupingError::InvalidLimit)
    );
    assert_eq!(
        group_owned_int8([(Some(1), Some(1))], 1, 8, |_| usize::MAX, || true),
        Err(GroupingError::KeyBytesLimitExceeded { limit: 8 })
    );
}

#[test]
fn uuid_bytes_and_null_groups_obey_group_and_byte_limits() {
    let key = [255u8; 16];
    let groups = group_owned_int8(
        [(Some(key), Some(2)), (None, None), (Some(key), Some(3))],
        2,
        16,
        |_| 16,
        || true,
    )
    .unwrap();
    assert_eq!(
        groups
            .iter()
            .map(|g| (g.key, g.summary.rows(), g.summary.sum()))
            .collect::<Vec<_>>(),
        vec![(None, 1, None), (Some(key), 2, Some(5))]
    );
    assert_eq!(
        group_owned_int8([(Some(key), Some(2)), (None, None)], 1, 16, |_| 16, || true),
        Err(GroupingError::GroupLimitExceeded { limit: 1 })
    );
}

#[test]
fn cancellation_checks_large_repeated_keys_before_emitting_any_output() {
    let mut checks = 0;
    let result = group_owned_int8(
        (0..4).map(|_| (Some("same".to_owned()), Some(1))),
        1,
        4,
        String::len,
        || {
            checks += 1;
            checks < 4
        },
    );
    assert_eq!(result, Err(GroupingError::Cancelled));
    assert_eq!(checks, 4);
}

#[test]
fn empty_input_has_no_null_group() {
    let result = group_owned_int8::<String>([], 1, 1, String::len, || true).unwrap();
    assert_eq!(result, Vec::new());
}
