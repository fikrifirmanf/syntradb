//! Partition deltas: the net change to a partition since its base state and
//! segment were written. See docs/checkpoint-delta-design.md.
//!
//! An entry records a key's value in the base and its value now, each absent,
//! NULL or a value. Entries are sorted by key, one per key, and an entry whose two
//! values are equal is never stored. An empty delta is never encoded.
use crate::replay::{Binding, Change, ReplayError, Row};
use crate::{int_at, u16_at, u32_at, Int, SegmentError};

pub const DELTA_HEADER: usize = 72;
/// Largest encoded delta, header included. Apply folds before reaching it.
pub const MAX_DELTA_BYTES: usize = 64 * 1024;
const MAGIC: &[u8; 8] = b"SYNTRACD";
const VERSION: u16 = 1;

/// Outer None means absent; Some(None) means a present SQL NULL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeltaEntry<K = i32, V = i32> {
    pub key: K,
    pub base: Option<Option<V>>,
    pub current: Option<Option<V>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delta<K = i32, V = i32> {
    binding: Binding,
    base_revision: u64,
    entries: Vec<DeltaEntry<K, V>>,
}

fn poll<E>(keep: &mut impl FnMut() -> bool, cancelled: E) -> Result<(), E> {
    if keep() {
        Ok(())
    } else {
        Err(cancelled)
    }
}
const fn stride<K: Int, V: Int>() -> usize {
    K::WIDTH + 1 + 2 * V::WIDTH
}
/// Exact encoded length of a delta with `entries` entries.
pub fn encoded_delta_len<K: Int, V: Int>(entries: usize) -> Result<usize, SegmentError> {
    let total = entries
        .checked_mul(stride::<K, V>())
        .and_then(|n| n.checked_add(DELTA_HEADER))
        .ok_or(SegmentError::LimitExceeded)?;
    if entries == 0 {
        return Err(SegmentError::InvalidMetadata);
    }
    if total > MAX_DELTA_BYTES {
        return Err(SegmentError::LimitExceeded);
    }
    Ok(total)
}
fn code<V>(value: Option<Option<V>>) -> u8 {
    match value {
        None => 0,
        Some(None) => 1,
        Some(Some(_)) => 2,
    }
}
fn lookup<K: Int, V: Int>(base: &[Row<K, V>], key: K) -> Option<Option<V>> {
    base.binary_search_by_key(&key, |row| row.key)
        .ok()
        .map(|i| base[i].value)
}
fn crc(bytes: &[u8], keep: &mut impl FnMut() -> bool) -> Result<u32, SegmentError> {
    let mut hash = crc32fast::Hasher::new();
    for slice in [&bytes[..64], &bytes[68..]] {
        for chunk in slice.chunks(65536) {
            poll(keep, SegmentError::Cancelled)?;
            hash.update(chunk);
        }
    }
    Ok(hash.finalize())
}

impl<K: Int, V: Int> Delta<K, V> {
    pub fn empty(binding: Binding, base_revision: u64) -> Self {
        Self {
            binding,
            base_revision,
            entries: Vec::new(),
        }
    }
    pub fn binding(&self) -> Binding {
        self.binding
    }
    pub fn base_revision(&self) -> u64 {
        self.base_revision
    }
    pub fn entries(&self) -> &[DeltaEntry<K, V>] {
        &self.entries
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    /// Change in row count from the base to now.
    pub fn row_change(&self) -> i64 {
        self.entries
            .iter()
            .map(|e| i64::from(e.current.is_some()) - i64::from(e.base.is_some()))
            .sum()
    }
    /// The fold rule: more than `max(64, base_rows / 16)` entries, or an encoding
    /// over `MAX_DELTA_BYTES`, means apply writes a new base instead.
    pub fn fold_due(&self, base_rows: usize) -> bool {
        self.entries.len() > (base_rows / 16).max(64)
            || encoded_delta_len::<K, V>(self.entries.len().max(1))
                .is_err_and(|e| e == SegmentError::LimitExceeded)
    }

    /// Replays `changes` in event order onto the base overlaid with this delta and
    /// returns the new net delta. Every before image must match, and every touched
    /// entry's recorded base must equal `base`. Nothing changes on error.
    pub fn advance(
        &self,
        base: &[Row<K, V>],
        changes: &[Change<K, V>],
        mut keep: impl FnMut() -> bool,
    ) -> Result<Self, ReplayError> {
        poll(&mut keep, ReplayError::Cancelled)?;
        if changes.len() > 4096 {
            return Err(ReplayError::ChangeLimit);
        }
        let mut order: Vec<usize> = Vec::new();
        order
            .try_reserve_exact(changes.len())
            .map_err(|_| ReplayError::AllocationFailed)?;
        order.extend(0..changes.len());
        // Event order is kept per key; independent keys are processed in key order.
        order.sort_unstable_by_key(|index| (changes[*index].key, *index));
        let mut touched: Vec<(K, Option<DeltaEntry<K, V>>)> = Vec::new();
        let mut position = 0;
        while position < order.len() {
            let key = changes[order[position]].key;
            let base_value = lookup(base, key);
            let existing = self
                .entries
                .binary_search_by_key(&key, |e| e.key)
                .ok()
                .map(|i| self.entries[i]);
            if existing.is_some_and(|e| e.base != base_value) {
                return Err(ReplayError::BeforeImageMismatch(key.to_i64()));
            }
            let mut value = existing.map_or(base_value, |e| e.current);
            while position < order.len() && changes[order[position]].key == key {
                if position % 64 == 0 {
                    poll(&mut keep, ReplayError::Cancelled)?;
                }
                let change = changes[order[position]];
                if change.before.is_none() && change.after.is_none() {
                    return Err(ReplayError::InvalidChange);
                }
                if change.before != value {
                    return Err(ReplayError::BeforeImageMismatch(key.to_i64()));
                }
                value = change.after;
                position += 1;
            }
            let entry = (value != base_value).then_some(DeltaEntry {
                key,
                base: base_value,
                current: value,
            });
            touched.push((key, entry));
        }
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(self.entries.len() + touched.len())
            .map_err(|_| ReplayError::AllocationFailed)?;
        let (mut old, mut new) = (0, 0);
        while old < self.entries.len() || new < touched.len() {
            if (old + new) % 1024 == 0 {
                poll(&mut keep, ReplayError::Cancelled)?;
            }
            let take_old = old < self.entries.len()
                && (new == touched.len() || self.entries[old].key < touched[new].0);
            if take_old {
                entries.push(self.entries[old]);
                old += 1;
            } else {
                if old < self.entries.len() && self.entries[old].key == touched[new].0 {
                    old += 1;
                }
                if let Some(entry) = touched[new].1 {
                    entries.push(entry);
                }
                new += 1;
            }
        }
        Ok(Self {
            binding: self.binding,
            base_revision: self.base_revision,
            entries,
        })
    }

    /// The base with this delta applied. Fails if an entry's recorded base value
    /// disagrees with `base`, so a delta can never be applied to another base.
    pub fn merged(
        &self,
        base: &[Row<K, V>],
        mut keep: impl FnMut() -> bool,
    ) -> Result<Vec<Row<K, V>>, ReplayError> {
        let rows = (base.len() as i64)
            .checked_add(self.row_change())
            .filter(|n| *n >= 0)
            .ok_or(ReplayError::InvalidChange)? as usize;
        let mut output = Vec::new();
        output
            .try_reserve_exact(rows)
            .map_err(|_| ReplayError::AllocationFailed)?;
        let (mut b, mut d) = (0, 0);
        while b < base.len() || d < self.entries.len() {
            if (b + d) % 1024 == 0 {
                poll(&mut keep, ReplayError::Cancelled)?;
            }
            if d == self.entries.len() || (b < base.len() && base[b].key < self.entries[d].key) {
                output.push(base[b]);
                b += 1;
                continue;
            }
            let entry = self.entries[d];
            let in_base = b < base.len() && base[b].key == entry.key;
            if entry.base != in_base.then(|| base[b].value) {
                return Err(ReplayError::BeforeImageMismatch(entry.key.to_i64()));
            }
            if in_base {
                b += 1;
            }
            if let Some(value) = entry.current {
                output.push(Row {
                    key: entry.key,
                    value,
                });
            }
            d += 1;
        }
        Ok(output)
    }

    /// Values the delta removes from the base and adds now, as multisets. A reader
    /// sums a partition as its base values minus `removals`, plus `additions`.
    pub fn removals(&self) -> Removals<V> {
        Removals::new(self.entries.iter().filter_map(|e| e.base))
    }
    pub fn additions(&self) -> impl Iterator<Item = Option<V>> + '_ {
        self.entries.iter().filter_map(|e| e.current)
    }

    pub fn encode(&self, mut keep: impl FnMut() -> bool) -> Result<Vec<u8>, SegmentError> {
        poll(&mut keep, SegmentError::Cancelled)?;
        check(self.binding, &self.entries)?;
        let total = encoded_delta_len::<K, V>(self.entries.len())?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(total)
            .map_err(|_| SegmentError::AllocationFailed)?;
        bytes.resize(total, 0);
        bytes[..8].copy_from_slice(MAGIC);
        bytes[8..10].copy_from_slice(&VERSION.to_le_bytes());
        bytes[10] = K::WIDTH as u8;
        bytes[11] = V::WIDTH as u8;
        bytes[12..16].copy_from_slice(&(self.entries.len() as u32).to_le_bytes());
        bytes[16..48].copy_from_slice(&self.binding.stream);
        bytes[48..56].copy_from_slice(&self.binding.epoch.to_le_bytes());
        bytes[56..64].copy_from_slice(&self.base_revision.to_le_bytes());
        for (index, e) in self.entries.iter().enumerate() {
            if index % 1024 == 0 {
                poll(&mut keep, SegmentError::Cancelled)?;
            }
            let offset = DELTA_HEADER + index * stride::<K, V>();
            e.key.put(&mut bytes[offset..]);
            bytes[offset + K::WIDTH] = code(e.base) | code(e.current) << 2;
            if let Some(Some(v)) = e.base {
                v.put(&mut bytes[offset + K::WIDTH + 1..]);
            }
            if let Some(Some(v)) = e.current {
                v.put(&mut bytes[offset + K::WIDTH + 1 + V::WIDTH..]);
            }
        }
        let checksum = crc(&bytes, &mut keep)?;
        bytes[64..68].copy_from_slice(&checksum.to_le_bytes());
        Ok(bytes)
    }

    pub fn decode(bytes: &[u8], mut keep: impl FnMut() -> bool) -> Result<Self, SegmentError> {
        poll(&mut keep, SegmentError::Cancelled)?;
        if bytes.len() < DELTA_HEADER {
            return Err(SegmentError::Truncated);
        }
        if bytes.len() > MAX_DELTA_BYTES {
            return Err(SegmentError::LimitExceeded);
        }
        if &bytes[..8] != MAGIC {
            return Err(SegmentError::InvalidHeader);
        }
        let version = u16_at(bytes, 8)?;
        if version != VERSION {
            return Err(SegmentError::UnsupportedVersion(version));
        }
        // A delta of another layout belongs to a different feed.
        if bytes[10] as usize != K::WIDTH || bytes[11] as usize != V::WIDTH {
            return Err(SegmentError::InvalidHeader);
        }
        if bytes[68..72] != [0; 4] {
            return Err(SegmentError::NonCanonical);
        }
        let count = u32_at(bytes, 12)? as usize;
        if count == 0 {
            return Err(SegmentError::NonCanonical);
        }
        if encoded_delta_len::<K, V>(count)? != bytes.len() {
            return Err(SegmentError::InvalidMetadata);
        }
        if crc(bytes, &mut keep)? != u32_at(bytes, 64)? {
            return Err(SegmentError::ChecksumMismatch);
        }
        let mut stream = [0; 32];
        stream.copy_from_slice(&bytes[16..48]);
        let binding = Binding {
            stream,
            epoch: u64::from_le_bytes(
                bytes[48..56]
                    .try_into()
                    .map_err(|_| SegmentError::Truncated)?,
            ),
        };
        let base_revision = u64::from_le_bytes(
            bytes[56..64]
                .try_into()
                .map_err(|_| SegmentError::Truncated)?,
        );
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(count)
            .map_err(|_| SegmentError::AllocationFailed)?;
        for index in 0..count {
            if index % 1024 == 0 {
                poll(&mut keep, SegmentError::Cancelled)?;
            }
            let offset = DELTA_HEADER + index * stride::<K, V>();
            let tag = bytes[offset + K::WIDTH];
            let read = |code: u8, at: usize| -> Result<Option<Option<V>>, SegmentError> {
                let value: V = int_at(bytes, at)?;
                match code {
                    0 | 1 if value != V::default() => Err(SegmentError::NonCanonical),
                    0 => Ok(None),
                    1 => Ok(Some(None)),
                    2 => Ok(Some(Some(value))),
                    _ => Err(SegmentError::NonCanonical),
                }
            };
            if tag >> 4 != 0 {
                return Err(SegmentError::NonCanonical);
            }
            entries.push(DeltaEntry {
                key: int_at(bytes, offset)?,
                base: read(tag & 3, offset + K::WIDTH + 1)?,
                current: read(tag >> 2, offset + K::WIDTH + 1 + V::WIDTH)?,
            });
        }
        check(binding, &entries)?;
        Ok(Self {
            binding,
            base_revision,
            entries,
        })
    }
}

fn check<K: Int, V: Int>(
    binding: Binding,
    entries: &[DeltaEntry<K, V>],
) -> Result<(), SegmentError> {
    if binding.epoch == 0 || binding.epoch > i64::MAX as u64 {
        return Err(SegmentError::InvalidMetadata);
    }
    for (index, e) in entries.iter().enumerate() {
        if e.base == e.current || (index > 0 && entries[index - 1].key >= e.key) {
            return Err(SegmentError::NonCanonical);
        }
    }
    Ok(())
}

/// A multiset of values to skip while reading a base segment.
pub struct Removals<V> {
    counts: Vec<(Option<V>, u32)>,
    left: usize,
}
impl<V: Int> Removals<V> {
    fn new(values: impl Iterator<Item = Option<V>>) -> Self {
        let mut all: Vec<Option<V>> = values.collect();
        all.sort_unstable();
        let left = all.len();
        let mut counts: Vec<(Option<V>, u32)> = Vec::new();
        for value in all {
            match counts.last_mut() {
                Some((last, n)) if *last == value => *n += 1,
                _ => counts.push((value, 1)),
            }
        }
        Self { counts, left }
    }
    /// True when `value` is one of the removed occurrences; that occurrence is used up.
    pub fn take(&mut self, value: Option<V>) -> bool {
        if self.left == 0 {
            return false;
        }
        match self.counts.binary_search_by(|(v, _)| v.cmp(&value)) {
            Ok(i) if self.counts[i].1 > 0 => {
                self.counts[i].1 -= 1;
                self.left -= 1;
                true
            }
            _ => false,
        }
    }
    /// False when some removed value never appeared in the base: the delta and the
    /// base disagree, and the result must not be used.
    pub fn finished(&self) -> bool {
        self.left == 0
    }
}
