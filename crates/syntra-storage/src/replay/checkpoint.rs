//! Versioned keyed state. Bounds and CRC are checked before row allocation.
//! Version 1 is `int4` keys and values. Version 2 records the key and value
//! widths at offset 108, which version 1 requires to be zero.
use super::{Binding, Limits, ReplayState, Row};
use crate::{int_at, u16_at, u32_at, Int, SegmentError, MAX_ROWS, MAX_SEGMENT_BYTES};
use std::mem::size_of;

const HEADER: usize = 112;
const MAGIC: &[u8; 8] = b"SYNTRACP";
fn poll(keep: &mut impl FnMut() -> bool) -> Result<(), SegmentError> {
    if keep() {
        Ok(())
    } else {
        Err(SegmentError::Cancelled)
    }
}
fn u64_at(bytes: &[u8], offset: usize) -> Result<u64, SegmentError> {
    let value = bytes
        .get(offset..offset + 8)
        .ok_or(SegmentError::Truncated)?;
    Ok(u64::from_le_bytes(
        value.try_into().map_err(|_| SegmentError::Truncated)?,
    ))
}
fn checksum(bytes: &[u8], keep: &mut impl FnMut() -> bool) -> Result<u32, SegmentError> {
    let mut crc = crc32fast::Hasher::new();
    for slice in [&bytes[..104], &bytes[108..]] {
        for chunk in slice.chunks(65536) {
            poll(keep)?;
            crc.update(chunk);
        }
    }
    Ok(crc.finalize())
}
fn version<K: Int, V: Int>() -> u16 {
    if K::WIDTH == 4 && V::WIDTH == 4 {
        1
    } else {
        2
    }
}
fn length<K: Int, V: Int>(rows: usize) -> Result<(usize, usize), SegmentError> {
    if rows > MAX_ROWS {
        return Err(SegmentError::LimitExceeded);
    }
    let bitmap = rows.div_ceil(8);
    let total = rows
        .checked_mul(K::WIDTH + V::WIDTH)
        .and_then(|n| n.checked_add(bitmap + HEADER))
        .ok_or(SegmentError::LimitExceeded)?;
    if total > MAX_SEGMENT_BYTES {
        return Err(SegmentError::LimitExceeded);
    }
    Ok((bitmap, total))
}
impl<K: Int, V: Int> ReplayState<K, V> {
    /// Exact encoded checkpoint length for `rows`, independent of keys and values.
    pub fn encoded_checkpoint_len(rows: usize) -> Result<usize, SegmentError> {
        length::<K, V>(rows).map(|(_, total)| total)
    }
    pub fn encode_checkpoint(
        &self,
        mut keep: impl FnMut() -> bool,
    ) -> Result<Vec<u8>, SegmentError> {
        poll(&mut keep)?;
        let (bitmap, total) = length::<K, V>(self.rows.len())?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(total)
            .map_err(|_| SegmentError::AllocationFailed)?;
        bytes.resize(total, 0);
        bytes[..8].copy_from_slice(MAGIC);
        bytes[8..10].copy_from_slice(&version::<K, V>().to_le_bytes());
        bytes[10..12].copy_from_slice(&u16::from(self.last_digest.is_some()).to_le_bytes());
        bytes[12..16].copy_from_slice(&(self.rows.len() as u32).to_le_bytes());
        bytes[16..48].copy_from_slice(&self.binding.stream);
        bytes[48..56].copy_from_slice(&self.binding.epoch.to_le_bytes());
        bytes[56..64].copy_from_slice(&self.sequence.to_le_bytes());
        if let Some(digest) = self.last_digest {
            bytes[64..96].copy_from_slice(&digest);
        }
        bytes[96..104].copy_from_slice(&((total - HEADER) as u64).to_le_bytes());
        if version::<K, V>() == 2 {
            bytes[108] = K::WIDTH as u8;
            bytes[109] = V::WIDTH as u8;
        }
        let stride = K::WIDTH + V::WIDTH;
        for (index, row) in self.rows.iter().enumerate() {
            if index % 1024 == 0 {
                poll(&mut keep)?;
            }
            let offset = HEADER + bitmap + index * stride;
            row.key.put(&mut bytes[offset..]);
            if let Some(value) = row.value {
                value.put(&mut bytes[offset + K::WIDTH..]);
            } else {
                bytes[HEADER + index / 8] |= 1 << (index % 8);
            }
        }
        let crc = checksum(&bytes, &mut keep)?;
        bytes[104..108].copy_from_slice(&crc.to_le_bytes());
        Ok(bytes)
    }
    pub fn decode_checkpoint(
        bytes: &[u8],
        limits: Limits,
        mut keep: impl FnMut() -> bool,
    ) -> Result<Self, SegmentError> {
        poll(&mut keep)?;
        if bytes.len() < HEADER {
            return Err(SegmentError::Truncated);
        }
        if bytes.len() > MAX_SEGMENT_BYTES {
            return Err(SegmentError::LimitExceeded);
        }
        if &bytes[..8] != MAGIC {
            return Err(SegmentError::InvalidHeader);
        }
        let version = u16_at(bytes, 8)?;
        if version != 1 && version != 2 {
            return Err(SegmentError::UnsupportedVersion(version));
        }
        // A state of another layout is a different feed, never a readable one.
        if version != self::version::<K, V>() {
            return Err(SegmentError::InvalidHeader);
        }
        let widths = if version == 1 {
            [0, 0]
        } else {
            [K::WIDTH as u8, V::WIDTH as u8]
        };
        if version == 2 && bytes[108..110] != widths {
            return Err(SegmentError::InvalidHeader);
        }
        let flags = u16_at(bytes, 10)?;
        if flags > 1 || bytes[108..110] != widths || bytes[110..112] != [0; 2] {
            return Err(SegmentError::NonCanonical);
        }
        let count = u32_at(bytes, 12)? as usize;
        let (bitmap, total) = length::<K, V>(count)?;
        if total != bytes.len() || u64_at(bytes, 96)? != (total - HEADER) as u64 {
            return Err(SegmentError::InvalidMetadata);
        }
        if checksum(bytes, &mut keep)? != u32_at(bytes, 104)? {
            return Err(SegmentError::ChecksumMismatch);
        }
        if limits.rows > MAX_ROWS || !(1..=4096).contains(&limits.changes) {
            return Err(SegmentError::InvalidMetadata);
        }
        if count > limits.rows
            || count
                .checked_mul(size_of::<Row<K, V>>())
                .is_none_or(|n| n > limits.buffer_bytes)
        {
            return Err(SegmentError::LimitExceeded);
        }
        let epoch = u64_at(bytes, 48)?;
        if epoch == 0 {
            return Err(SegmentError::InvalidMetadata);
        }
        if flags == 0 && bytes[64..96].iter().any(|b| *b != 0) {
            return Err(SegmentError::NonCanonical);
        }
        if !count.is_multiple_of(8) && bytes[HEADER + bitmap - 1] >> (count % 8) != 0 {
            return Err(SegmentError::NonCanonical);
        }
        let mut rows = Vec::new();
        rows.try_reserve_exact(count)
            .map_err(|_| SegmentError::AllocationFailed)?;
        for index in 0..count {
            if index % 1024 == 0 {
                poll(&mut keep)?;
            }
            let offset = HEADER + bitmap + index * (K::WIDTH + V::WIDTH);
            let key: K = int_at(bytes, offset)?;
            let value: V = int_at(bytes, offset + K::WIDTH)?;
            if rows.last().is_some_and(|row: &Row<K, V>| row.key >= key) {
                return Err(SegmentError::NonCanonical);
            }
            let null = bytes[HEADER + index / 8] & (1 << (index % 8)) != 0;
            if null && value != V::default() {
                return Err(SegmentError::NonCanonical);
            }
            rows.push(Row {
                key,
                value: (!null).then_some(value),
            });
        }
        let mut stream = [0; 32];
        stream.copy_from_slice(&bytes[16..48]);
        let last_digest = if flags == 1 {
            let mut value = [0; 32];
            value.copy_from_slice(&bytes[64..96]);
            Some(value)
        } else {
            None
        };
        poll(&mut keep)?;
        Ok(Self {
            binding: Binding { stream, epoch },
            sequence: u64_at(bytes, 56)?,
            rows,
            last_digest,
            limits,
        })
    }
}
