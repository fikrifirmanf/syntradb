use syntra_storage::{encode_int4, Int4Segment, SegmentError, GROUP_ROWS, MAX_ROWS};

#[test]
fn counted_writer_matches_independent_counts_and_format() {
    for length in [0, 1, GROUP_ROWS - 1, GROUP_ROWS, GROUP_ROWS + 1, MAX_ROWS] {
        let input = (0..length).map(|n| (n % 3 != 0).then_some(n as i32));
        let (bytes, rows) = syntra_storage::encode_int4_with_row_count(input, || true).unwrap();
        assert_eq!(rows as usize, length);
        let segment = Int4Segment::open(&bytes, || true).unwrap();
        assert_eq!(segment.rows() as usize, length);
        assert_eq!(decode(&bytes).len(), length);
    }
    assert_eq!(
        syntra_storage::encode_int4_with_row_count([Some(i32::MIN), None, Some(7)], || true)
            .unwrap(),
        (GOLDEN.to_vec(), 3)
    );
}

#[test]
fn counted_writer_returns_no_output_after_limit_or_cancellation() {
    assert_eq!(
        syntra_storage::encode_int4_with_row_count(std::iter::repeat_n(None, MAX_ROWS + 1), || {
            true
        }),
        Err(SegmentError::LimitExceeded)
    );
    let mut polls = 0;
    assert_eq!(
        syntra_storage::encode_int4_with_row_count([Some(1)], || {
            polls += 1;
            polls < 3
        }),
        Err(SegmentError::Cancelled)
    );
    assert_eq!(polls, 3);
}

// Generated independently with Python struct.pack and zlib.crc32.
const GOLDEN: &[u8] = include_bytes!("fixtures/int4-v1.bin");

fn decode(bytes: &[u8]) -> Vec<Option<i32>> {
    Int4Segment::open(bytes, || true)
        .unwrap()
        .groups()
        .flat_map(|group| group.unwrap().values().collect::<Vec<_>>())
        .collect()
}

fn seal_header(bytes: &mut [u8]) {
    let crc = crc32fast::hash(&bytes[..36]);
    bytes[36..40].copy_from_slice(&crc.to_le_bytes());
}

fn seal_first_group(bytes: &mut [u8]) {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&bytes[40..60]);
    hasher.update(&bytes[64..]);
    bytes[60..64].copy_from_slice(&hasher.finalize().to_le_bytes());
}

#[test]
fn writer_matches_independent_binary_fixture() {
    assert_eq!(
        encode_int4([Some(i32::MIN), None, Some(7)], || true).unwrap(),
        GOLDEN
    );
}

#[test]
fn reader_decodes_independent_binary_fixture() {
    assert_eq!(decode(GOLDEN), [Some(i32::MIN), None, Some(7)]);
    let segment = Int4Segment::open(GOLDEN, || true).unwrap();
    assert_eq!(
        (segment.rows(), segment.group_count(), segment.byte_len()),
        (3, 1, 77)
    );
    let group = segment.groups().next().unwrap().unwrap();
    assert_eq!(
        (group.rows(), group.nulls(), group.min(), group.max()),
        (3, 1, Some(i32::MIN), Some(7))
    );
}

#[test]
fn empty_segment_has_no_groups() {
    let bytes = encode_int4([], || true).unwrap();
    let segment = Int4Segment::open(&bytes, || true).unwrap();
    assert_eq!(
        (segment.rows(), segment.group_count(), bytes.len()),
        (0, 0, 40)
    );
    assert_eq!(segment.groups().count(), 0);
}

#[test]
fn roundtrips_bitmap_and_group_boundaries() {
    for len in [1, 7, 8, 9, 1023, 1024, 1025, 2051] {
        let values: Vec<_> = (0..len)
            .map(|n| if n % 7 == 0 { None } else { Some(n - 1000) })
            .collect();
        let bytes = encode_int4(values.iter().copied(), || true).unwrap();
        assert_eq!(decode(&bytes), values, "length {len}");
        assert_eq!(
            Int4Segment::open(&bytes, || true).unwrap().group_count() as usize,
            values.len().div_ceil(GROUP_ROWS)
        );
    }
}

#[test]
fn all_null_group_has_no_value_bounds() {
    let bytes = encode_int4([None; GROUP_ROWS + 1], || true).unwrap();
    let segment = Int4Segment::open(&bytes, || true).unwrap();
    let bounds: Vec<_> = segment
        .groups()
        .map(|g| {
            let g = g.unwrap();
            (g.rows(), g.nulls(), g.min(), g.max())
        })
        .collect();
    assert_eq!(bounds, [(1024, 1024, None, None), (1, 1, None, None)]);
}

#[test]
fn rejects_every_truncated_fixture_prefix() {
    for end in 0..GOLDEN.len() {
        assert!(
            Int4Segment::open(&GOLDEN[..end], || true).is_err(),
            "accepted prefix {end}"
        );
    }
}

#[test]
fn rejects_every_single_bit_corruption() {
    for index in 0..GOLDEN.len() {
        for bit in 0..8 {
            let mut bytes = GOLDEN.to_vec();
            bytes[index] ^= 1 << bit;
            assert!(
                Int4Segment::open(&bytes, || true).is_err(),
                "accepted byte {index} bit {bit}"
            );
        }
    }
}

#[test]
fn rejects_unknown_version_and_type() {
    let mut bytes = GOLDEN.to_vec();
    bytes[8..10].copy_from_slice(&2u16.to_le_bytes());
    assert_eq!(
        Int4Segment::open(&bytes, || true).unwrap_err(),
        SegmentError::UnsupportedVersion(2)
    );
    let mut bytes = GOLDEN.to_vec();
    bytes[10..12].copy_from_slice(&2u16.to_le_bytes());
    seal_header(&mut bytes);
    assert_eq!(
        Int4Segment::open(&bytes, || true).unwrap_err(),
        SegmentError::InvalidHeader
    );
}

#[test]
fn rejects_forged_zone_maps_even_with_valid_checksums() {
    let mut bytes = GOLDEN.to_vec();
    bytes[48..52].copy_from_slice(&0i32.to_le_bytes());
    seal_first_group(&mut bytes);
    assert_eq!(
        Int4Segment::open(&bytes, || true).unwrap_err(),
        SegmentError::InvalidMetadata
    );
}

#[test]
fn rejects_false_null_counts_and_group_lengths() {
    for (offset, value) in [(44, 0u32), (40, 1025), (56, u32::MAX)] {
        let mut bytes = GOLDEN.to_vec();
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        seal_first_group(&mut bytes);
        assert_eq!(
            Int4Segment::open(&bytes, || true).unwrap_err(),
            SegmentError::InvalidMetadata
        );
    }
}

#[test]
fn rejects_nonzero_null_slots_and_bitmap_padding() {
    let mut bytes = GOLDEN.to_vec();
    bytes[69] = 1; // NULL slot's otherwise-unused payload must be zero.
    seal_first_group(&mut bytes);
    assert_eq!(
        Int4Segment::open(&bytes, || true).unwrap_err(),
        SegmentError::NonCanonical
    );
    let mut bytes = GOLDEN.to_vec();
    bytes[64] |= 0x80;
    seal_first_group(&mut bytes);
    assert_eq!(
        Int4Segment::open(&bytes, || true).unwrap_err(),
        SegmentError::NonCanonical
    );
}

#[test]
fn rejects_impossible_counts_without_allocating_from_them() {
    let mut bytes = GOLDEN.to_vec();
    bytes[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
    seal_header(&mut bytes);
    assert_eq!(
        Int4Segment::open(&bytes, || true).unwrap_err(),
        SegmentError::LimitExceeded
    );
    let mut bytes = GOLDEN.to_vec();
    bytes[24..28].copy_from_slice(&u32::MAX.to_le_bytes());
    seal_header(&mut bytes);
    assert_eq!(
        Int4Segment::open(&bytes, || true).unwrap_err(),
        SegmentError::InvalidMetadata
    );
}

#[test]
fn rejects_trailing_bytes() {
    let mut bytes = GOLDEN.to_vec();
    bytes.push(0);
    assert_eq!(
        Int4Segment::open(&bytes, || true).unwrap_err(),
        SegmentError::InvalidMetadata
    );
}

#[test]
fn enforces_writer_row_limit() {
    assert_eq!(
        encode_int4(std::iter::repeat_n(Some(1), MAX_ROWS + 1), || true),
        Err(SegmentError::LimitExceeded)
    );
}

#[test]
fn writer_and_reader_cancel_between_groups() {
    let mut checks = 0;
    assert_eq!(
        encode_int4(std::iter::repeat_n(Some(1), GROUP_ROWS * 3), || {
            checks += 1;
            checks < 2
        }),
        Err(SegmentError::Cancelled)
    );
    let bytes = encode_int4(std::iter::repeat_n(Some(1), GROUP_ROWS * 3), || true).unwrap();
    let mut checks = 0;
    assert_eq!(
        Int4Segment::open(&bytes, || {
            checks += 1;
            checks < 3
        })
        .unwrap_err(),
        SegmentError::Cancelled
    );
}

#[test]
fn survives_seeded_arbitrary_bytes_without_panicking() {
    let mut state = 0x1234_5678u32;
    for len in 0..512 {
        let bytes: Vec<u8> = (0..len)
            .map(|_| {
                state = state.wrapping_mul(1664525).wrapping_add(1013904223);
                (state >> 24) as u8
            })
            .collect();
        assert!(Int4Segment::open(&bytes, || true).is_err());
    }
}

#[test]
fn serialized_bytes_survive_a_file_roundtrip() {
    // create_new plus process ID gives this test exclusive ownership of its file.
    let path = std::env::temp_dir().join(format!("syntradb-segment-{}.bin", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    use std::io::Write;
    file.write_all(GOLDEN).unwrap();
    file.sync_all().unwrap();
    drop(file);
    let bytes = std::fs::read(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    assert_eq!(decode(&bytes), [Some(i32::MIN), None, Some(7)]);
}
