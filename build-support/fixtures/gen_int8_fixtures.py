"""Write the int8 golden fixtures for syntra-storage from the format description
alone, without the Rust encoder: struct.pack for layout and zlib.crc32 for CRCs.

    python3 -I build-support/fixtures/gen_int8_fixtures.py crates/syntra-storage/tests/fixtures
"""
import struct
import sys
import zlib
from pathlib import Path

GROUP_ROWS = 1024


def segment_int8(values):
    """Nullable int8 segment: type code 2, one or more groups, 8-byte values."""
    groups = []
    for start in range(0, len(values), GROUP_ROWS):
        part = values[start:start + GROUP_ROWS]
        bitmap = bytearray((len(part) + 7) // 8)
        data = bytearray()
        present = [v for v in part if v is not None]
        for index, value in enumerate(part):
            if value is not None:
                bitmap[index // 8] |= 1 << (index % 8)
            data += struct.pack("<q", 0 if value is None else value)
        metadata = struct.pack(
            "<IIqqI",
            len(part),
            len(part) - len(present),
            min(present) if present else 0,
            max(present) if present else 0,
            len(bitmap),
        )
        crc = zlib.crc32(metadata + bytes(bitmap) + bytes(data))
        groups.append(metadata + struct.pack("<I", crc) + bytes(bitmap) + bytes(data))
    body = b"".join(groups)
    total = 40 + len(body)
    header = b"SYNTRAS1" + struct.pack(
        "<HHHHIIIII", 1, 2, 0, 0, len(values), GROUP_ROWS, len(groups), 0, total
    )
    header += struct.pack("<I", zlib.crc32(header))
    return header + body


def state_v2(key_width, value_width, stream, epoch, sequence, rows):
    """Keyed state version 2: widths at offset 108, a NULL bitmap, then rows."""
    codes = {4: "i", 8: "q"}
    bitmap = bytearray((len(rows) + 7) // 8)
    data = bytearray()
    for index, (key, value) in enumerate(rows):
        if value is None:
            bitmap[index // 8] |= 1 << (index % 8)
        data += struct.pack("<" + codes[key_width], key)
        data += struct.pack("<" + codes[value_width], 0 if value is None else value)
    payload = bytes(bitmap) + bytes(data)
    head = b"SYNTRACP" + struct.pack("<HHI", 2, 0, len(rows)) + stream
    head += struct.pack("<QQ", epoch, sequence) + bytes(32)
    head += struct.pack("<Q", len(payload))
    tail = bytes([key_width, value_width, 0, 0])
    crc = zlib.crc32(head + tail + payload)
    return head + struct.pack("<I", crc) + tail + payload


def fmix64_bucket(key):
    x = key & 0xFFFFFFFFFFFFFFFF
    x ^= x >> 33
    x = (x * 0xFF51AFD7ED558CCD) & 0xFFFFFFFFFFFFFFFF
    x ^= x >> 33
    x = (x * 0xC4CEB9FE1A85EC53) & 0xFFFFFFFFFFFFFFFF
    x ^= x >> 33
    return x & 63


def main(out):
    out = Path(out)
    values = [None, -(2**63), 0, 2**63 - 1, 5_000_000_000, None, -1]
    (out / "int8-v1.bin").write_bytes(segment_int8(values))
    rows = [(-3, None), (6_000_000_000, -7_000_000_000)]
    (out / "keyed_checkpoint_v2_int8.bin").write_bytes(
        state_v2(8, 8, bytes([7] * 32), 9, 0, rows)
    )
    mixed = [(-3, None), (5, -7_000_000_000)]
    (out / "keyed_checkpoint_v2_int4_int8.bin").write_bytes(
        state_v2(4, 8, bytes([7] * 32), 9, 0, mixed)
    )
    keys = [-(2**63), -(2**31) - 1, -1, 0, 1, 2, 2**31, 2**32, 5_000_000_000, 2**63 - 1]
    (out / "chunk_routing_int8.txt").write_text(
        "".join(f"{key} {fmix64_bucket(key)}\n" for key in keys)
    )


if __name__ == "__main__":
    main(sys.argv[1])
