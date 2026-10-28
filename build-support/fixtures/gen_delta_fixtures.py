"""Write the partition-delta golden fixtures for syntra-storage from the format
description alone, without the Rust encoder: struct.pack for layout and zlib.crc32.

    python3 -I build-support/fixtures/gen_delta_fixtures.py crates/syntra-storage/tests/fixtures
"""
import struct
import sys
import zlib
from pathlib import Path

ABSENT = object()
CODES = {4: "i", 8: "q"}


def code(value):
    if value is ABSENT:
        return 0
    return 1 if value is None else 2


def delta_v1(key_width, value_width, stream, epoch, base_revision, entries):
    """Header of 72 bytes, then entries: key, tag (base code | current code << 2),
    base value, current value; values are zero unless their code is 2."""
    body = bytearray()
    for key, base, current in entries:
        body += struct.pack("<" + CODES[key_width], key)
        body.append(code(base) | code(current) << 2)
        for value in (base, current):
            plain = value if code(value) == 2 else 0
            body += struct.pack("<" + CODES[value_width], plain)
    head = b"SYNTRACD" + struct.pack("<HBBI", 1, key_width, value_width, len(entries))
    head += stream + struct.pack("<QQ", epoch, base_revision)
    tail = bytes(4)
    crc = zlib.crc32(head + tail + bytes(body))
    return head + struct.pack("<I", crc) + tail + bytes(body)


def manifest_v2(stream, epoch, applied, generation, refs):
    """Manifest version 2: the v1 header, then 64 references of 128 bytes:
    revision, rows, base rows, state digest, segment digest, base revision,
    delta entry count, four zero bytes, delta digest."""
    body = bytearray()
    for revision, rows, state, segment, delta in refs:
        base_revision, base_rows, entries, digest = delta or (revision, rows, 0, bytes(32))
        body += struct.pack("<QII", revision, rows, base_rows) + state + segment
        body += struct.pack("<QII", base_revision, entries, 0) + digest
    total = sum(ref[1] for ref in refs)
    head = b"SYNTRACM" + struct.pack("<HHI", 2, 64, total) + stream
    head += struct.pack("<QQQ", epoch, applied, generation)
    crc = zlib.crc32(head + bytes(4) + bytes(body))
    return head + struct.pack("<I", crc) + bytes(4) + bytes(body)


def main(out):
    out = Path(out)
    stream = bytes([7] * 32)
    narrow = [(-5, ABSENT, 7), (3, 1, None), (9, None, ABSENT), (12, 2, -3)]
    (out / "delta_v1_int4.bin").write_bytes(delta_v1(4, 4, stream, 9, 4, narrow))
    wide = [
        (-6_000_000_000, ABSENT, -(2**63)),
        (3, 2**63 - 1, None),
        (5_000_000_000, None, ABSENT),
    ]
    (out / "delta_v1_int8.bin").write_bytes(delta_v1(8, 8, stream, 9, 4, wide))
    mixed = [(-5, ABSENT, 7_000_000_000), (12, 2, None)]
    (out / "delta_v1_int4_int8.bin").write_bytes(delta_v1(4, 8, stream, 9, 4, mixed))
    empty = (0, 0, bytes(32), bytes(32), None)
    first = (1, 2, bytes([3] * 32), bytes([4] * 32), (0, 3, 4, bytes([5] * 32)))
    (out / "chunk_manifest_v2.bin").write_bytes(
        manifest_v2(stream, 9, 5, 2, [first] + [empty] * 63)
    )


if __name__ == "__main__":
    main(sys.argv[1])
