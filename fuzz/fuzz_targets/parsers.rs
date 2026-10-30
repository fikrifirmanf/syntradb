#![no_main]

use libfuzzer_sys::fuzz_target;
use syntra_storage::{
    chunks::Manifest,
    delta::Delta,
    replay::{Limits, ReplayState},
    Int, Segment,
};

fn checkpoint<K: Int, V: Int>(bytes: &[u8]) {
    let limits = Limits {
        rows: 1_000_000,
        changes: 4096,
        buffer_bytes: 32 * 1024 * 1024,
    };
    if let Ok(state) = ReplayState::<K, V>::decode_checkpoint(bytes, limits, || true) {
        assert_eq!(state.encode_checkpoint(|| true).unwrap(), bytes);
    }
}

fn delta<K: Int, V: Int>(bytes: &[u8]) {
    if let Ok(delta) = Delta::<K, V>::decode(bytes, || true) {
        assert_eq!(delta.encode(|| true).unwrap(), bytes);
    }
}

fn segment<V: Int>(bytes: &[u8]) {
    if let Ok(segment) = Segment::<V>::open(bytes, || true) {
        let mut rows = 0_u64;
        for group in segment.groups() {
            let group = group.unwrap();
            assert_eq!(group.values().count(), group.rows() as usize);
            rows += u64::from(group.rows());
        }
        assert_eq!(rows, u64::from(segment.rows()));
    }
}

fuzz_target!(|data: &[u8]| {
    let Some((&tag, input)) = data.split_first() else {
        return;
    };
    let kind = (tag & 0x7f) % 9;
    let mut bytes = input.to_vec();
    // Half the inputs repair the checksum, exposing metadata validation to the
    // fuzzer. The other half exercise checksum rejection on raw damaged bytes.
    let crc_at = match kind {
        0..=2 => Some(104),
        3..=5 => Some(64),
        6 => Some(72),
        _ => None,
    };
    if let Some(at) = crc_at.filter(|at| tag & 0x80 != 0 && bytes.len() >= at + 4) {
        let mut crc = crc32fast::Hasher::new();
        crc.update(&bytes[..at]);
        crc.update(&bytes[at + 4..]);
        bytes[at..at + 4].copy_from_slice(&crc.finalize().to_le_bytes());
    }
    match kind {
        0 => checkpoint::<i32, i32>(&bytes),
        1 => checkpoint::<i64, i64>(&bytes),
        2 => checkpoint::<i32, i64>(&bytes),
        3 => delta::<i32, i32>(&bytes),
        4 => delta::<i64, i64>(&bytes),
        5 => delta::<i32, i64>(&bytes),
        6 => {
            if let Ok(manifest) = Manifest::decode(&bytes) {
                assert_eq!(manifest.encode().unwrap(), bytes);
            }
        }
        7 => segment::<i32>(&bytes),
        8 => segment::<i64>(&bytes),
        _ => unreachable!(),
    }
});
