use syntra_storage::{
    chunks::{
        chunk_binding, digest, partition, ChunkRef, DeltaRef, Manifest, MANIFEST_BYTES,
        MANIFEST_V2_BYTES, PARTITIONS,
    },
    replay::Binding,
    SegmentError,
};
fn manifest() -> Manifest {
    let mut chunks = [ChunkRef::default(); PARTITIONS];
    chunks[0] = ChunkRef {
        revision: 1,
        rows: 2,
        state_digest: [3; 32],
        segment_digest: [4; 32],
        delta: None,
    };
    Manifest {
        binding: Binding {
            stream: [7; 32],
            epoch: 9,
        },
        applied: 5,
        generation: 2,
        chunks,
    }
}
fn repair(bytes: &mut [u8]) {
    let mut crc = crc32fast::Hasher::new();
    crc.update(&bytes[..72]);
    crc.update(&bytes[76..]);
    bytes[72..76].copy_from_slice(&crc.finalize().to_le_bytes());
}
#[test]
fn manifest_matches_independently_generated_python_bytes() {
    let fixture = include_bytes!("fixtures/chunk_manifest_v1.bin");
    assert_eq!(manifest().encode().unwrap(), fixture);
    assert_eq!(Manifest::decode(fixture).unwrap(), manifest());
    assert_eq!(fixture.len(), MANIFEST_BYTES);
}
#[test]
fn every_byte_corruption_truncation_and_trailing_bytes_fail() {
    let original = manifest().encode().unwrap();
    for index in 0..original.len() {
        let mut bytes = original.clone();
        bytes[index] ^= 1;
        assert!(Manifest::decode(&bytes).is_err(), "offset {index}");
        assert!(Manifest::decode(&original[..index]).is_err());
    }
    let mut bytes = original;
    bytes.push(0);
    assert!(Manifest::decode(&bytes).is_err());
}
#[test]
fn checksum_valid_noncanonical_fields_and_progress_are_rejected() {
    let original = manifest().encode().unwrap();
    for index in [10, 76, 92] {
        let mut bytes = original.clone();
        bytes[index] ^= 1;
        repair(&mut bytes);
        assert_eq!(Manifest::decode(&bytes), Err(SegmentError::NonCanonical));
    }
    for (offset, value) in [(48, 0_u64), (64, 0), (64, 1), (56, u64::MAX), (80, 2)] {
        let mut bytes = original.clone();
        bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        repair(&mut bytes);
        assert_eq!(Manifest::decode(&bytes), Err(SegmentError::InvalidMetadata));
    }
    let mut bytes = original;
    bytes[12..16].copy_from_slice(&3_u32.to_le_bytes());
    repair(&mut bytes);
    assert_eq!(Manifest::decode(&bytes), Err(SegmentError::InvalidMetadata));
}
#[test]
fn source_row_limit_and_revision_bound_apply_before_encoding() {
    let mut m = manifest();
    m.chunks[1].rows = 999998;
    assert_eq!(m.rows(), Ok(1_000_000));
    assert!(m.encode().is_ok());
    m.chunks[1].rows += 1;
    assert_eq!(m.encode(), Err(SegmentError::LimitExceeded));
    m = manifest();
    m.chunks[1].revision = 2;
    assert_eq!(m.encode(), Err(SegmentError::InvalidMetadata));
    m = manifest();
    m.chunks[1].rows = u32::MAX;
    assert_eq!(m.encode(), Err(SegmentError::LimitExceeded));
}
#[test]
fn routing_matches_python_endpoints_and_spreads_sequential_keys() {
    let expected = include_str!("fixtures/chunk_routing_v1.bin");
    for line in expected.lines() {
        let (key, bucket) = line.split_once(' ').unwrap();
        assert_eq!(
            partition(key.parse().unwrap()),
            bucket.parse::<usize>().unwrap()
        );
    }
    let mut counts = [0; PARTITIONS];
    for key in 0..100000 {
        counts[partition(key)] += 1;
    }
    assert!(
        counts.iter().all(|n| (1300..1900).contains(n)),
        "{counts:?}"
    );
    // Deliberate skew remains routable; no partition-specific row limit is imposed.
    assert_eq!(
        (0..300000)
            .filter(|key| partition(*key) == 0)
            .take(4096)
            .count(),
        4096
    );
}
#[test]
fn digests_and_partition_bindings_match_independent_sha256_vectors() {
    assert_eq!(
        digest(b"hello", || true).unwrap(),
        include_bytes!("fixtures/chunk_digest_v1.bin").as_slice()
    );
    assert_eq!(
        chunk_binding(manifest().binding, 0).unwrap().stream,
        include_bytes!("fixtures/chunk_binding_v1.bin").as_slice()
    );
    assert_ne!(
        chunk_binding(manifest().binding, 0),
        chunk_binding(manifest().binding, 1)
    );
    assert_eq!(
        chunk_binding(manifest().binding, 64),
        Err(SegmentError::InvalidMetadata)
    );
}
#[test]
fn hashing_is_bounded_and_cancellable_in_long_payloads() {
    let bytes = vec![1; 200000];
    let mut polls = 0;
    assert_eq!(
        digest(&bytes, || {
            polls += 1;
            polls < 3
        }),
        Err(SegmentError::Cancelled)
    );
    assert_eq!(
        digest(&vec![0; syntra_storage::MAX_SEGMENT_BYTES + 1], || true),
        Err(SegmentError::LimitExceeded)
    );
}

fn with_delta() -> Manifest {
    let mut m = manifest();
    m.chunks[0].delta = Some(DeltaRef {
        base_revision: 0,
        base_rows: 3,
        entries: 4,
        digest: [5; 32],
    });
    m
}
#[test]
fn version_2_matches_python_bytes_and_is_written_only_with_a_delta() {
    let fixture = include_bytes!("fixtures/chunk_manifest_v2.bin");
    assert_eq!(fixture.len(), MANIFEST_V2_BYTES);
    assert_eq!(with_delta().encode().unwrap(), fixture);
    assert_eq!(Manifest::decode(fixture).unwrap(), with_delta());
    // Without a delta the same manifest is still version 1, byte for byte.
    assert_eq!(
        manifest().encode().unwrap(),
        include_bytes!("fixtures/chunk_manifest_v1.bin")
    );
    assert_eq!(with_delta().chunks[0].base_rows(), 3);
    assert_eq!(manifest().chunks[0].base_rows(), 2);
}
#[test]
fn version_2_corruption_and_noncanonical_references_are_rejected() {
    let original = with_delta().encode().unwrap();
    for index in 0..original.len() {
        let mut bytes = original.clone();
        bytes[index] ^= 1;
        assert!(Manifest::decode(&bytes).is_err(), "offset {index}");
    }
    let edit = |offset: usize, value: &[u8]| {
        let mut bytes = original.clone();
        bytes[offset..offset + value.len()].copy_from_slice(value);
        repair(&mut bytes);
        Manifest::decode(&bytes)
    };
    // Partition 0 starts at 80; partition 1 at 208.
    // Reserved bytes after the entry count.
    assert_eq!(edit(80 + 92, &[1]), Err(SegmentError::NonCanonical));
    // A partition without a delta must repeat its own rows and revision as base.
    assert_eq!(edit(208 + 12, &[1]), Err(SegmentError::NonCanonical));
    assert_eq!(edit(208 + 80, &[1]), Err(SegmentError::NonCanonical));
    assert_eq!(edit(208 + 96, &[1]), Err(SegmentError::NonCanonical));
    // A delta's base must be older than the partition.
    assert_eq!(
        edit(80 + 80, &1_u64.to_le_bytes()),
        Err(SegmentError::InvalidMetadata)
    );
    // Removing the only delta leaves a version 2 manifest that should be version 1.
    let mut bytes = original.clone();
    bytes[80 + 12..80 + 16].copy_from_slice(&2_u32.to_le_bytes());
    bytes[80 + 80..80 + 88].copy_from_slice(&1_u64.to_le_bytes());
    bytes[80 + 88..80 + 92].copy_from_slice(&0_u32.to_le_bytes());
    bytes[80 + 96..80 + 128].copy_from_slice(&[0; 32]);
    repair(&mut bytes);
    assert_eq!(Manifest::decode(&bytes), Err(SegmentError::NonCanonical));
    // Length and version must agree.
    let mut short = original.clone();
    short.truncate(MANIFEST_BYTES);
    repair(&mut short);
    assert_eq!(Manifest::decode(&short), Err(SegmentError::InvalidMetadata));
}
#[test]
fn delta_references_are_bounded_before_encoding() {
    let mut m = with_delta();
    m.chunks[0].delta.as_mut().unwrap().entries = 0;
    assert_eq!(m.encode(), Err(SegmentError::InvalidMetadata));
    m.chunks[0].delta.as_mut().unwrap().entries = 5035;
    assert!(m.encode().is_ok());
    m.chunks[0].delta.as_mut().unwrap().entries = 5036;
    assert_eq!(m.encode(), Err(SegmentError::InvalidMetadata));
    m = with_delta();
    m.chunks[0].delta.as_mut().unwrap().base_revision = 1;
    assert_eq!(m.encode(), Err(SegmentError::InvalidMetadata));
}
