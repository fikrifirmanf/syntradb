use syntra_storage::{
    chunks::{partition, partition_of, PARTITIONS},
    encode, encoded_len,
    replay::{Applied, Batch, Binding, Change, Limits, ReplayError, ReplayState, Row},
    Int4Segment, Segment, SegmentError,
};

// Generated independently by build-support/fixtures/gen_int8_fixtures.py.
const SEGMENT: &[u8] = include_bytes!("fixtures/int8-v1.bin");
const STATE: &[u8] = include_bytes!("fixtures/keyed_checkpoint_v2_int8.bin");
const MIXED_STATE: &[u8] = include_bytes!("fixtures/keyed_checkpoint_v2_int4_int8.bin");
const ROUTING: &str = include_str!("fixtures/chunk_routing_int8.txt");

const FIXTURE_VALUES: [Option<i64>; 7] = [
    None,
    Some(i64::MIN),
    Some(0),
    Some(i64::MAX),
    Some(5_000_000_000),
    None,
    Some(-1),
];

fn limits() -> Limits {
    Limits {
        rows: 1_000_000,
        changes: 4096,
        buffer_bytes: 32 * 1024 * 1024,
    }
}
fn binding() -> Binding {
    Binding {
        stream: [7; 32],
        epoch: 9,
    }
}
fn decode(bytes: &[u8]) -> Vec<Option<i64>> {
    Segment::<i64>::open(bytes, || true)
        .unwrap()
        .groups()
        .flat_map(|group| group.unwrap().values().collect::<Vec<_>>())
        .collect()
}
fn reseal_header(bytes: &mut [u8]) {
    let crc = crc32fast::hash(&bytes[..36]);
    bytes[36..40].copy_from_slice(&crc.to_le_bytes());
}

#[test]
fn int8_segment_matches_the_python_fixture_and_round_trips() {
    let bytes = encode(FIXTURE_VALUES, || true).unwrap();
    assert_eq!(bytes, SEGMENT);
    assert_eq!(decode(SEGMENT), FIXTURE_VALUES);
    let segment = Segment::<i64>::open(SEGMENT, || true).unwrap();
    let group = segment.groups().next().unwrap().unwrap();
    assert_eq!(
        (group.rows(), group.nulls(), group.min(), group.max()),
        (7, 2, Some(i64::MIN), Some(i64::MAX))
    );
    assert_eq!(encoded_len::<i64>(7), Ok(SEGMENT.len()));
}

#[test]
fn int8_segments_span_groups_and_report_exact_lengths() {
    for rows in [0, 1, 1023, 1024, 1025, 4097] {
        let values: Vec<Option<i64>> = (0..rows)
            .map(|i| (i % 5 != 0).then_some((i as i64 - 2000) * 3_000_000_000))
            .collect();
        let bytes = encode(values.iter().copied(), || true).unwrap();
        assert_eq!(bytes.len(), encoded_len::<i64>(rows).unwrap(), "{rows}");
        assert_eq!(decode(&bytes), values, "{rows}");
    }
}

#[test]
fn readers_refuse_a_segment_of_the_other_width() {
    assert_eq!(
        Int4Segment::open(SEGMENT, || true).err(),
        Some(SegmentError::InvalidHeader)
    );
    let int4 = syntra_storage::encode_int4([Some(1), None], || true).unwrap();
    assert_eq!(
        Segment::<i64>::open(&int4, || true).err(),
        Some(SegmentError::InvalidHeader)
    );
}

#[test]
fn int8_segment_corruption_is_detected() {
    // Group zero starts at 40: rows, nulls, min (8), max (8), bitmap length, CRC.
    let mut wrong_min = SEGMENT.to_vec();
    wrong_min[48] ^= 1;
    assert_eq!(
        Segment::<i64>::open(&wrong_min, || true).err(),
        Some(SegmentError::ChecksumMismatch)
    );
    let mut truncated = SEGMENT[..SEGMENT.len() - 8].to_vec();
    let len = truncated.len() as u32;
    truncated[32..36].copy_from_slice(&len.to_le_bytes());
    reseal_header(&mut truncated);
    assert_eq!(
        Segment::<i64>::open(&truncated, || true).err(),
        Some(SegmentError::InvalidMetadata)
    );
}

#[test]
fn int8_state_matches_the_python_fixture_and_restores() {
    let rows = [
        Row {
            key: -3_i64,
            value: None,
        },
        Row {
            key: 6_000_000_000,
            value: Some(-7_000_000_000_i64),
        },
    ];
    let state = ReplayState::seed(binding(), 0, &rows, limits(), || true).unwrap();
    assert_eq!(state.encode_checkpoint(|| true).unwrap(), STATE);
    let restored = ReplayState::<i64, i64>::decode_checkpoint(STATE, limits(), || true).unwrap();
    assert_eq!(restored.rows(), rows);
    assert_eq!(
        ReplayState::<i64, i64>::encoded_checkpoint_len(2),
        Ok(STATE.len())
    );
}

#[test]
fn mixed_width_state_matches_its_fixture() {
    let rows = [
        Row {
            key: -3_i32,
            value: None,
        },
        Row {
            key: 5,
            value: Some(-7_000_000_000_i64),
        },
    ];
    let state = ReplayState::seed(binding(), 0, &rows, limits(), || true).unwrap();
    assert_eq!(state.encode_checkpoint(|| true).unwrap(), MIXED_STATE);
    assert_eq!(
        ReplayState::<i32, i64>::decode_checkpoint(MIXED_STATE, limits(), || true)
            .unwrap()
            .rows(),
        rows
    );
}

#[test]
fn a_state_is_only_readable_under_its_own_layout() {
    assert_eq!(
        <ReplayState>::decode_checkpoint(STATE, limits(), || true).err(),
        Some(SegmentError::InvalidHeader)
    );
    assert_eq!(
        ReplayState::<i32, i64>::decode_checkpoint(STATE, limits(), || true).err(),
        Some(SegmentError::InvalidHeader)
    );
    assert_eq!(
        ReplayState::<i64, i64>::decode_checkpoint(MIXED_STATE, limits(), || true).err(),
        Some(SegmentError::InvalidHeader)
    );
    let int4 = <ReplayState>::seed(binding(), 0, &[], limits(), || true)
        .unwrap()
        .encode_checkpoint(|| true)
        .unwrap();
    assert_eq!(
        ReplayState::<i64, i64>::decode_checkpoint(&int4, limits(), || true).err(),
        Some(SegmentError::InvalidHeader)
    );
}

#[test]
fn width_bytes_and_reserved_bytes_must_be_canonical() {
    let repair = |bytes: &mut Vec<u8>| {
        let mut crc = crc32fast::Hasher::new();
        crc.update(&bytes[..104]);
        crc.update(&bytes[108..]);
        bytes[104..108].copy_from_slice(&crc.finalize().to_le_bytes());
    };
    let mut reserved = STATE.to_vec();
    reserved[110] = 1;
    repair(&mut reserved);
    assert_eq!(
        ReplayState::<i64, i64>::decode_checkpoint(&reserved, limits(), || true).err(),
        Some(SegmentError::NonCanonical)
    );
    let mut narrowed = STATE.to_vec();
    narrowed[109] = 4;
    repair(&mut narrowed);
    assert_eq!(
        ReplayState::<i64, i64>::decode_checkpoint(&narrowed, limits(), || true).err(),
        Some(SegmentError::InvalidHeader)
    );
}

#[test]
fn int8_keys_beyond_int4_replay_and_retry_exactly() {
    let big = 3_000_000_000_i64;
    let mut state = ReplayState::seed(
        binding(),
        0,
        &[Row {
            key: big,
            value: Some(1_i64),
        }],
        limits(),
        || true,
    )
    .unwrap();
    let changes = [
        Change {
            key: big,
            before: Some(Some(1)),
            after: Some(Some(i64::MAX)),
        },
        Change {
            key: -big,
            before: None,
            after: Some(None),
        },
    ];
    let batch = || Batch {
        binding: binding(),
        previous: 0,
        next: 2,
        changes: &changes,
    };
    assert_eq!(state.apply_events(batch(), || true), Ok(Applied::New));
    assert_eq!(
        state.rows(),
        [
            Row {
                key: -big,
                value: None
            },
            Row {
                key: big,
                value: Some(i64::MAX)
            }
        ]
    );
    let restored = ReplayState::<i64, i64>::decode_checkpoint(
        &state.encode_checkpoint(|| true).unwrap(),
        limits(),
        || true,
    )
    .unwrap();
    let mut restored = restored;
    assert_eq!(
        restored.apply_events(batch(), || true),
        Ok(Applied::Duplicate)
    );
    let stale = [Change {
        key: big,
        before: Some(Some(1)),
        after: None,
    }];
    assert_eq!(
        restored.apply_events(
            Batch {
                binding: binding(),
                previous: 2,
                next: 3,
                changes: &stale,
            },
            || true
        ),
        Err(ReplayError::BeforeImageMismatch(big))
    );
}

#[test]
fn int8_routing_matches_the_python_fixture_and_spreads_keys() {
    for line in ROUTING.lines() {
        let (key, bucket) = line.split_once(' ').unwrap();
        assert_eq!(
            partition_of(key.parse::<i64>().unwrap()),
            bucket.parse::<usize>().unwrap(),
            "{key}"
        );
    }
    let mut counts = [0; PARTITIONS];
    for key in 0..100_000_i64 {
        counts[partition_of(key * 4_294_967_311)] += 1;
    }
    assert!(
        counts.iter().all(|n| (1300..1900).contains(n)),
        "{counts:?}"
    );
    // int4 keys keep the frozen v1 routing through the generic entry point.
    for key in [i32::MIN, -1, 0, 1, 7, i32::MAX] {
        assert_eq!(partition_of(key), partition(key));
    }
}
