#!/usr/bin/env bash
set -euo pipefail
SYNTRA_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$SYNTRA_ROOT"
exec bash scripts/benchmark-production.sh --mixed-seconds 5 --mixed-rounds 3 \
    --wait-sample-ms 10 --output benchmarks/results/server-waits-latest.json "$@"
