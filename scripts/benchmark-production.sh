#!/usr/bin/env bash
set -euo pipefail
SYNTRA_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$SYNTRA_ROOT"
bash scripts/setup-production.sh
SYNTRA_PG_BIN="$SYNTRA_ROOT/.tools/postgresql-production/18.6/install/bin"
export CARGO_TARGET_DIR="$SYNTRA_ROOT/target/production"
export PGRX_PG_CONFIG_PATH="$SYNTRA_PG_BIN/pg_config"
bash scripts/pgrx.sh install --release --cargo=--locked --pg-config "$PGRX_PG_CONFIG_PATH"
cargo run --locked --release -p syntra-bench -- --pg-bin "$SYNTRA_PG_BIN" \
    --require-production true --mixed-seconds 3 --mixed-rounds 3 \
    --output benchmarks/results/production-latest.json "$@"
