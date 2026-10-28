//! Versioned manifest for 64 MVCC-backed keyed/value partitions. Version 1 has
//! no partition deltas; version 2 adds a delta reference to each partition and is
//! written only while some partition has a delta.
use crate::delta::{DELTA_HEADER, MAX_DELTA_BYTES};
use crate::replay::Binding;
use crate::{u16_at, u32_at, Int, SegmentError, MAX_ROWS, MAX_SEGMENT_BYTES};
use sha2::{Digest, Sha256};
pub const PARTITIONS: usize = 64;
pub const MANIFEST_BYTES: usize = 80 + PARTITIONS * 80;
pub const MANIFEST_V2_BYTES: usize = 80 + PARTITIONS * 128;
/// Most entries any layout fits in `MAX_DELTA_BYTES`: int4 entries are 13 bytes.
const MAX_DELTA_ENTRIES: u32 = ((MAX_DELTA_BYTES - DELTA_HEADER) / 13) as u32;
/// `revision` and `rows` describe the partition now, as its catalog row does.
/// Without a delta they are also the base state's revision and row count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ChunkRef {
    pub revision: u64,
    pub rows: u32,
    pub state_digest: [u8; 32],
    pub segment_digest: [u8; 32],
    pub delta: Option<DeltaRef>,
}
/// The base a delta applies to, and the delta's own size and digest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeltaRef {
    pub base_revision: u64,
    pub base_rows: u32,
    pub entries: u32,
    pub digest: [u8; 32],
}
impl ChunkRef {
    pub fn base_revision(&self) -> u64 {
        self.delta.map_or(self.revision, |d| d.base_revision)
    }
    pub fn base_rows(&self) -> u32 {
        self.delta.map_or(self.rows, |d| d.base_rows)
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub binding: Binding,
    pub applied: u64,
    pub generation: u64,
    pub chunks: [ChunkRef; PARTITIONS],
}
/// Frozen v1 routing: Murmur3's 32-bit finalizer, then the low six bits.
pub fn partition(key: i32) -> usize {
    key.partition()
}
/// Routing for a key of either width; `int4` keys route exactly as `partition`.
pub fn partition_of<K: Int>(key: K) -> usize {
    key.partition()
}
pub fn chunk_binding(binding: Binding, partition: usize) -> Result<Binding, SegmentError> {
    if partition >= PARTITIONS {
        return Err(SegmentError::InvalidMetadata);
    }
    let mut identity = Vec::from(&b"syntra-chunk-binding-v1\0"[..]);
    identity.extend_from_slice(&binding.stream);
    identity.extend_from_slice(&(partition as u16).to_le_bytes());
    Ok(Binding::derive(&identity, binding.epoch))
}
pub fn digest(bytes: &[u8], mut keep: impl FnMut() -> bool) -> Result<[u8; 32], SegmentError> {
    if bytes.len() > MAX_SEGMENT_BYTES {
        return Err(SegmentError::LimitExceeded);
    }
    let mut hash = Sha256::new();
    hash.update(b"syntra-chunk-bytes-v1\0");
    hash.update((bytes.len() as u64).to_le_bytes());
    for chunk in bytes.chunks(65536) {
        if !keep() {
            return Err(SegmentError::Cancelled);
        }
        hash.update(chunk);
    }
    if !keep() {
        return Err(SegmentError::Cancelled);
    }
    Ok(hash.finalize().into())
}
fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    // Only called after validating the fixed manifest length.
    let mut value = [0; 8];
    value.copy_from_slice(&bytes[offset..offset + 8]);
    u64::from_le_bytes(value)
}
fn crc(bytes: &[u8]) -> u32 {
    let mut hash = crc32fast::Hasher::new();
    hash.update(&bytes[..72]);
    hash.update(&bytes[76..]);
    hash.finalize()
}
impl Manifest {
    pub fn rows(&self) -> Result<u32, SegmentError> {
        let rows = self
            .chunks
            .iter()
            .try_fold(0_u32, |sum, c| sum.checked_add(c.rows))
            .ok_or(SegmentError::LimitExceeded)?;
        if rows as usize > MAX_ROWS {
            return Err(SegmentError::LimitExceeded);
        }
        Ok(rows)
    }
    fn validate(&self) -> Result<(), SegmentError> {
        self.rows()?;
        for c in &self.chunks {
            if let Some(d) = c.delta {
                if d.base_revision >= c.revision
                    || d.entries == 0
                    || d.entries > MAX_DELTA_ENTRIES
                    || d.base_rows as usize > MAX_ROWS
                {
                    return Err(SegmentError::InvalidMetadata);
                }
            }
        }
        if self.binding.epoch == 0
            || self.binding.epoch > i64::MAX as u64
            || self.generation == 0
            || self.generation > i64::MAX as u64
            || self.applied > i64::MAX as u64
            || self
                .chunks
                .iter()
                .any(|c| c.revision > i64::MAX as u64 || c.revision >= self.generation)
        {
            return Err(SegmentError::InvalidMetadata);
        }
        Ok(())
    }
    fn version(&self) -> u16 {
        if self.chunks.iter().any(|c| c.delta.is_some()) {
            2
        } else {
            1
        }
    }
    pub fn encode(&self) -> Result<Vec<u8>, SegmentError> {
        self.validate()?;
        let version = self.version();
        let stride = if version == 1 { 80 } else { 128 };
        let mut bytes = vec![0; 80 + PARTITIONS * stride];
        bytes[..8].copy_from_slice(b"SYNTRACM");
        bytes[8..10].copy_from_slice(&version.to_le_bytes());
        bytes[10..12].copy_from_slice(&(PARTITIONS as u16).to_le_bytes());
        bytes[12..16].copy_from_slice(&self.rows()?.to_le_bytes());
        bytes[16..48].copy_from_slice(&self.binding.stream);
        bytes[48..56].copy_from_slice(&self.binding.epoch.to_le_bytes());
        bytes[56..64].copy_from_slice(&self.applied.to_le_bytes());
        bytes[64..72].copy_from_slice(&self.generation.to_le_bytes());
        for (index, c) in self.chunks.iter().enumerate() {
            let offset = 80 + index * stride;
            bytes[offset..offset + 8].copy_from_slice(&c.revision.to_le_bytes());
            bytes[offset + 8..offset + 12].copy_from_slice(&c.rows.to_le_bytes());
            bytes[offset + 16..offset + 48].copy_from_slice(&c.state_digest);
            bytes[offset + 48..offset + 80].copy_from_slice(&c.segment_digest);
            if version == 2 {
                // Base row count in v1's reserved slot, then base revision, entry
                // count, four reserved bytes and the delta digest.
                bytes[offset + 12..offset + 16].copy_from_slice(&c.base_rows().to_le_bytes());
                bytes[offset + 80..offset + 88].copy_from_slice(&c.base_revision().to_le_bytes());
                if let Some(d) = c.delta {
                    bytes[offset + 88..offset + 92].copy_from_slice(&d.entries.to_le_bytes());
                    bytes[offset + 96..offset + 128].copy_from_slice(&d.digest);
                }
            }
        }
        let checksum = crc(&bytes);
        bytes[72..76].copy_from_slice(&checksum.to_le_bytes());
        Ok(bytes)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, SegmentError> {
        if bytes.len() != MANIFEST_BYTES && bytes.len() != MANIFEST_V2_BYTES {
            return Err(SegmentError::InvalidMetadata);
        }
        if &bytes[..8] != b"SYNTRACM" {
            return Err(SegmentError::InvalidHeader);
        }
        let version = u16_at(bytes, 8)?;
        if version != 1 && version != 2 {
            return Err(SegmentError::UnsupportedVersion(version));
        }
        let stride = if version == 1 { 80 } else { 128 };
        if bytes.len() != 80 + PARTITIONS * stride {
            return Err(SegmentError::InvalidMetadata);
        }
        if u16_at(bytes, 10)? as usize != PARTITIONS || bytes[76..80] != [0; 4] {
            return Err(SegmentError::NonCanonical);
        }
        if crc(bytes) != u32_at(bytes, 72)? {
            return Err(SegmentError::ChecksumMismatch);
        }
        let mut stream = [0; 32];
        stream.copy_from_slice(&bytes[16..48]);
        let mut chunks = [ChunkRef::default(); PARTITIONS];
        for (index, c) in chunks.iter_mut().enumerate() {
            let offset = 80 + index * stride;
            c.revision = u64_at(bytes, offset);
            c.rows = u32_at(bytes, offset + 8)?;
            c.state_digest
                .copy_from_slice(&bytes[offset + 16..offset + 48]);
            c.segment_digest
                .copy_from_slice(&bytes[offset + 48..offset + 80]);
            if version == 1 {
                if bytes[offset + 12..offset + 16] != [0; 4] {
                    return Err(SegmentError::NonCanonical);
                }
                continue;
            }
            let base_rows = u32_at(bytes, offset + 12)?;
            let base_revision = u64_at(bytes, offset + 80);
            let entries = u32_at(bytes, offset + 88)?;
            let mut digest = [0; 32];
            digest.copy_from_slice(&bytes[offset + 96..offset + 128]);
            if bytes[offset + 92..offset + 96] != [0; 4] {
                return Err(SegmentError::NonCanonical);
            }
            if entries == 0 {
                // No delta: the base is the partition, and nothing else is recorded.
                if base_rows != c.rows || base_revision != c.revision || digest != [0; 32] {
                    return Err(SegmentError::NonCanonical);
                }
            } else {
                c.delta = Some(DeltaRef {
                    base_revision,
                    base_rows,
                    entries,
                    digest,
                });
            }
        }
        let manifest = Self {
            binding: Binding {
                stream,
                epoch: u64_at(bytes, 48),
            },
            applied: u64_at(bytes, 56),
            generation: u64_at(bytes, 64),
            chunks,
        };
        manifest.validate()?;
        // Version 2 is only written while some partition has a delta.
        if manifest.version() != version {
            return Err(SegmentError::NonCanonical);
        }
        if manifest.rows() != Ok(u32_at(bytes, 12)?) {
            return Err(SegmentError::InvalidMetadata);
        }
        Ok(manifest)
    }
}
