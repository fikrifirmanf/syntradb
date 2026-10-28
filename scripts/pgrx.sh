#!/usr/bin/env bash
set -euo pipefail
SYNTRA_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export PGRX_HOME="$SYNTRA_ROOT/.pgrx"
export PATH="$SYNTRA_ROOT/.tools/bin:$PATH"
# These rollback fixtures alter shared private catalogs and their triggers.
# Run them serially; committed multi-session and load tests exercise concurrency.
if [[ "${1:-}" == "test" ]]; then
    export RUST_TEST_THREADS="${RUST_TEST_THREADS:-1}"
fi
cd "$SYNTRA_ROOT/crates/syntra-pg"
exec cargo pgrx "$@"
