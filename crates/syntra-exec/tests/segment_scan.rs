use syntra_exec::{summarize_segment, ExecutionError, Int4Predicate};
use syntra_storage::{encode_int4, SegmentError, GROUP_ROWS};

#[test]
fn scans_all_groups_and_counts_nulls_correctly() {
    let bytes = encode_int4([Some(-5), None, Some(7), Some(0)], || true).unwrap();
    let result = summarize_segment(&bytes, Int4Predicate::All, || true).unwrap();
    let s = result.summary;
    assert_eq!(
        (s.rows(), s.non_nulls(), s.sum(), s.min(), s.max()),
        (4, 3, Some(2), Some(-5), Some(7))
    );
    assert_eq!(
        (
            result.stats.groups_scanned,
            result.stats.groups_pruned,
            result.stats.rows_examined
        ),
        (1, 0, 4)
    );
    assert_eq!(result.stats.scan_bytes as usize, bytes.len() - 40);
    assert_eq!(result.stats.validation_bytes as usize, bytes.len());
}

#[test]
fn prunes_nonmatching_groups_and_keeps_inclusive_range_edges() {
    let bytes = encode_int4((0..3072).map(Some), || true).unwrap();
    let result = summarize_segment(
        &bytes,
        Int4Predicate::Range {
            lower: 1100,
            upper: 1200,
        },
        || true,
    )
    .unwrap();
    let s = result.summary;
    assert_eq!(
        (s.rows(), s.non_nulls(), s.sum(), s.min(), s.max()),
        (101, 101, Some(116_150), Some(1100), Some(1200))
    );
    assert_eq!(
        (
            result.stats.groups_scanned,
            result.stats.groups_pruned,
            result.stats.rows_examined
        ),
        (1, 2, 1024)
    );
    assert_eq!(result.stats.scan_bytes, 4248);
    assert_eq!(result.stats.validation_bytes as usize, bytes.len());
}

#[test]
fn null_predicates_prune_only_eligible_groups() {
    let values =
        std::iter::repeat_n(Some(5), GROUP_ROWS).chain(std::iter::repeat_n(None, GROUP_ROWS));
    let bytes = encode_int4(values, || true).unwrap();
    let nulls = summarize_segment(&bytes, Int4Predicate::IsNull, || true).unwrap();
    assert_eq!(
        (
            nulls.summary.rows(),
            nulls.summary.non_nulls(),
            nulls.summary.sum()
        ),
        (1024, 0, None)
    );
    assert_eq!(
        (nulls.stats.groups_scanned, nulls.stats.groups_pruned),
        (1, 1)
    );
    let values = summarize_segment(&bytes, Int4Predicate::IsNotNull, || true).unwrap();
    assert_eq!(
        (values.summary.rows(), values.summary.sum()),
        (1024, Some(5120))
    );
    assert_eq!(
        (values.stats.groups_scanned, values.stats.groups_pruned),
        (1, 1)
    );
}

#[test]
fn empty_or_inverted_range_returns_sql_empty_aggregates() {
    let bytes = encode_int4([Some(1), None, Some(2)], || true).unwrap();
    for predicate in [
        Int4Predicate::Range { lower: 2, upper: 1 },
        Int4Predicate::Range { lower: 5, upper: 6 },
    ] {
        let result = summarize_segment(&bytes, predicate, || true).unwrap();
        assert_eq!(
            (
                result.summary.rows(),
                result.summary.sum(),
                result.summary.min(),
                result.summary.max()
            ),
            (0, None, None, None)
        );
        assert_eq!(
            (
                result.stats.groups_scanned,
                result.stats.groups_pruned,
                result.stats.scan_bytes
            ),
            (0, 1, 0)
        );
    }
    let empty = encode_int4([], || true).unwrap();
    let result = summarize_segment(&empty, Int4Predicate::All, || true).unwrap();
    assert_eq!(
        (
            result.summary.rows(),
            result.summary.sum(),
            result.stats.validation_bytes
        ),
        (0, None, 40)
    );
}

#[test]
fn malformed_data_errors_even_when_every_group_would_be_pruned() {
    let mut bytes = encode_int4([Some(1), Some(2)], || true).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    assert_eq!(
        summarize_segment(
            &bytes,
            Int4Predicate::Range {
                lower: 10,
                upper: 20
            },
            || true
        ),
        Err(ExecutionError::Segment(SegmentError::ChecksumMismatch))
    );
}

#[test]
fn cancellation_during_scan_returns_no_partial_result() {
    let bytes = encode_int4(std::iter::repeat_n(Some(1), GROUP_ROWS * 3), || true).unwrap();
    let mut checks = 0;
    let result = summarize_segment(&bytes, Int4Predicate::All, || {
        checks += 1;
        checks < 6
    });
    assert_eq!(result, Err(ExecutionError::Cancelled));
}

#[test]
fn generated_nullable_inputs_match_a_scalar_reference() {
    for len in [0, 1, 7, 8, 1023, 1024, 1025, 5000] {
        let input: Vec<_> = (0..len)
            .map(|n| if n % 5 == 0 { None } else { Some(n - 1000) })
            .collect();
        let expected: Vec<i32> = input
            .iter()
            .flatten()
            .copied()
            .filter(|n| (-123..=456).contains(n))
            .collect();
        let bytes = encode_int4(input, || true).unwrap();
        let result = summarize_segment(
            &bytes,
            Int4Predicate::Range {
                lower: -123,
                upper: 456,
            },
            || true,
        )
        .unwrap();
        assert_eq!(result.summary.rows(), expected.len() as i64);
        assert_eq!(result.summary.min(), expected.iter().copied().min());
        assert_eq!(result.summary.max(), expected.iter().copied().max());
        assert_eq!(
            result.summary.sum(),
            (!expected.is_empty()).then(|| expected.iter().map(|n| i64::from(*n)).sum())
        );
    }
}
