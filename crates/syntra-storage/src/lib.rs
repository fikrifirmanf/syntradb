//! Experimental immutable nullable-integer segments. See docs/segment-format.md.
//! Readers borrow the encoded bytes and allocate no row-sized buffers.
#![forbid(unsafe_code)]

use std::fmt;
pub mod chunks;
pub mod delta;
pub mod replay;

pub const GROUP_ROWS: usize = 1024;
pub const MAX_ROWS: usize = 1_000_000;
pub const MAX_SEGMENT_BYTES: usize = 8 * 1024 * 1024;
const HEADER_BYTES: usize = 40;
const MAGIC: &[u8; 8] = b"SYNTRAS1";
const VERSION: u16 = 1;

mod sealed {
    pub trait Sealed {}
    impl Sealed for i32 {}
    impl Sealed for i64 {}
}

/// A fixed-width SQL integer: `int4` (`i32`) or `int8` (`i64`). Every encoding
/// writes it little-endian at its own width, so `int4` bytes are unchanged.
pub trait Int: sealed::Sealed + Copy + Ord + Default + fmt::Debug + Send + Sync + 'static {
    const WIDTH: usize;
    /// Segment header type code: 1 for nullable `int4`, 2 for nullable `int8`.
    const SEGMENT_TYPE: u16;
    /// Writes `WIDTH` little-endian bytes to the start of `out`.
    fn put(self, out: &mut [u8]);
    /// Reads `WIDTH` little-endian bytes from the start of `bytes`.
    fn get(bytes: &[u8]) -> Self;
    /// Frozen partition routing for keys of this width.
    fn partition(self) -> usize;
    fn to_i64(self) -> i64;
}

impl Int for i32 {
    const WIDTH: usize = 4;
    const SEGMENT_TYPE: u16 = 1;
    fn put(self, out: &mut [u8]) {
        out[..4].copy_from_slice(&self.to_le_bytes());
    }
    fn get(bytes: &[u8]) -> Self {
        let mut value = [0; 4];
        value.copy_from_slice(&bytes[..4]);
        i32::from_le_bytes(value)
    }
    /// Murmur3's 32-bit finalizer, then the low six bits.
    fn partition(self) -> usize {
        let mut x = self as u32;
        x ^= x >> 16;
        x = x.wrapping_mul(0x85ebca6b);
        x ^= x >> 13;
        x = x.wrapping_mul(0xc2b2ae35);
        x ^= x >> 16;
        (x & 63) as usize
    }
    fn to_i64(self) -> i64 {
        i64::from(self)
    }
}

impl Int for i64 {
    const WIDTH: usize = 8;
    const SEGMENT_TYPE: u16 = 2;
    fn put(self, out: &mut [u8]) {
        out[..8].copy_from_slice(&self.to_le_bytes());
    }
    fn get(bytes: &[u8]) -> Self {
        let mut value = [0; 8];
        value.copy_from_slice(&bytes[..8]);
        i64::from_le_bytes(value)
    }
    /// Murmur3's 64-bit finalizer, then the low six bits.
    fn partition(self) -> usize {
        let mut x = self as u64;
        x ^= x >> 33;
        x = x.wrapping_mul(0xff51afd7ed558ccd);
        x ^= x >> 33;
        x = x.wrapping_mul(0xc4ceb9fe1a85ec53);
        x ^= x >> 33;
        (x & 63) as usize
    }
    fn to_i64(self) -> i64 {
        self
    }
}

/// Rows, nulls, minimum, maximum, bitmap length and CRC.
const fn group_header_bytes(width: usize) -> usize {
    16 + 2 * width
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentError {
    InvalidHeader,
    UnsupportedVersion(u16),
    Truncated,
    ChecksumMismatch,
    InvalidMetadata,
    NonCanonical,
    LimitExceeded,
    AllocationFailed,
    Cancelled,
}

impl fmt::Display for SegmentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVersion(version) => write!(f, "unsupported segment version {version}"),
            error => f.write_str(match error {
                Self::InvalidHeader => "invalid int4 segment header",
                Self::Truncated => "truncated int4 segment",
                Self::ChecksumMismatch => "segment checksum mismatch",
                Self::InvalidMetadata => "segment metadata does not match its values",
                Self::NonCanonical => "noncanonical segment encoding",
                Self::LimitExceeded => "segment exceeds the prototype row or byte limit",
                Self::AllocationFailed => "cannot allocate segment output",
                Self::Cancelled => "segment operation cancelled",
                Self::UnsupportedVersion(_) => unreachable!(),
            }),
        }
    }
}

impl std::error::Error for SegmentError {}

fn poll(keep_running: &mut impl FnMut() -> bool) -> Result<(), SegmentError> {
    if keep_running() {
        Ok(())
    } else {
        Err(SegmentError::Cancelled)
    }
}

fn u16_at(bytes: &[u8], offset: usize) -> Result<u16, SegmentError> {
    let data = bytes
        .get(offset..offset + 2)
        .ok_or(SegmentError::Truncated)?;
    Ok(u16::from_le_bytes([data[0], data[1]]))
}

fn u32_at(bytes: &[u8], offset: usize) -> Result<u32, SegmentError> {
    let data = bytes
        .get(offset..offset + 4)
        .ok_or(SegmentError::Truncated)?;
    Ok(u32::from_le_bytes([data[0], data[1], data[2], data[3]]))
}

fn int_at<V: Int>(bytes: &[u8], offset: usize) -> Result<V, SegmentError> {
    bytes
        .get(offset..offset + V::WIDTH)
        .map(V::get)
        .ok_or(SegmentError::Truncated)
}

pub fn encoded_int4_len(rows: usize) -> Result<usize, SegmentError> {
    encoded_len::<i32>(rows)
}

/// Exact encoded length for `rows`. Values and NULLs never change the layout, so
/// callers can reject stored bytes of the wrong size before reading them.
pub fn encoded_len<V: Int>(rows: usize) -> Result<usize, SegmentError> {
    if rows > MAX_ROWS {
        return Err(SegmentError::LimitExceeded);
    }
    let group = |n: usize| group_header_bytes(V::WIDTH) + n.div_ceil(8) + n * V::WIDTH;
    let rest = rows % GROUP_ROWS;
    let total = HEADER_BYTES
        + (rows / GROUP_ROWS) * group(GROUP_ROWS)
        + if rest == 0 { 0 } else { group(rest) };
    if total > MAX_SEGMENT_BYTES {
        return Err(SegmentError::LimitExceeded);
    }
    Ok(total)
}

/// Encodes one typed column. Output is returned only after all input succeeds.
/// Output capacity is bounded; the only staging buffer holds one row group.
pub fn encode_int4(
    input: impl IntoIterator<Item = Option<i32>>,
    keep_running: impl FnMut() -> bool,
) -> Result<Vec<u8>, SegmentError> {
    encode_int4_with_row_count(input, keep_running).map(|(bytes, _rows)| bytes)
}

pub fn encode_int4_with_row_count(
    input: impl IntoIterator<Item = Option<i32>>,
    keep_running: impl FnMut() -> bool,
) -> Result<(Vec<u8>, u32), SegmentError> {
    encode_with_row_count(input, keep_running)
}

/// Encodes one typed column of either width. See `encode_int4`.
pub fn encode<V: Int>(
    input: impl IntoIterator<Item = Option<V>>,
    keep_running: impl FnMut() -> bool,
) -> Result<Vec<u8>, SegmentError> {
    encode_with_row_count(input, keep_running).map(|(bytes, _rows)| bytes)
}

/// Returns the writer's row count with its completed bytes. This avoids opening
/// and validating freshly encoded output solely to recover its input count.
/// Persisted or external bytes must still pass Segment::open before reading.
pub fn encode_with_row_count<V: Int>(
    input: impl IntoIterator<Item = Option<V>>,
    mut keep_running: impl FnMut() -> bool,
) -> Result<(Vec<u8>, u32), SegmentError> {
    let mut output = Vec::new();
    reserve_output(&mut output, HEADER_BYTES)?;
    output.resize(HEADER_BYTES, 0);
    let mut input = input.into_iter().fuse();
    let mut rows = 0usize;
    let mut groups = 0u32;
    loop {
        poll(&mut keep_running)?;
        let mut buffer = [None; GROUP_ROWS];
        let mut len = 0;
        for slot in &mut buffer {
            let Some(value) = input.next() else {
                break;
            };
            if rows == MAX_ROWS {
                return Err(SegmentError::LimitExceeded);
            }
            *slot = value;
            rows += 1;
            len += 1;
        }
        if len == 0 {
            break;
        }
        append_group(&mut output, &buffer[..len])?;
        groups += 1;
    }
    output[..8].copy_from_slice(MAGIC);
    output[8..10].copy_from_slice(&VERSION.to_le_bytes());
    output[10..12].copy_from_slice(&V::SEGMENT_TYPE.to_le_bytes());
    output[16..20].copy_from_slice(&(rows as u32).to_le_bytes());
    output[20..24].copy_from_slice(&(GROUP_ROWS as u32).to_le_bytes());
    output[24..28].copy_from_slice(&groups.to_le_bytes());
    let bytes = output.len() as u32; // bounded by MAX_SEGMENT_BYTES
    output[32..36].copy_from_slice(&bytes.to_le_bytes());
    let crc = crc32fast::hash(&output[..36]);
    output[36..40].copy_from_slice(&crc.to_le_bytes());
    poll(&mut keep_running)?;
    Ok((output, rows as u32))
}

fn reserve_output(output: &mut Vec<u8>, additional: usize) -> Result<(), SegmentError> {
    let required = output
        .len()
        .checked_add(additional)
        .ok_or(SegmentError::LimitExceeded)?;
    if required > MAX_SEGMENT_BYTES {
        return Err(SegmentError::LimitExceeded);
    }
    if required > output.capacity() {
        let capacity = output
            .capacity()
            .saturating_mul(2)
            .max(required)
            .min(MAX_SEGMENT_BYTES);
        output
            .try_reserve_exact(capacity - output.len())
            .map_err(|_| SegmentError::AllocationFailed)?;
    }
    Ok(())
}

fn append_group<V: Int>(output: &mut Vec<u8>, values: &[Option<V>]) -> Result<(), SegmentError> {
    let w = V::WIDTH;
    let header = group_header_bytes(w);
    let bitmap_len = values.len().div_ceil(8);
    let start = output.len();
    let length = header + bitmap_len + values.len() * w;
    reserve_output(output, length)?;
    output.resize(start + length, 0);
    let mut nulls = 0u32;
    let mut min: Option<V> = None;
    let mut max: Option<V> = None;
    for (index, value) in values.iter().copied().enumerate() {
        if let Some(value) = value {
            output[start + header + index / 8] |= 1 << (index % 8);
            min = Some(min.map_or(value, |old| old.min(value)));
            max = Some(max.map_or(value, |old| old.max(value)));
            value.put(&mut output[start + header + bitmap_len + index * w..]);
        } else {
            nulls += 1;
        }
    }
    output[start..start + 4].copy_from_slice(&(values.len() as u32).to_le_bytes());
    output[start + 4..start + 8].copy_from_slice(&nulls.to_le_bytes());
    min.unwrap_or_default().put(&mut output[start + 8..]);
    max.unwrap_or_default().put(&mut output[start + 8 + w..]);
    output[start + 8 + 2 * w..start + 12 + 2 * w]
        .copy_from_slice(&(bitmap_len as u32).to_le_bytes());
    let crc = group_crc(
        &output[start..start + header - 4],
        &output[start + header..],
    );
    output[start + header - 4..start + header].copy_from_slice(&crc.to_le_bytes());
    Ok(())
}

fn group_crc(metadata: &[u8], payload: &[u8]) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(metadata);
    hasher.update(payload);
    hasher.finalize()
}

pub type Int4Segment<'a> = Segment<'a, i32>;
pub type Int4Group<'a> = Group<'a, i32>;

#[derive(Debug)]
pub struct Segment<'a, V: Int> {
    bytes: &'a [u8],
    rows: u32,
    groups: u32,
    values: std::marker::PhantomData<V>,
}

impl<'a, V: Int> Segment<'a, V> {
    /// Verifies all data, including zone-map truth, before pruning is allowed.
    /// CRC is corruption detection, not authentication of attacker-controlled data.
    pub fn open(
        bytes: &'a [u8],
        mut keep_running: impl FnMut() -> bool,
    ) -> Result<Self, SegmentError> {
        poll(&mut keep_running)?;
        if bytes.len() > MAX_SEGMENT_BYTES {
            return Err(SegmentError::LimitExceeded);
        }
        if bytes.len() < HEADER_BYTES {
            return Err(SegmentError::Truncated);
        }
        if &bytes[..8] != MAGIC {
            return Err(SegmentError::InvalidHeader);
        }
        let version = u16_at(bytes, 8)?;
        if version != VERSION {
            return Err(SegmentError::UnsupportedVersion(version));
        }
        if u16_at(bytes, 10)? != V::SEGMENT_TYPE
            || u16_at(bytes, 12)? != 0
            || u16_at(bytes, 14)? != 0
            || u32_at(bytes, 28)? != 0
            || u32_at(bytes, 20)? != GROUP_ROWS as u32
        {
            return Err(SegmentError::InvalidHeader);
        }
        if crc32fast::hash(&bytes[..36]) != u32_at(bytes, 36)? {
            return Err(SegmentError::ChecksumMismatch);
        }
        let rows = u32_at(bytes, 16)?;
        let groups = u32_at(bytes, 24)?;
        if rows as usize > MAX_ROWS {
            return Err(SegmentError::LimitExceeded);
        }
        if groups as usize != (rows as usize).div_ceil(GROUP_ROWS) {
            return Err(SegmentError::InvalidMetadata);
        }
        // With fixed group sizes divisible by eight, bitmap lengths sum to ceil(rows/8).
        let expected = HEADER_BYTES
            + groups as usize * group_header_bytes(V::WIDTH)
            + rows as usize * V::WIDTH
            + (rows as usize).div_ceil(8);
        if bytes.len() != expected || u32_at(bytes, 32)? as usize != expected {
            return Err(SegmentError::InvalidMetadata);
        }
        let segment = Self {
            bytes,
            rows,
            groups,
            values: std::marker::PhantomData,
        };
        let mut remaining = rows;
        for group in segment.groups() {
            poll(&mut keep_running)?;
            let group = group?;
            if group.rows != remaining.min(GROUP_ROWS as u32) {
                return Err(SegmentError::InvalidMetadata);
            }
            remaining -= group.rows;
            group.validate()?;
        }
        if remaining != 0 {
            return Err(SegmentError::InvalidMetadata);
        }
        Ok(segment)
    }

    pub fn rows(&self) -> u32 {
        self.rows
    }
    pub fn group_count(&self) -> u32 {
        self.groups
    }
    pub fn byte_len(&self) -> u32 {
        self.bytes.len() as u32
    }
    pub fn groups(&self) -> GroupIter<'a, V> {
        GroupIter {
            bytes: self.bytes,
            offset: HEADER_BYTES,
            remaining: self.groups,
            values: std::marker::PhantomData,
        }
    }
}

pub struct GroupIter<'a, V: Int> {
    bytes: &'a [u8],
    offset: usize,
    remaining: u32,
    values: std::marker::PhantomData<V>,
}

impl<'a, V: Int> Iterator for GroupIter<'a, V> {
    type Item = Result<Group<'a, V>, SegmentError>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        self.remaining -= 1;
        let result = parse_group(self.bytes, self.offset);
        match &result {
            Ok(group) => self.offset += group.byte_len() as usize,
            Err(_) => self.remaining = 0,
        }
        Some(result)
    }
}

#[derive(Debug)]
pub struct Group<'a, V: Int> {
    rows: u32,
    nulls: u32,
    min: V,
    max: V,
    metadata: &'a [u8],
    bitmap: &'a [u8],
    data: &'a [u8],
    crc: u32,
}

fn parse_group<V: Int>(bytes: &[u8], offset: usize) -> Result<Group<'_, V>, SegmentError> {
    let w = V::WIDTH;
    let header_len = group_header_bytes(w);
    let header = bytes
        .get(offset..offset + header_len)
        .ok_or(SegmentError::Truncated)?;
    let rows = u32_at(header, 0)?;
    let nulls = u32_at(header, 4)?;
    if rows == 0 || rows > GROUP_ROWS as u32 || nulls > rows {
        return Err(SegmentError::InvalidMetadata);
    }
    let bitmap_len = (rows as usize).div_ceil(8);
    if u32_at(header, 8 + 2 * w)? as usize != bitmap_len {
        return Err(SegmentError::InvalidMetadata);
    }
    let start = offset + header_len;
    let data_start = start + bitmap_len;
    let bitmap = bytes
        .get(start..data_start)
        .ok_or(SegmentError::Truncated)?;
    let data = bytes
        .get(data_start..data_start + rows as usize * w)
        .ok_or(SegmentError::Truncated)?;
    Ok(Group {
        rows,
        nulls,
        min: int_at(header, 8)?,
        max: int_at(header, 8 + w)?,
        metadata: &header[..header_len - 4],
        bitmap,
        data,
        crc: u32_at(header, header_len - 4)?,
    })
}

impl<V: Int> Group<'_, V> {
    pub fn rows(&self) -> u32 {
        self.rows
    }
    pub fn nulls(&self) -> u32 {
        self.nulls
    }
    pub fn min(&self) -> Option<V> {
        (self.rows != self.nulls).then_some(self.min)
    }
    pub fn max(&self) -> Option<V> {
        (self.rows != self.nulls).then_some(self.max)
    }
    pub fn byte_len(&self) -> u32 {
        (group_header_bytes(V::WIDTH) + self.bitmap.len() + self.data.len()) as u32
    }
    pub fn values(&self) -> impl ExactSizeIterator<Item = Option<V>> + '_ {
        self.data
            .chunks_exact(V::WIDTH)
            .enumerate()
            .map(|(index, data)| {
                (self.bitmap[index / 8] & (1 << (index % 8)) != 0).then(|| V::get(data))
            })
    }

    fn validate(&self) -> Result<(), SegmentError> {
        let mut hasher = crc32fast::Hasher::new();
        hasher.update(self.metadata);
        hasher.update(self.bitmap);
        hasher.update(self.data);
        if hasher.finalize() != self.crc {
            return Err(SegmentError::ChecksumMismatch);
        }
        let used = self.rows as usize % 8;
        if used != 0 && self.bitmap[self.bitmap.len() - 1] >> used != 0 {
            return Err(SegmentError::NonCanonical);
        }
        let mut nulls = 0;
        let mut min: Option<V> = None;
        let mut max: Option<V> = None;
        let w = V::WIDTH;
        for (index, value) in self.values().enumerate() {
            if let Some(value) = value {
                min = Some(min.map_or(value, |old| old.min(value)));
                max = Some(max.map_or(value, |old| old.max(value)));
            } else {
                nulls += 1;
                if self.data[index * w..index * w + w].iter().any(|b| *b != 0) {
                    return Err(SegmentError::NonCanonical);
                }
            }
        }
        if self.nulls != nulls
            || self.min != min.unwrap_or_default()
            || self.max != max.unwrap_or_default()
        {
            return Err(SegmentError::InvalidMetadata);
        }
        Ok(())
    }
}
