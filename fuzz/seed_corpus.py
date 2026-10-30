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
