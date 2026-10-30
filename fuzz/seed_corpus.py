"""Seed libFuzzer with the storage tests' independently generated fixtures."""

from pathlib import Path

fuzz_root = Path(__file__).resolve().parent
fixtures = fuzz_root.parent / "crates/syntra-storage/tests/fixtures"
corpus = fuzz_root / "corpus/parsers"
corpus.mkdir(parents=True, exist_ok=True)

seeds = [
    (0, "keyed_checkpoint_v1.bin"),
    (1, "keyed_checkpoint_v2_int8.bin"),
    (2, "keyed_checkpoint_v2_int4_int8.bin"),
    (3, "delta_v1_int4.bin"),
    (4, "delta_v1_int8.bin"),
    (5, "delta_v1_int4_int8.bin"),
    (6, "chunk_manifest_v1.bin"),
    (6, "chunk_manifest_v2.bin"),
    (7, "int4-v1.bin"),
    (8, "int8-v1.bin"),
]
for tag, name in seeds:
    fixture = (fixtures / name).read_bytes()
    for mode in (0, 0x80):
        (corpus / f"{mode}-{name}").write_bytes(bytes([tag | mode]) + fixture)

grouping_corpus = fuzz_root / "corpus/grouping"
grouping_corpus.mkdir(parents=True, exist_ok=True)
for wide in (0, 1):
    rows = bytearray()
    for n in range(128):
        key = n % 13 - 6 + (5_000_000_000 if wide else 0)
        value = (n * 7919) % 100003 - 50001
        if wide:
            value = value * 1_000_000_000_000 + 3_000_000_000_000_000_000
        flags = 4 | (1 if n % 17 == 0 else 0) | (2 if n % 7 == 0 else 0)
        rows.append(flags)
        rows.extend(key.to_bytes(8, "little", signed=True))
        rows.extend(value.to_bytes(8, "little", signed=True))
    for limit in (0, 1, 16, 64):
        (grouping_corpus / f"{wide}-{limit}").write_bytes(bytes([wide, limit]) + rows)
