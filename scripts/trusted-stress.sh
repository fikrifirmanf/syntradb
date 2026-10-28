#!/usr/bin/env bash
set -euo pipefail
SYNTRA_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$SYNTRA_ROOT"
exec bash scripts/benchmark-production.sh --mixed-seconds 0 --rows 10000 \
    --repetitions 3 --trusted-stress-seconds 60 \
    --output benchmarks/results/trusted-stress-latest.json "$@"
