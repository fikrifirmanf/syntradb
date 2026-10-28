use syntra_storage::replay::{
    Applied, Batch, Binding, Change, Limits, ReplayError, ReplayState, Row,
};

fn binding() -> Binding {
    Binding {
        stream: [7; 32],
        epoch: 1,
    }
}
fn limits() -> Limits {
    Limits {
        rows: 8192,
        changes: 64,
        buffer_bytes: 1_048_576,
    }
}
fn seed(rows: &[Row]) -> ReplayState {
    <ReplayState>::seed(binding(), 0, rows, limits(), || true).unwrap()
}
fn batch(changes: &[Change], previous: u64) -> Batch<'_> {
    Batch {
        binding: binding(),
        previous,
        next: previous + 1,
        changes,
    }
}

#[test]
fn replays_insert_update_delete_nulls_duplicates_and_integer_endpoints() {
    let mut state = seed(&[
        Row {
            key: -1,
            value: None,
        },
        Row {
            key: 1,
            value: Some(5),
        },
        Row {
            key: 3,
            value: Some(5),
        },
    ]);
    let changes = [
        Change {
            key: 1,
            before: Some(Some(5)),
            after: Some(None),
        },
        Change {
            key: 3,
            before: Some(Some(5)),
            after: None,
        },
        Change {
            key: 4,
            before: None,
            after: Some(None),
        },
        Change {
            key: -1,
            before: Some(None),
            after: None,
        },
        Change {
            key: i32::MIN,
            before: None,
            after: Some(Some(i32::MIN)),
        },
        Change {
            key: i32::MAX,
            before: None,
            after: Some(Some(i32::MAX)),
        },
    ];
    assert_eq!(state.apply(batch(&changes, 0), || true), Ok(Applied::New));
    assert_eq!(
        state.rows(),
        &[
            Row {
                key: i32::MIN,
                value: Some(i32::MIN)
            },
            Row {
                key: 1,
                value: None
            },
            Row {
                key: 4,
                value: None
            },
            Row {
                key: i32::MAX,
                value: Some(i32::MAX)
            }
        ]
    );
    assert_eq!(state.sequence(), 1);
}

#[test]
fn key_moves_at_capacity_preserve_per_key_event_order() {
    let initial = [
        Row {
            key: 1,
            value: Some(10),
        },
        Row {
            key: 2,
            value: Some(20),
        },
    ];
    let mut state = <ReplayState>::seed(
        binding(),
        0,
        &initial,
        Limits {
            rows: 2,
            ..limits()
        },
        || true,
    )
    .unwrap();
    let changes = [
        Change {
            key: 1,
            before: Some(Some(10)),
            after: None,
        },
        Change {
            key: 2,
            before: Some(Some(20)),
            after: None,
        },
        Change {
            key: 2,
            before: None,
            after: Some(Some(10)),
        },
        Change {
            key: 1,
            before: None,
            after: Some(Some(20)),
        },
    ];
    assert_eq!(state.apply(batch(&changes, 0), || true), Ok(Applied::New));
    assert_eq!(
        state.rows(),
        &[
            Row {
                key: 1,
                value: Some(20)
            },
            Row {
                key: 2,
                value: Some(10)
            }
        ]
    );
}

#[test]
fn a_late_before_image_conflict_leaves_rows_cursor_and_retry_state_unchanged() {
    let initial = [
        Row {
            key: 1,
            value: None,
        },
        Row {
            key: 2,
            value: Some(5),
        },
    ];
    let mut state = seed(&initial);
    let bad = [
        Change {
            key: 1,
            before: Some(None),
            after: Some(Some(10)),
        },
        Change {
            key: 2,
            before: Some(Some(6)),
            after: None,
        },
    ];
    assert_eq!(
        state.apply(batch(&bad, 0), || true),
        Err(ReplayError::BeforeImageMismatch(2))
    );
    assert_eq!(state.rows(), initial);
    assert_eq!(state.sequence(), 0);
    let valid = [
        bad[0],
        Change {
            key: 2,
            before: Some(Some(5)),
            after: None,
        },
    ];
    assert_eq!(state.apply(batch(&valid, 0), || true), Ok(Applied::New));
    assert_eq!(
        state.apply(batch(&valid, 0), || true),
        Ok(Applied::Duplicate)
    );
    assert_eq!(
        state.rows(),
        &[Row {
            key: 1,
            value: Some(10)
        }]
    );
}

#[test]
fn exact_retry_is_idempotent_but_conflicting_content_and_older_batches_are_rejected() {
    let mut state = seed(&[]);
    let changes = [
        Change {
            key: 1,
            before: None,
            after: Some(None),
        },
        Change {
            key: 2,
            before: None,
            after: Some(Some(10)),
        },
    ];
    state.apply(batch(&changes, 0), || true).unwrap();
    assert_eq!(
        state.apply(batch(&changes, 0), || true),
        Ok(Applied::Duplicate)
    );
    let reordered = [changes[1], changes[0]];
    assert_eq!(
        state.apply(batch(&reordered, 0), || true),
        Err(ReplayError::ConflictingRetry)
    );
    let forged = [
        Change {
            key: 1,
            before: None,
            after: Some(Some(0)),
        },
        changes[1],
    ];
    assert_eq!(
        state.apply(batch(&forged, 0), || true),
        Err(ReplayError::ConflictingRetry)
    );
    state.apply(batch(&[], 1), || true).unwrap();
    assert_eq!(
        state.apply(batch(&changes, 0), || true),
        Err(ReplayError::SequenceGap)
    );
    assert_eq!(
        state.rows(),
        &[
            Row {
                key: 1,
                value: None
            },
            Row {
                key: 2,
                value: Some(10)
            }
        ]
    );
    assert_eq!(state.sequence(), 2);
}

#[test]
fn stream_epoch_gaps_and_sequence_overflow_cannot_advance_progress() {
    let mut state = seed(&[]);
    for altered in [
        Binding {
            stream: [8; 32],
            ..binding()
        },
        Binding {
            epoch: 2,
            ..binding()
        },
    ] {
        assert_eq!(
            state.apply(
                Batch {
                    binding: altered,
                    ..batch(&[], 0)
                },
                || true
            ),
            Err(ReplayError::WrongBinding)
        );
    }
    assert_eq!(
        state.apply(batch(&[], 1), || true),
        Err(ReplayError::SequenceGap)
    );
    assert_eq!(
        state.apply(
            Batch {
                next: 2,
                ..batch(&[], 0)
            },
            || true
        ),
        Err(ReplayError::SequenceGap)
    );
    assert_eq!(state.sequence(), 0);
    assert_eq!(state.binding(), binding());
    let mut exhausted = <ReplayState>::seed(binding(), u64::MAX, &[], limits(), || true).unwrap();
    assert_eq!(
        exhausted.apply(
            Batch {
                binding: binding(),
                previous: u64::MAX,
                next: 0,
                changes: &[]
            },
            || true
        ),
        Err(ReplayError::SequenceGap)
    );
    assert_eq!(exhausted.sequence(), u64::MAX);
}

#[test]
fn invalid_seed_order_limits_and_capacity_are_rejected() {
    for rows in [
        [
            Row {
                key: 2,
                value: None,
            },
            Row {
                key: 1,
                value: None,
            },
        ],
        [
            Row {
                key: 1,
                value: Some(1),
            },
            Row {
                key: 1,
                value: None,
            },
        ],
    ] {
        assert_eq!(
            <ReplayState>::seed(binding(), 0, &rows, limits(), || true).err(),
            Some(ReplayError::InvalidSeed)
        );
    }
    for invalid in [
        Limits {
            rows: 1_000_001,
            ..limits()
        },
        Limits {
            changes: 0,
            ..limits()
        },
        Limits {
            changes: 4097,
            ..limits()
        },
    ] {
        assert_eq!(
            <ReplayState>::seed(binding(), 0, &[], invalid, || true).err(),
            Some(ReplayError::InvalidLimits)
        );
    }
    assert_eq!(
        <ReplayState>::seed(
            Binding {
                epoch: 0,
                ..binding()
            },
            0,
            &[],
            limits(),
            || true
        )
        .err(),
        Some(ReplayError::InvalidLimits)
    );
    assert_eq!(
        <ReplayState>::seed(
            binding(),
            0,
            &[Row {
                key: 1,
                value: None
            }],
            Limits {
                rows: 0,
                ..limits()
            },
            || true
        )
        .err(),
        Some(ReplayError::RowLimit)
    );
    assert_eq!(
        <ReplayState>::seed(
            binding(),
            0,
            &[Row {
                key: 1,
                value: None
            }],
            Limits {
                buffer_bytes: 0,
                ..limits()
            },
            || true
        )
        .err(),
        Some(ReplayError::BufferLimit)
    );
}

#[test]
fn row_change_and_owned_buffer_limits_are_atomic() {
    let initial = [Row {
        key: 1,
        value: Some(1),
    }];
    let mut state = <ReplayState>::seed(
        binding(),
        0,
        &initial,
        Limits {
            rows: 1,
            changes: 1,
            ..limits()
        },
        || true,
    )
    .unwrap();
    let insert = Change {
        key: 2,
        before: None,
        after: Some(None),
    };
    assert_eq!(
        state.apply(batch(&[insert], 0), || true),
        Err(ReplayError::RowLimit)
    );
    assert_eq!(
        state.apply(batch(&[insert, insert], 0), || true),
        Err(ReplayError::ChangeLimit)
    );
    assert_eq!(state.rows(), initial);
    assert_eq!(state.sequence(), 0);
    let mut tight = <ReplayState>::seed(
        binding(),
        0,
        &initial,
        Limits {
            buffer_bytes: std::mem::size_of::<Row>(),
            ..limits()
        },
        || true,
    )
    .unwrap();
    let update = Change {
        key: 1,
        before: Some(Some(1)),
        after: Some(None),
    };
    assert_eq!(
        tight.apply(batch(&[update], 0), || true),
        Err(ReplayError::BufferLimit)
    );
    assert_eq!(tight.rows(), initial);
    assert_eq!(tight.sequence(), 0);
}

#[test]
fn cancellation_during_a_long_merge_preserves_the_entire_old_state() {
    let rows: Vec<_> = (0..5120)
        .map(|n| Row {
            key: n,
            value: Some(n),
        })
        .collect();
    let mut state = seed(&rows);
    let change = Change {
        key: 5119,
        before: Some(Some(5119)),
        after: None,
    };
    let mut polls = 0;
    assert_eq!(
        state.apply(batch(&[change], 0), || {
            polls += 1;
            polls < 7
        }),
        Err(ReplayError::Cancelled)
    );
    assert_eq!(polls, 7);
    assert_eq!(state.rows(), rows);
    assert_eq!(state.sequence(), 0);
    assert_eq!(state.apply(batch(&[change], 0), || true), Ok(Applied::New));
    assert_eq!(state.rows(), &rows[..5119]);
}

#[test]
fn absent_null_and_empty_progress_are_distinct() {
    let mut state = seed(&[Row {
        key: 1,
        value: None,
    }]);
    assert_eq!(
        state.apply(
            batch(
                &[Change {
                    key: 1,
                    before: None,
                    after: Some(Some(2))
                }],
                0
            ),
            || true
        ),
        Err(ReplayError::BeforeImageMismatch(1))
    );
    assert_eq!(
        state.apply(
            batch(
                &[Change {
                    key: 2,
                    before: None,
                    after: None
                }],
                0
            ),
            || true
        ),
        Err(ReplayError::InvalidChange)
    );
    assert_eq!(state.apply(batch(&[], 0), || true), Ok(Applied::New));
    assert_eq!(state.apply(batch(&[], 0), || true), Ok(Applied::Duplicate));
    assert_eq!(
        state.rows(),
        &[Row {
            key: 1,
            value: None
        }]
    );
    assert_eq!(state.sequence(), 1);
    assert_eq!(
        state.apply(batch(&[], 1), || false),
        Err(ReplayError::Cancelled)
    );
    assert_eq!(state.sequence(), 1);
}

#[test]
fn seeded_batches_match_an_independent_ordered_map_through_repeated_keys_and_deletes() {
    use std::collections::BTreeMap;
    let mut reference = BTreeMap::<i32, Option<i32>>::new();
    let mut state = seed(&[]);
    let mut rng = 29u64;
    for previous in 0..200 {
        let mut changes = Vec::new();
        for _ in 0..17 {
            rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
            let key = ((rng >> 32) % 33) as i32 - 16;
            let before = reference.get(&key).copied();
            let mut after = if rng.is_multiple_of(7) {
                None
            } else {
                Some(if rng.is_multiple_of(5) {
                    None
                } else {
                    Some((rng >> 16) as i32)
                })
            };
            if before.is_none() && after.is_none() {
                after = Some(None);
            }
            match after {
                None => {
                    reference.remove(&key);
                }
                Some(v) => {
                    reference.insert(key, v);
                }
            }
            changes.push(Change { key, before, after });
        }
        assert_eq!(
            state.apply(batch(&changes, previous), || true),
            Ok(Applied::New)
        );
        let expected: Vec<_> = reference
            .iter()
            .map(|(key, value)| Row {
                key: *key,
                value: *value,
            })
            .collect();
        assert_eq!(state.rows(), expected, "batch {previous}");
        assert_eq!(state.sequence(), previous + 1);
        assert_eq!(
            state.apply(batch(&changes, previous), || true),
            Ok(Applied::Duplicate)
        );
    }
}
