#!/usr/bin/env bash
set -euo pipefail
SYNTRA_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$SYNTRA_ROOT"
python3 -m unittest discover -s scripts/tests -p 'test_*.py'
cargo fmt --all -- --check
rustfmt --edition 2021 --check crates/syntra-pg/src/source_tests.rs
rustfmt --edition 2021 --check crates/syntra-pg/src/projection_tests.rs
rustfmt --edition 2021 --check crates/syntra-pg/src/tracking_tests.rs
rustfmt --edition 2021 --check crates/syntra-pg/src/ddl_tests.rs
rustfmt --edition 2021 --check crates/syntra-pg/src/heap_tests.rs
rustfmt --edition 2021 --check crates/syntra-pg/src/temporal_tests.rs
rustfmt --edition 2021 --check crates/syntra-pg/src/grouping_tests.rs
rustfmt --edition 2021 --check crates/syntra-pg/src/live_tests.rs
rustfmt --edition 2021 --check crates/syntra-pg/src/capture_tests.rs
rustfmt --edition 2021 --check crates/syntra-pg/src/checkpoint_tests.rs
rustfmt --edition 2021 --check crates/syntra-pg/src/int8_tests.rs
rustfmt --edition 2021 --check crates/syntra-pg/src/int2_tests.rs
rustfmt --edition 2021 --check crates/syntra-pg/src/numeric_tests.rs
rustfmt --edition 2021 --check crates/syntra-pg/src/delta_tests.rs
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked
cargo test --locked -p syntra-bench
bash scripts/pgrx.sh test pg18 --cargo=--locked
