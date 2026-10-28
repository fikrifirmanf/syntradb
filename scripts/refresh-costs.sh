#!/usr/bin/env bash
set -euo pipefail
SYNTRA_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$SYNTRA_ROOT"
exec bash scripts/benchmark-production.sh --mixed-seconds 0 --rows 10000 \
    --repetitions 51 --refresh-cost-checks true \
    --output benchmarks/results/refresh-costs-latest.json "$@"
