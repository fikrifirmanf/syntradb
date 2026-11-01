use syntra_exec::{group_owned_decimal, DecimalResult::*, DecimalSummary, GroupingError};

#[test]
fn decimal_nan_is_nonnull_and_greater_than_every_finite_value() {
    let mut summary = DecimalSummary::default();
    for v in [Null, NaN, Finite(-123), Finite(456), NaN] {
        summary.push(v).unwrap();
    }
    assert_eq!((summary.rows(), summary.non_nulls()), (5, 4));
    assert_eq!(
        (summary.sum(), summary.min(), summary.max()),
        (NaN, Finite(-123), NaN)
    );
}

#[test]
fn decimal_all_null_all_nan_and_empty_are_distinct() {
    let groups = group_owned_decimal(
        [(None, Null), (None, Null), (Some(1), NaN), (Some(1), Null)],
        2,
        8,
        |_| 8,
        || true,
    )
    .unwrap();
    assert_eq!(
        (groups[0].summary.rows(), groups[0].summary.non_nulls()),
        (2, 0)
    );
    assert_eq!(
        (
            groups[0].summary.sum(),
            groups[0].summary.min(),
            groups[0].summary.max()
        ),
        (Null, Null, Null)
    );
    assert_eq!(
        (groups[1].summary.rows(), groups[1].summary.non_nulls()),
        (2, 1)
    );
    assert_eq!(
        (
            groups[1].summary.sum(),
            groups[1].summary.min(),
            groups[1].summary.max()
        ),
        (NaN, NaN, NaN)
    );
    assert_eq!(
        group_owned_decimal::<i64>([], 1, 8, |_| 8, || true).unwrap(),
        vec![]
    );
}

#[test]
fn decimal_scaled_units_carry_and_sum_widens_without_float_rounding() {
    let groups = group_owned_decimal(
        [
            (Some(1), Finite(99)),
            (Some(1), Finite(2)),
            (Some(2), Finite(i64::MAX)),
            (Some(2), Finite(i64::MAX)),
            (Some(3), Finite(i64::MIN)),
            (Some(3), Finite(i64::MIN)),
        ],
        3,
        24,
        |_| 8,
        || true,
    )
    .unwrap();
    assert_eq!(
        groups.iter().map(|g| g.summary.sum()).collect::<Vec<_>>(),
        vec![
            Finite(101),
            Finite(i128::from(i64::MAX) * 2),
            Finite(i128::from(i64::MIN) * 2)
        ]
    );
    assert_eq!(
        (groups[0].summary.min(), groups[0].summary.max()),
        (Finite(2), Finite(99))
    );
}

#[test]
fn decimal_limits_reject_whole_result_and_repeated_keys_cost_no_extra_bytes() {
    let rows = [
        (Some("é".to_owned()), Finite(1)),
        (Some("é".to_owned()), Finite(2)),
        (None, NaN),
    ];
    let result = group_owned_decimal(rows.clone(), 2, 2, String::len, || true).unwrap();
    assert_eq!(result[1].summary.sum(), Finite(3));
    assert_eq!(
        group_owned_decimal(rows.clone(), 1, 2, String::len, || true),
        Err(GroupingError::GroupLimitExceeded { limit: 1 })
    );
    assert_eq!(
        group_owned_decimal(rows, 2, 1, String::len, || true),
        Err(GroupingError::KeyBytesLimitExceeded { limit: 1 })
    );
    assert_eq!(
        group_owned_decimal::<i64>([], 0, 8, |_| 8, || true),
        Err(GroupingError::InvalidLimit)
    );
    assert_eq!(
        group_owned_decimal::<i64>([], 1, 0, |_| 8, || true),
        Err(GroupingError::InvalidKeyLimit)
    );
}

#[test]
fn decimal_cancellation_discards_accumulated_groups() {
    let mut polls = 0;
    let result = group_owned_decimal(
        (0..10).map(|n| (Some(n), Finite(n))),
        10,
        80,
        |_| 8,
        || {
            polls += 1;
            polls < 5
        },
    );
    assert_eq!(result, Err(GroupingError::Cancelled));
    assert_eq!(polls, 5);
}
