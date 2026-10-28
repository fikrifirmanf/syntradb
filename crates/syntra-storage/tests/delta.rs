use syntra_storage::{
    delta::{encoded_delta_len, Delta, DeltaEntry, MAX_DELTA_BYTES},
    replay::{Batch, Binding, Change, Limits, ReplayError, ReplayState, Row},
    Int, SegmentError,
};

// Generated independently by build-support/fixtures/gen_delta_fixtures.py.
const NARROW: &[u8] = include_bytes!("fixtures/delta_v1_int4.bin");
const WIDE: &[u8] = include_bytes!("fixtures/delta_v1_int8.bin");
const MIXED: &[u8] = include_bytes!("fixtures/delta_v1_int4_int8.bin");

fn binding() -> Binding {
    Binding {
        stream: [7; 32],
        epoch: 9,
    }
}
fn entry<K, V>(key: K, base: Option<Option<V>>, current: Option<Option<V>>) -> DeltaEntry<K, V> {
    DeltaEntry { key, base, current }
}
fn narrow_entries() -> Vec<DeltaEntry> {
    vec![
        entry(-5, None, Some(Some(7))),
        entry(3, Some(Some(1)), Some(None)),
        entry(9, Some(None), None),
        entry(12, Some(Some(2)), Some(Some(-3))),
    ]
}
/// The base the narrow fixture applies to.
fn narrow_base() -> Vec<Row> {
    vec![
        Row {
            key: 3,
            value: Some(1),
        },
        Row {
            key: 4,
            value: None,
        },
        Row {
            key: 9,
            value: None,
        },
        Row {
            key: 12,
            value: Some(2),
        },
    ]
}
/// Recomputes the CRC after a deliberate edit, so a later check is reached.
fn reseal(bytes: &mut [u8]) {
    let mut crc = crc32fast::Hasher::new();
    crc.update(&bytes[..64]);
    crc.update(&bytes[68..]);
    let crc = crc.finalize();
    bytes[64..68].copy_from_slice(&crc.to_le_bytes());
}
fn decode4(bytes: &[u8]) -> Result<Delta, SegmentError> {
    Delta::<i32, i32>::decode(bytes, || true)
}

#[test]
fn deltas_match_the_python_fixtures_in_every_layout() {
    let narrow = decode4(NARROW).unwrap();
    assert_eq!(
        (narrow.binding(), narrow.base_revision(), narrow.entries()),
        (binding(), 4, &narrow_entries()[..])
    );
    assert_eq!(narrow.encode(|| true).unwrap(), NARROW);
    assert_eq!(encoded_delta_len::<i32, i32>(4), Ok(NARROW.len()));

    let wide = Delta::<i64, i64>::decode(WIDE, || true).unwrap();
    assert_eq!(
        wide.entries(),
        [
            entry(-6_000_000_000, None, Some(Some(i64::MIN))),
            entry(3, Some(Some(i64::MAX)), Some(None)),
            entry(5_000_000_000, Some(None), None),
        ]
    );
    assert_eq!(wide.encode(|| true).unwrap(), WIDE);

    let mixed = Delta::<i32, i64>::decode(MIXED, || true).unwrap();
    assert_eq!(
        mixed.entries(),
        [
            entry(-5, None, Some(Some(7_000_000_000))),
            entry(12, Some(Some(2)), Some(None)),
        ]
    );
    assert_eq!(mixed.encode(|| true).unwrap(), MIXED);
}

#[test]
fn a_delta_is_only_readable_under_its_own_layout() {
    assert_eq!(decode4(WIDE).err(), Some(SegmentError::InvalidHeader));
    assert_eq!(decode4(MIXED).err(), Some(SegmentError::InvalidHeader));
    assert_eq!(
        Delta::<i64, i64>::decode(NARROW, || true).err(),
        Some(SegmentError::InvalidHeader)
    );
}

#[test]
fn damaged_and_noncanonical_deltas_are_rejected() {
    let edit = |change: &dyn Fn(&mut Vec<u8>), seal: bool| {
        let mut bytes = NARROW.to_vec();
        change(&mut bytes);
        if seal {
            reseal(&mut bytes);
        }
        decode4(&bytes).err()
    };
    // Entries start at 72 and are 13 bytes: key, tag, base value, current value.
    assert_eq!(
        edit(&|b| b[80] ^= 1, false),
        Some(SegmentError::ChecksumMismatch)
    );
    assert_eq!(
        edit(&|b| b[0] = b'X', true),
        Some(SegmentError::InvalidHeader)
    );
    assert_eq!(
        edit(&|b| b[8] = 2, true),
        Some(SegmentError::UnsupportedVersion(2))
    );
    assert_eq!(edit(&|b| b[70] = 1, true), Some(SegmentError::NonCanonical));
    // Tag bits above the two codes.
    assert_eq!(
        edit(&|b| b[76] |= 0x10, true),
        Some(SegmentError::NonCanonical)
    );
    // Tag code 3 does not exist.
    assert_eq!(edit(&|b| b[76] = 3, true), Some(SegmentError::NonCanonical));
    // First entry's base is absent, so its base value bytes must be zero.
    assert_eq!(edit(&|b| b[77] = 1, true), Some(SegmentError::NonCanonical));
    // Swap the first two keys: entries must be sorted.
    assert_eq!(
        edit(
            &|b| {
                let first: Vec<u8> = b[72..76].to_vec();
                let second: Vec<u8> = b[85..89].to_vec();
                b[72..76].copy_from_slice(&second);
                b[85..89].copy_from_slice(&first);
            },
            true
        ),
        Some(SegmentError::NonCanonical)
    );
    // Last entry's current value set equal to its base (2): such an entry is never stored.
    assert_eq!(
        edit(&|b| b[120..124].copy_from_slice(&2_i32.to_le_bytes()), true),
        Some(SegmentError::NonCanonical)
    );
    assert_eq!(
        edit(&|b| b[12..16].copy_from_slice(&3_u32.to_le_bytes()), true),
        Some(SegmentError::InvalidMetadata)
    );
    assert_eq!(
        edit(&|b| b[12..16].copy_from_slice(&0_u32.to_le_bytes()), true),
        Some(SegmentError::NonCanonical)
    );
    assert_eq!(decode4(&NARROW[..71]).err(), Some(SegmentError::Truncated));
    assert_eq!(
        decode4(&vec![0; MAX_DELTA_BYTES + 1]).err(),
        Some(SegmentError::LimitExceeded)
    );
    assert_eq!(
        Delta::<i32, i32>::empty(binding(), 4).encode(|| true).err(),
        Some(SegmentError::InvalidMetadata)
    );
}

#[test]
fn a_delta_applies_only_to_its_own_base() {
    let delta = decode4(NARROW).unwrap();
    assert_eq!(
        delta.merged(&narrow_base(), || true).unwrap(),
        [
            Row {
                key: -5,
                value: Some(7)
            },
            Row {
                key: 3,
                value: None
            },
            Row {
                key: 4,
                value: None
            },
            Row {
                key: 12,
                value: Some(-3)
            },
        ]
    );
    let mut other = narrow_base();
    other[3].value = Some(5);
    assert_eq!(
        delta.merged(&other, || true),
        Err(ReplayError::BeforeImageMismatch(12))
    );
    // A key the delta says was absent from the base, present there instead.
    let mut extra = narrow_base();
    extra.insert(
        0,
        Row {
            key: -5,
            value: Some(1),
        },
    );
    assert_eq!(
        delta.merged(&extra, || true),
        Err(ReplayError::BeforeImageMismatch(-5))
    );
    // Removals: 1, NULL and 2 leave the base; a base without them cannot finish.
    let mut removals = delta.removals();
    for value in [Some(1), None, Some(2)] {
        assert!(!removals.finished());
        assert!(removals.take(value));
    }
    assert!(removals.finished());
    assert!(!removals.take(Some(1)));
    let mut short = delta.removals();
    assert!(short.take(Some(1)) && short.take(None));
    assert!(!short.finished());
}

#[test]
fn advance_checks_before_images_against_base_and_delta() {
    let delta = decode4(NARROW).unwrap();
    let base = narrow_base();
    // Key 12 is -3 now; a before image of the base value 2 is stale.
    let stale = [Change {
        key: 12,
        before: Some(Some(2)),
        after: None,
    }];
    assert_eq!(
        delta.advance(&base, &stale, || true),
        Err(ReplayError::BeforeImageMismatch(12))
    );
    // Returning key 3 to its base value drops its entry; key 4 gains one.
    let changes = [
        Change {
            key: 3,
            before: Some(None),
            after: Some(Some(1)),
        },
        Change {
            key: 4,
            before: Some(None),
            after: Some(Some(8)),
        },
    ];
    let next = delta.advance(&base, &changes, || true).unwrap();
    assert_eq!(
        next.entries(),
        [
            entry(-5, None, Some(Some(7))),
            entry(4, Some(None), Some(Some(8))),
            entry(9, Some(None), None),
            entry(12, Some(Some(2)), Some(Some(-3))),
        ]
    );
    // A delta whose recorded base disagrees with the base it is given is refused.
    let mut wrong = base.clone();
    wrong[0].value = Some(6);
    assert_eq!(
        delta.advance(&wrong, &changes, || true),
        Err(ReplayError::BeforeImageMismatch(3))
    );
    let invalid = [Change {
        key: 1,
        before: None,
        after: None,
    }];
    assert_eq!(
        delta.advance(&base, &invalid, || true),
        Err(ReplayError::InvalidChange)
    );
}

#[test]
fn fold_rule_follows_entries_and_bytes() {
    let many = |n: i32| {
        let changes: Vec<Change> = (0..n)
            .map(|key| Change {
                key,
                before: None,
                after: Some(Some(key)),
            })
            .collect();
        Delta::empty(binding(), 0)
            .advance(&[], &changes, || true)
            .unwrap()
    };
    assert!(!many(64).fold_due(0));
    assert!(many(65).fold_due(0));
    assert!(!many(100).fold_due(1600));
    assert!(many(101).fold_due(1600));
    // 64 KiB holds (65536 - 72) / 13 = 5035 int4 entries; one more must fold.
    let big = many(4096).advance(
        &[],
        &(4096..5036)
            .map(|key| Change {
                key,
                before: None,
                after: Some(Some(key)),
            })
            .collect::<Vec<_>>(),
        || true,
    );
    let big = big.unwrap();
    assert_eq!(big.entries().len(), 5036);
    assert!(big.fold_due(usize::MAX / 2));
    assert_eq!(big.encode(|| true).err(), Some(SegmentError::LimitExceeded));
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// Aggregates (rows, non-NULL rows, sum, min, max) of a value multiset.
fn aggregate<V: Int>(
    values: impl Iterator<Item = Option<V>>,
) -> (usize, usize, i128, Option<i64>, Option<i64>) {
    let mut s = (0, 0, 0_i128, None, None::<i64>);
    for value in values {
        s.0 += 1;
        if let Some(v) = value {
            let v = v.to_i64();
            s.1 += 1;
            s.2 += i128::from(v);
            s.3 = Some(s.3.map_or(v, |m: i64| m.min(v)));
            s.4 = Some(s.4.map_or(v, |m: i64| m.max(v)));
        }
    }
    s
}

/// Random batches against the existing full-rewrite kernel. After every batch the
/// delta's merged rows equal the kernel's rows, the delta round-trips through its
/// encoding, and base values minus removals plus additions give the same
/// aggregates. The fold rule decides when to write a new base.
/// Keys and values are scaled so that 8-byte layouts leave the int4 range.
/// Returns how many times the fold rule wrote a new base.
fn random_feed<K: Int + TryFrom<i64>, V: Int + TryFrom<i64>>(
    seed: u64,
    key_scale: i64,
    value_scale: i64,
) -> usize {
    let limits = Limits {
        rows: 1_000_000,
        changes: 4096,
        buffer_bytes: 32 * 1024 * 1024,
    };
    let to_k = |n: i64| K::try_from(n * key_scale).ok().unwrap();
    let to_v = |n: i64| V::try_from(n * value_scale).ok().unwrap();
    let mut rng = Rng(seed);
    let mut truth = <ReplayState<K, V>>::seed(binding(), 0, &[], limits, || true).unwrap();
    let mut base: Vec<Row<K, V>> = Vec::new();
    let mut delta = Delta::<K, V>::empty(binding(), 0);
    let (mut folds, mut revision) = (0, 0_u64);
    for _ in 0..400 {
        let mut view: std::collections::BTreeMap<i64, Option<V>> = truth
            .rows()
            .iter()
            .map(|r| (r.key.to_i64(), r.value))
            .collect();
        let mut changes = Vec::new();
        for _ in 0..1 + rng.below(40) {
            let key = to_k(rng.below(200) as i64 - 100).to_i64();
            let before = view.get(&key).copied();
            let after = match rng.below(4) {
                0 => None,
                1 => Some(None),
                _ => Some(Some(to_v(rng.below(2001) as i64 - 1000))),
            };
            if before.is_none() && after.is_none() {
                continue;
            }
            match after {
                Some(value) => view.insert(key, value),
                None => view.remove(&key),
            };
            changes.push(Change {
                key: K::try_from(key).ok().unwrap(),
                before,
                after,
            });
        }
        if changes.is_empty() {
            continue;
        }
        let sequence = truth.sequence();
        truth
            .apply(
                Batch {
                    binding: binding(),
                    previous: sequence,
                    next: sequence + 1,
                    changes: &changes,
                },
                || true,
            )
            .unwrap();
        revision += 1;
        delta = delta.advance(&base, &changes, || true).unwrap();
        if delta.fold_due(base.len()) {
            base = delta.merged(&base, || true).unwrap();
            delta = Delta::empty(binding(), revision);
            folds += 1;
        }
        assert_eq!(delta.merged(&base, || true).unwrap(), truth.rows());
        if !delta.is_empty() {
            let bytes = delta.encode(|| true).unwrap();
            assert_eq!(Delta::<K, V>::decode(&bytes, || true).unwrap(), delta);
        }
        let mut removals = delta.removals();
        let read = aggregate(
            base.iter()
                .map(|r| r.value)
                .filter(|v| !removals.take(*v))
                .chain(delta.additions()),
        );
        assert!(removals.finished());
        assert_eq!(read, aggregate(truth.rows().iter().map(|r| r.value)));
    }
    folds
}

#[test]
fn random_feeds_match_full_rewrites_in_every_layout() {
    for seed in 1..=40 {
        // Each feed must fold several times, so both paths are exercised.
        assert!(random_feed::<i32, i32>(seed, 1, 1) > 2, "int4 seed {seed}");
        assert!(
            random_feed::<i64, i64>(seed + 1000, 5_000_000_000, 3_000_000_000_000_000) > 2,
            "int8 seed {seed}"
        );
        assert!(
            random_feed::<i32, i64>(seed + 2000, 1, 3_000_000_000) > 2,
            "mixed seed {seed}"
        );
    }
}
