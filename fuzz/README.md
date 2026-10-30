# Storage and grouping fuzzing

The `Storage and execution fuzzing` GitHub workflow runs libFuzzer with AddressSanitizer
for one hour by default. Choose 300 seconds for a smoke run. Select `parsers` for
decoders or `grouping` for integer grouped aggregates. The workflow pins
cargo-fuzz 0.12.0, libfuzzer-sys 0.4.10 and nightly-2026-10-30, and records the
source commit and compiler in its artifact.

The `parsers` target covers nullable `int4` and `int8` segments; checkpoints and partition
deltas for `int4`, `int8` and mixed `int4` keys with `int8` values; and both
manifest versions. It starts from the storage tests' independent binary fixtures.
One input bit selects checksum repair for checkpoints, deltas and manifests.
This lets mutations reach validation after the checksum. Other inputs retain raw
damaged checksums. Segment checksums are never repaired by the harness.

Every accepted checkpoint, delta or manifest must re-encode to identical canonical
bytes. Accepted segments must expose their declared row count through their groups.
Panics, sanitizer failures, noncanonical accepted encodings and row-count mismatches
fail the run. Decoder errors are expected rejections.

The `grouping` target compares both integer grouping paths with an independent
ordered-map oracle using direct arithmetic. Inputs exercise nullable keys and
values, repeated and distinct keys, wide sums and group limits from 0 to 255.
The deterministic tests separately cover the full 65,536-group boundary.
The oracle checks exact ordered results or the expected limit error.

To run locally, install the pinned tools and seed the corpus:

```sh
rustup toolchain install nightly-2026-10-30 --profile minimal
cargo +1.99.0 install cargo-fuzz --version 0.12.0 --locked --root .tools/fuzz
python3 fuzz/seed_corpus.py
cargo fetch --locked --manifest-path fuzz/Cargo.toml
RUSTUP_TOOLCHAIN=nightly-2026-10-30 CARGO_NET_OFFLINE=true \
  .tools/fuzz/bin/cargo-fuzz fuzz run parsers -- \
  -max_total_time=300 -timeout=10 -rss_limit_mb=2048 -max_len=65536 -seed=20261031
```

The first input byte selects the format and checksum mode. It is a harness byte,
not part of the stored encoding. Reproduce a saved crash with:

```sh
RUSTUP_TOOLCHAIN=nightly-2026-10-30 \
  .tools/fuzz/bin/cargo-fuzz fuzz run parsers fuzz/artifacts/parsers/crash-input
```

Use the crash file's actual path. GitHub retains the log, generated corpus and crash
inputs for seven days. Keep any confirmed bug's minimized input in a regression
test before fixing the decoder.

Inputs are limited to 64 KiB, individual cases to ten seconds and process RSS to
2 GiB. These are fuzz-run limits. They do not test the largest valid database
segments or bound memory across PostgreSQL backends. A successful run adds
corruption coverage; it does not prove SQL visibility or recovery correctness.
See the [Rust Fuzz Book](https://rust-fuzz.github.io/book/cargo-fuzz.html)
for the underlying tool.
