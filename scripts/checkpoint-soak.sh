#!/usr/bin/env bash
set -euo pipefail
SYNTRA_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$SYNTRA_ROOT"
# Paced, not saturated: a saturated multi-minute run can exhaust local disk.
# Later arguments override these defaults.
exec bash scripts/benchmark-production.sh --mixed-seconds 0 --rows 10000 \
    --repetitions 3 --checkpoint-soak-seconds 1200 \
    --checkpoint-soak-window-seconds 60 --checkpoint-worker-rate 30 \
    --checkpoint-combined-maintenance true \
    --output benchmarks/results/checkpoint-soak-latest.json "$@"
