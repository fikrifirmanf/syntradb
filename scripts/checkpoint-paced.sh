#!/usr/bin/env bash
set -euo pipefail
SYNTRA_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$SYNTRA_ROOT"
exec bash scripts/checkpoint-stress.sh --checkpoint-stress-seconds 30 \
    --checkpoint-worker-rate 100 --checkpoint-reader-replay false \
    --output benchmarks/results/checkpoint-paced-latest.json "$@"
