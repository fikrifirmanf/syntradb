use syntra_storage::{
    replay::{Applied, Batch, Binding, Change, Limits, ReplayState, Row},
    SegmentError,
};
fn limits() -> Limits {
    Limits {
        rows: 1_000_000,
        changes: 4096,
        buffer_bytes: 32 * 1024 * 1024,
    }
}
fn state() -> ReplayState {
    <ReplayState>::seed(
        Binding {
            stream: [7; 32],
            epoch: 9,
        },
        0,
        &[
            Row {
                key: -3,
                value: None,
            },
            Row {
                key: 5,
                value: Some(-7),
            },
        ],
        limits(),
        || true,
    )
    .unwrap()
}
fn repair(bytes: &mut [u8]) {
    let mut crc = crc32fast::Hasher::new();
    crc.update(&bytes[..104]);
    crc.update(&bytes[108..]);
    bytes[104..108].copy_from_slice(&crc.finalize().to_le_bytes());
}
#[test]
fn checkpoint_matches_an_independent_python_golden_fixture() {
    let expected = include_bytes!("fixtures/keyed_checkpoint_v1.bin");
    assert_eq!(state().encode_checkpoint(|| true).unwrap(), expected);
    let decoded = <ReplayState>::decode_checkpoint(expected, limits(), || true).unwrap();
    assert_eq!(decoded.rows(), state().rows());
    assert_eq!(decoded.binding(), state().binding());
    assert_eq!(decoded.sequence(), 0);
}
#[test]
fn restored_progress_and_retry_identity_prevent_duplicate_application() {
    let mut original = state();
    let binding = original.binding();
    let changes = [Change {
        key: 5,
        before: Some(Some(-7)),
        after: Some(Some(11)),
    }];
    original
        .apply(
            Batch {
                binding,
                previous: 0,
                next: 1,
                changes: &changes,
            },
            || true,
        )
        .unwrap();
    let bytes = original.encode_checkpoint(|| true).unwrap();
    let mut restored = <ReplayState>::decode_checkpoint(&bytes, limits(), || true).unwrap();
    assert_eq!(restored.sequence(), 1);
    assert_eq!(restored.rows(), original.rows());
    assert_eq!(
        restored.apply(
            Batch {
                binding,
                previous: 0,
                next: 1,
                changes: &changes
            },
            || true
        ),
        Ok(Applied::Duplicate)
    );
    assert_eq!(restored.encode_checkpoint(|| true).unwrap(), bytes);
}
#[test]
fn corrupted_truncated_and_trailing_bytes_are_rejected() {
    let bytes = state().encode_checkpoint(|| true).unwrap();
    for end in 0..bytes.len() {
        assert!(
            <ReplayState>::decode_checkpoint(&bytes[..end], limits(), || true).is_err(),
            "prefix {end}"
        );
    }
    for offset in 0..bytes.len() {
        let mut broken = bytes.clone();
        broken[offset] ^= 1;
        assert!(
            <ReplayState>::decode_checkpoint(&broken, limits(), || true).is_err(),
            "offset {offset}"
        );
    }
    let mut trailing = bytes;
    trailing.push(0);
    assert!(<ReplayState>::decode_checkpoint(&trailing, limits(), || true).is_err());
}
#[test]
fn canonical_key_order_null_payloads_and_bitmap_padding_are_enforced() {
    let bytes = state().encode_checkpoint(|| true).unwrap();
    for kind in 0..4 {
        let mut broken = bytes.clone();
        match kind {
            0 => broken[112] |= 0x80,
            1 => broken[117] = 1,
            2 => broken[121..125].copy_from_slice(&(-3_i32).to_le_bytes()),
            _ => broken[64] = 1,
        }
        repair(&mut broken);
        assert_eq!(
            <ReplayState>::decode_checkpoint(&broken, limits(), || true).err(),
            Some(SegmentError::NonCanonical)
        );
    }
}
#[test]
fn decode_checks_row_byte_and_format_limits_before_allocating() {
    let bytes = state().encode_checkpoint(|| true).unwrap();
    for restricted in [
        Limits {
            rows: 1,
            ..limits()
        },
        Limits {
            buffer_bytes: 1,
            ..limits()
        },
    ] {
        assert_eq!(
            <ReplayState>::decode_checkpoint(&bytes, restricted, || true).err(),
            Some(SegmentError::LimitExceeded)
        );
    }
    let mut invalid = bytes.clone();
    invalid[12..16].copy_from_slice(&1_000_001_u32.to_le_bytes());
    assert_eq!(
        <ReplayState>::decode_checkpoint(&invalid, limits(), || true).err(),
        Some(SegmentError::LimitExceeded)
    );
    invalid = bytes.clone();
    invalid[8..10].copy_from_slice(&3_u16.to_le_bytes());
    assert_eq!(
        <ReplayState>::decode_checkpoint(&invalid, limits(), || true).err(),
        Some(SegmentError::UnsupportedVersion(3))
    );
    // Version 2 is a wider layout: an int4 reader refuses it before its rows.
    invalid = bytes;
    invalid[8..10].copy_from_slice(&2_u16.to_le_bytes());
    assert_eq!(
        <ReplayState>::decode_checkpoint(&invalid, limits(), || true).err(),
        Some(SegmentError::InvalidHeader)
    );
}
#[test]
fn codec_cancellation_returns_no_partial_state_or_bytes() {
    let rows: Vec<_> = (0..4096)
        .map(|key| Row {
            key,
            value: Some(key),
        })
        .collect();
    let original = <ReplayState>::seed(state().binding(), 0, &rows, limits(), || true).unwrap();
    let mut polls = 0;
    assert_eq!(
        original.encode_checkpoint(|| {
            polls += 1;
            polls < 4
        }),
        Err(SegmentError::Cancelled)
    );
    let bytes = original.encode_checkpoint(|| true).unwrap();
    polls = 0;
    assert_eq!(
        <ReplayState>::decode_checkpoint(&bytes, limits(), || {
            polls += 1;
            polls < 5
        })
        .err(),
        Some(SegmentError::Cancelled)
    );
    assert_eq!(original.rows(), rows);
}
#[test]
fn maximum_row_checkpoint_fits_the_postgresql_bytea_bound() {
    let rows: Vec<_> = (0..1_000_000)
        .map(|key| Row {
            key,
            value: if key % 17 == 0 {
                None
            } else {
                Some(i32::MAX - key)
            },
        })
        .collect();
    let original = <ReplayState>::seed(state().binding(), 0, &rows, limits(), || true).unwrap();
    let bytes = original.encode_checkpoint(|| true).unwrap();
    assert_eq!(bytes.len(), 8_125_112);
    assert!(bytes.len() <= syntra_storage::MAX_SEGMENT_BYTES);
    let restored = <ReplayState>::decode_checkpoint(&bytes, limits(), || true).unwrap();
    assert_eq!(restored.rows(), rows);
}
#[test]
fn derived_bindings_distinguish_source_identities_and_epochs() {
    assert_ne!(
        Binding::derive(b"source:1:2", 1),
        Binding::derive(b"source:1:3", 1)
    );
    assert_ne!(
        Binding::derive(b"source:1:2", 1),
        Binding::derive(b"source:1:2", 2)
    );
    assert_eq!(
        Binding::derive(b"source:1:2", 1),
        Binding {
            stream: [
                125, 162, 143, 248, 146, 48, 0, 138, 105, 16, 73, 222, 115, 104, 178, 64, 239, 227,
                200, 42, 65, 227, 148, 145, 155, 238, 59, 112, 170, 151, 147, 12
            ],
            epoch: 1
        }
    );
}

#[test]
fn event_ranges_restore_their_ordinal_and_exact_retry_identity() {
    let mut original = state();
    let binding = original.binding();
    let changes = [
        Change {
            key: 5,
            before: Some(Some(-7)),
            after: None,
        },
        Change {
            key: 6,
            before: None,
            after: Some(Some(11)),
        },
    ];
    let batch = || Batch {
        binding,
        previous: 0,
        next: 2,
        changes: &changes,
    };
    assert_eq!(original.apply_events(batch(), || true), Ok(Applied::New));
    let bytes = original.encode_checkpoint(|| true).unwrap();
    let mut restored = <ReplayState>::decode_checkpoint(&bytes, limits(), || true).unwrap();
    assert_eq!(restored.sequence(), 2);
    assert_eq!(
        restored.rows(),
        &[
            Row {
                key: -3,
                value: None
            },
            Row {
                key: 6,
                value: Some(11)
            }
        ]
    );
    assert_eq!(
        restored.apply_events(batch(), || true),
        Ok(Applied::Duplicate)
    );
    assert_eq!(restored.encode_checkpoint(|| true).unwrap(), bytes);
}

#[test]
fn event_ranges_reject_empty_or_noncontiguous_ordinals_without_changing_state() {
    use syntra_storage::replay::ReplayError;
    let mut original = state();
    let binding = original.binding();
    let bytes = original.encode_checkpoint(|| true).unwrap();
    let changes = [Change {
        key: 5,
        before: Some(Some(-7)),
        after: Some(Some(11)),
    }];
    for (previous, next) in [(0, 2), (1, 2), (0, 0)] {
        assert_eq!(
            original.apply_events(
                Batch {
                    binding,
                    previous,
                    next,
                    changes: &changes
                },
                || true
            ),
            Err(ReplayError::SequenceGap)
        );
        assert_eq!(original.encode_checkpoint(|| true).unwrap(), bytes);
    }
    assert_eq!(
        original.apply_events(
            Batch {
                binding,
                previous: 0,
                next: 0,
                changes: &[]
            },
            || true
        ),
        Err(ReplayError::InvalidChange)
    );
    assert_eq!(original.encode_checkpoint(|| true).unwrap(), bytes);
}
