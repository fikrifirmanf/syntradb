use syntra_exec::{summarize_int4_where, ExecutionError, Int4Predicate, INTERRUPT_INTERVAL};

#[test]
fn streaming_predicates_preserve_null_and_inclusive_range_semantics() {
    let input = [
        None,
        Some(i32::MIN),
        Some(-1),
        Some(0),
        None,
        Some(7),
        Some(i32::MAX),
    ];
    for (predicate, expected) in [
        (Int4Predicate::IsNull, (2, 0, None, None, None)),
        (
            Int4Predicate::IsNotNull,
            (5, 5, Some(5), Some(i32::MIN), Some(i32::MAX)),
        ),
        (
            Int4Predicate::Range {
                lower: -1,
                upper: 7,
            },
            (3, 3, Some(6), Some(-1), Some(7)),
        ),
        (
            Int4Predicate::Range {
                lower: 7,
                upper: -1,
            },
            (0, 0, None, None, None),
        ),
    ] {
        let s = summarize_int4_where(input, predicate, || true).unwrap();
        assert_eq!(
            (s.rows(), s.non_nulls(), s.sum(), s.min(), s.max()),
            expected
        );
    }
}

#[test]
fn rejected_rows_still_poll_and_cancel_a_long_stream() {
    let mut consumed = 0;
    let mut polls = 0;
    let result = summarize_int4_where(
        (0..10000).map(|_| {
            consumed += 1;
            Some(1)
        }),
        Int4Predicate::Range { lower: 2, upper: 3 },
        || {
            polls += 1;
            polls < 2
        },
    );
    assert_eq!(result, Err(ExecutionError::Cancelled));
    assert_eq!(consumed, INTERRUPT_INTERVAL + 1);
    assert_eq!(polls, 2);
}

#[test]
fn filtered_empty_input_honors_cancellation() {
    assert_eq!(
        summarize_int4_where([], Int4Predicate::IsNull, || false),
        Err(ExecutionError::Cancelled)
    );
}
