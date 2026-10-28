// Single-bit flips stop at the CRC. These edits rewrite the CRC, so the parsers
// behind it see arbitrary multi-byte damage. A decoder must then refuse the
// bytes or accept only bytes it would write itself.
use syntra_storage::{
    chunks::Manifest,
    delta::Delta,
    replay::{Limits, ReplayState},
};

const ROUNDS: usize = 20_000;

fn limits() -> Limits {
    Limits {
        rows: 1_000_000,
        changes: 4096,
        buffer_bytes: 32 * 1024 * 1024,
    }
}

/// Seeded edits of one to four bytes outside the CRC field, then a fresh CRC.
fn mutations(original: &[u8], crc_at: usize, seed: u32) -> impl Iterator<Item = Vec<u8>> + '_ {
    let mut state = seed;
    let mut next = move || {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        state >> 8
    };
    (0..ROUNDS).map(move |_| {
        let mut bytes = original.to_vec();
        for _ in 0..=next() % 4 {
            let offset = next() as usize % (bytes.len() - 4);
            let offset = if offset >= crc_at { offset + 4 } else { offset };
            bytes[offset] = next() as u8;
        }
        let mut crc = crc32fast::Hasher::new();
        crc.update(&bytes[..crc_at]);
        crc.update(&bytes[crc_at + 4..]);
        bytes[crc_at..crc_at + 4].copy_from_slice(&crc.finalize().to_le_bytes());
        bytes
    })
}

/// Runs every mutation and returns how many the decoder refused; every accepted
/// one must encode back to the same bytes.
fn refused(
    original: &[u8],
    crc_at: usize,
    seed: u32,
    reencode: impl Fn(&[u8]) -> Option<Vec<u8>>,
) -> usize {
    let mut refused = 0;
    for bytes in mutations(original, crc_at, seed) {
        match reencode(&bytes) {
            None => refused += 1,
            Some(written) => assert_eq!(written, bytes, "accepted a non-canonical encoding"),
        }
    }
    refused
}

/// Payload bytes take any value, so some edits stay valid; headers and canonical
/// order do not. Both outcomes must occur, or the edits miss the parser.
fn both_paths(refused: usize) {
    assert!(
        (1000..ROUNDS - 1000).contains(&refused),
        "refused {refused} of {ROUNDS}"
    );
}

#[test]
fn resealed_checkpoint_edits_are_refused_or_canonical() {
    for (fixture, seed) in [
        (&include_bytes!("fixtures/keyed_checkpoint_v1.bin")[..], 1),
        (include_bytes!("fixtures/keyed_checkpoint_v2_int8.bin"), 2),
    ] {
        let narrow = seed == 1;
        let count = refused(fixture, 104, seed, |bytes| {
            if narrow {
                let state = <ReplayState>::decode_checkpoint(bytes, limits(), || true).ok()?;
                Some(state.encode_checkpoint(|| true).unwrap())
            } else {
                let state =
                    ReplayState::<i64, i64>::decode_checkpoint(bytes, limits(), || true).ok()?;
                Some(state.encode_checkpoint(|| true).unwrap())
            }
        });
        both_paths(count);
    }
}

#[test]
fn resealed_delta_edits_are_refused_or_canonical() {
    let narrow = include_bytes!("fixtures/delta_v1_int4.bin");
    let count = refused(narrow, 64, 3, |bytes| {
        let delta = Delta::<i32, i32>::decode(bytes, || true).ok()?;
        Some(delta.encode(|| true).unwrap())
    });
    both_paths(count);
    let wide = include_bytes!("fixtures/delta_v1_int8.bin");
    let count = refused(wide, 64, 4, |bytes| {
        let delta = Delta::<i64, i64>::decode(bytes, || true).ok()?;
        Some(delta.encode(|| true).unwrap())
    });
    both_paths(count);
}

#[test]
fn resealed_manifest_edits_are_refused_or_canonical() {
    for (fixture, seed) in [
        (&include_bytes!("fixtures/chunk_manifest_v1.bin")[..], 5),
        (include_bytes!("fixtures/chunk_manifest_v2.bin"), 6),
    ] {
        let count = refused(fixture, 72, seed, |bytes| {
            Some(Manifest::decode(bytes).ok()?.encode().unwrap())
        });
        both_paths(count);
    }
}
