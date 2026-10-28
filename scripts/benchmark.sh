#!/usr/bin/env bash
set -euo pipefail
SYNTRA_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$SYNTRA_ROOT"
bash scripts/pgrx.sh install --release --pg-config "$SYNTRA_ROOT/.pgrx/18.6/pgrx-install/bin/pg_config"
cargo run --locked --release -p syntra-bench -- --pg-bin "$SYNTRA_ROOT/.pgrx/18.6/pgrx-install/bin" "$@"
