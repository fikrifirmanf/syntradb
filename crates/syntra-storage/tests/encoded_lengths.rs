use syntra_storage::{
    encode_int4, encoded_int4_len,
    replay::{Binding, Limits, ReplayState, Row},
    SegmentError, MAX_ROWS,
};

const LIMITS: Limits = Limits {
    rows: 1_000_000,
    changes: 4096,
    buffer_bytes: 32 * 1024 * 1024,
};
const BINDING: Binding = Binding {
    stream: [3; 32],
    epoch: 1,
};

// Two value patterns per count: all NULL and mostly non-NULL. Neither may change
// the predicted length, which callers use to reject stored bytes before reading.
fn rows(count: usize, nulls: bool) -> Vec<Row> {
    (0..count)
        .map(|index| Row {
            key: index as i32,
            value: if nulls || index % 7 == 0 {
                None
            } else {
                Some(index as i32 - 50)
            },
        })
        .collect()
}

#[test]
fn predicted_lengths_match_encoders_at_group_and_bitmap_boundaries() {
    for count in [0, 1, 7, 8, 9, 1023, 1024, 1025, 2048, 2049, 65_535] {
        for nulls in [false, true] {
            let rows = rows(count, nulls);
            let segment = encode_int4(rows.iter().map(|r| r.value), || true).unwrap();
            assert_eq!(
                encoded_int4_len(count),
                Ok(segment.len()),
                "segment {count}"
            );
            let state = <ReplayState>::seed(BINDING, 0, &rows, LIMITS, || true).unwrap();
            let encoded = state.encode_checkpoint(|| true).unwrap();
            assert_eq!(
                <ReplayState>::encoded_checkpoint_len(count),
                Ok(encoded.len()),
                "state {count}"
            );
        }
    }
}

#[test]
fn predicted_lengths_accept_the_row_limit_and_reject_one_more() {
    let rows = rows(MAX_ROWS, false);
    let segment = encode_int4(rows.iter().map(|r| r.value), || true).unwrap();
    assert_eq!(encoded_int4_len(MAX_ROWS), Ok(segment.len()));
    let state = <ReplayState>::seed(BINDING, 0, &rows, LIMITS, || true).unwrap();
    assert_eq!(
        <ReplayState>::encoded_checkpoint_len(MAX_ROWS),
        Ok(state.encode_checkpoint(|| true).unwrap().len())
    );
    assert_eq!(
        encoded_int4_len(MAX_ROWS + 1),
        Err(SegmentError::LimitExceeded)
    );
    assert_eq!(
        <ReplayState>::encoded_checkpoint_len(MAX_ROWS + 1),
        Err(SegmentError::LimitExceeded)
    );
}
