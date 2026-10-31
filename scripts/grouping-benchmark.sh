#!/usr/bin/env bash
set -euo pipefail
SYNTRA_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
exec bash "$SYNTRA_ROOT/scripts/benchmark-production.sh" \
    --mixed-seconds 0 --rows 100000 --repetitions 7 \
    --grouping-benchmark true \
    --output benchmarks/results/grouping-latest.json "$@"
