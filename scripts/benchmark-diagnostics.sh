#!/usr/bin/env bash
set -euo pipefail
SYNTRA_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$SYNTRA_ROOT"
exec bash scripts/benchmark-production.sh --mixed-seconds 10 --mixed-rounds 3 \
    --output benchmarks/results/dispatch-lag-latest.json "$@"
