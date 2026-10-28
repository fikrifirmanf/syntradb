//! Bounded keyed-integer replay kernel. The coordinator must supply a complete,
//! snapshot-correct stream; this module neither captures nor authorizes PG data.
//! Keys and values are `int4` or `int8` (see `Int`); `int4` feeds keep v1 bytes.
use crate::Int;
use sha2::{Digest, Sha256};
use std::{fmt, mem::size_of};
mod checkpoint;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    pub stream: [u8; 32],
    pub epoch: u64,
}
impl Binding {
    pub fn derive(identity: &[u8], epoch: u64) -> Self {
        let mut hash = Sha256::new();
        hash.update(b"syntra-replay-identity-v1\0");
        hash.update((identity.len() as u64).to_le_bytes());
        hash.update(identity);
        Self {
            stream: hash.finalize().into(),
            epoch,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row<K = i32, V = i32> {
    pub key: K,
    pub value: Option<V>,
}
/// Outer None means absent; Some(None) means a present SQL NULL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Change<K = i32, V = i32> {
    pub key: K,
    pub before: Option<Option<V>>,
    pub after: Option<Option<V>>,
}
pub struct Batch<'a, K = i32, V = i32> {
    pub binding: Binding,
    pub previous: u64,
    pub next: u64,
    pub changes: &'a [Change<K, V>],
}
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub rows: usize,
    pub changes: usize,
    pub buffer_bytes: usize,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayError {
    InvalidLimits,
    InvalidSeed,
    WrongBinding,
    SequenceGap,
    ConflictingRetry,
    BeforeImageMismatch(i64),
    InvalidChange,
    RowLimit,
    ChangeLimit,
    BufferLimit,
    AllocationFailed,
    Cancelled,
}
impl fmt::Display for ReplayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "keyed replay failed: {self:?}")
    }
}
impl std::error::Error for ReplayError {}
type Result<T> = std::result::Result<T, ReplayError>;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applied {
    New,
    Duplicate,
}
#[derive(Clone, Copy)]
struct Edit<K, V> {
    key: K,
    after: Option<Option<V>>,
}

pub struct ReplayState<K = i32, V = i32> {
    binding: Binding,
    sequence: u64,
    rows: Vec<Row<K, V>>,
    last_digest: Option<[u8; 32]>,
    limits: Limits,
}

fn poll(keep_running: &mut impl FnMut() -> bool) -> Result<()> {
    if keep_running() {
        Ok(())
    } else {
        Err(ReplayError::Cancelled)
    }
}
fn bytes(parts: &[(usize, usize)], limit: usize) -> Result<()> {
    let total = parts
        .iter()
        .try_fold(0usize, |sum, (count, width)| {
            count.checked_mul(*width).and_then(|n| sum.checked_add(n))
        })
        .ok_or(ReplayError::BufferLimit)?;
    if total > limit {
        Err(ReplayError::BufferLimit)
    } else {
        Ok(())
    }
}
fn hash_int<V: Int>(hash: &mut Sha256, value: V) {
    let mut bytes = [0; 8];
    value.put(&mut bytes);
    hash.update(&bytes[..V::WIDTH]);
}
fn hash_value<V: Int>(hash: &mut Sha256, value: Option<Option<V>>) {
    match value {
        None => hash.update([0]),
        Some(None) => hash.update([1]),
        Some(Some(value)) => {
            hash.update([2]);
            hash_int(hash, value);
        }
    }
}

impl<K: Int, V: Int> ReplayState<K, V> {
    /// Input rows are borrowed and outside the requested owned-buffer budget.
    /// Seed keys must be strictly increasing. Allocator metadata is not counted.
    pub fn seed(
        binding: Binding,
        sequence: u64,
        rows: &[Row<K, V>],
        limits: Limits,
        mut keep_running: impl FnMut() -> bool,
    ) -> Result<Self> {
        if binding.epoch == 0 || limits.rows > 1_000_000 || !(1..=4096).contains(&limits.changes) {
            return Err(ReplayError::InvalidLimits);
        }
        if rows.len() > limits.rows {
            return Err(ReplayError::RowLimit);
        }
        bytes(&[(rows.len(), size_of::<Row<K, V>>())], limits.buffer_bytes)?;
        poll(&mut keep_running)?;
        for (index, row) in rows.iter().enumerate() {
            if index % 1024 == 0 {
                poll(&mut keep_running)?;
            }
            if index > 0 && rows[index - 1].key >= row.key {
                return Err(ReplayError::InvalidSeed);
            }
        }
        let mut owned = Vec::new();
        owned
            .try_reserve_exact(rows.len())
            .map_err(|_| ReplayError::AllocationFailed)?;
        owned.extend_from_slice(rows);
        Ok(Self {
            binding,
            sequence,
            rows: owned,
            last_digest: None,
            limits,
        })
    }
    pub fn rows(&self) -> &[Row<K, V>] {
        &self.rows
    }
    pub fn sequence(&self) -> u64 {
        self.sequence
    }
    pub fn binding(&self) -> Binding {
        self.binding
    }

    /// No state or progress changes on error. Only an exact retry of the last
    /// applied batch is accepted as a duplicate; older retries are rejected.
    pub fn apply(
        &mut self,
        batch: Batch<'_, K, V>,
        keep_running: impl FnMut() -> bool,
    ) -> Result<Applied> {
        self.apply_advance(batch, 1, keep_running)
    }

    /// Each change consumes one contiguous event ordinal. Empty event ranges
    /// are rejected; ordinary apply() retains its one-sequence-per-batch contract.
    pub fn apply_events(
        &mut self,
        batch: Batch<'_, K, V>,
        keep_running: impl FnMut() -> bool,
    ) -> Result<Applied> {
        if batch.changes.is_empty() {
            return Err(ReplayError::InvalidChange);
        }
        let advance = batch.changes.len() as u64;
        self.apply_advance(batch, advance, keep_running)
    }

    fn apply_advance(
        &mut self,
        batch: Batch<'_, K, V>,
        advance: u64,
        mut keep_running: impl FnMut() -> bool,
    ) -> Result<Applied> {
        poll(&mut keep_running)?;
        if batch.binding != self.binding {
            return Err(ReplayError::WrongBinding);
        }
        if batch.changes.len() > self.limits.changes {
            return Err(ReplayError::ChangeLimit);
        }
        if batch.previous.checked_add(advance) != Some(batch.next) {
            return Err(ReplayError::SequenceGap);
        }
        let mut hash = Sha256::new();
        // The same domain for every width: a feed's binding fixes its layout, and
        // int4 batches must hash exactly as before so retries survive upgrades.
        hash.update(b"syntra-keyed-replay-v1\0");
        hash.update(batch.binding.stream);
        hash.update(batch.binding.epoch.to_le_bytes());
        hash.update(batch.previous.to_le_bytes());
        hash.update(batch.next.to_le_bytes());
        hash.update((batch.changes.len() as u64).to_le_bytes());
        for (index, change) in batch.changes.iter().enumerate() {
            if index % 64 == 0 {
                poll(&mut keep_running)?;
            }
            hash_int(&mut hash, change.key);
            hash_value(&mut hash, change.before);
            hash_value(&mut hash, change.after);
        }
        let digest: [u8; 32] = hash.finalize().into();
        if batch.next == self.sequence {
            return if self.last_digest == Some(digest) {
                Ok(Applied::Duplicate)
            } else {
                Err(ReplayError::ConflictingRetry)
            };
        }
        if batch.previous != self.sequence {
            return Err(ReplayError::SequenceGap);
        }
        if batch.changes.is_empty() {
            poll(&mut keep_running)?;
            self.sequence = batch.next;
            self.last_digest = Some(digest);
            return Ok(Applied::New);
        }
        let resident = self.rows.capacity();
        let count = batch.changes.len();
        bytes(
            &[
                (resident, size_of::<Row<K, V>>()),
                (count, size_of::<usize>()),
                (count, size_of::<Edit<K, V>>()),
            ],
            self.limits.buffer_bytes,
        )?;
        let mut order = Vec::new();
        order
            .try_reserve_exact(count)
            .map_err(|_| ReplayError::AllocationFailed)?;
        order.extend(0..count);
        // Preserve event order per key while processing independent keys together.
        // The sort has at most 4096 elements and allocates no further buffers.
        order.sort_unstable_by_key(|index| (batch.changes[*index].key, *index));
        poll(&mut keep_running)?;
        let mut edits = Vec::new();
        edits
            .try_reserve_exact(count)
            .map_err(|_| ReplayError::AllocationFailed)?;
        let mut position = 0;
        let mut final_rows = self.rows.len();
        while position < count {
            let key = batch.changes[order[position]].key;
            let old = self
                .rows
                .binary_search_by_key(&key, |row| row.key)
                .ok()
                .map(|i| self.rows[i].value);
            let mut value = old;
            while position < count && batch.changes[order[position]].key == key {
                if position % 64 == 0 {
                    poll(&mut keep_running)?;
                }
                let change = batch.changes[order[position]];
                if change.before.is_none() && change.after.is_none() {
                    return Err(ReplayError::InvalidChange);
                }
                if change.before != value {
                    return Err(ReplayError::BeforeImageMismatch(key.to_i64()));
                }
                value = change.after;
                position += 1;
            }
            match (old.is_some(), value.is_some()) {
                (false, true) => final_rows += 1,
                (true, false) => final_rows -= 1,
                _ => {}
            }
            edits.push(Edit { key, after: value });
        }
        if final_rows > self.limits.rows {
            return Err(ReplayError::RowLimit);
        }
        bytes(
            &[
                (resident, size_of::<Row<K, V>>()),
                (order.capacity(), size_of::<usize>()),
                (edits.capacity(), size_of::<Edit<K, V>>()),
                (final_rows, size_of::<Row<K, V>>()),
            ],
            self.limits.buffer_bytes,
        )?;
        let mut output = Vec::new();
        output
            .try_reserve_exact(final_rows)
            .map_err(|_| ReplayError::AllocationFailed)?;
        let (mut base, mut edit, mut examined) = (0, 0, 0);
        while base < self.rows.len() || edit < edits.len() {
            if examined % 1024 == 0 {
                poll(&mut keep_running)?;
            }
            examined += 1;
            let take_base = base < self.rows.len()
                && (edit == edits.len() || self.rows[base].key < edits[edit].key);
            if take_base {
                output.push(self.rows[base]);
                base += 1;
            } else {
                let change = edits[edit];
                if base < self.rows.len() && self.rows[base].key == change.key {
                    base += 1;
                }
                if let Some(value) = change.after {
                    output.push(Row {
                        key: change.key,
                        value,
                    });
                }
                edit += 1;
            }
        }
        debug_assert_eq!(output.len(), final_rows);
        poll(&mut keep_running)?;
        // No fallible work follows this publication point.
        self.rows = output;
        self.sequence = batch.next;
        self.last_digest = Some(digest);
        Ok(Applied::New)
    }
}
