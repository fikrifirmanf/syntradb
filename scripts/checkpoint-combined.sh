#!/usr/bin/env bash
set -euo pipefail
SYNTRA_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$SYNTRA_ROOT"
exec bash scripts/checkpoint-stress.sh --checkpoint-combined-maintenance true \
    --output benchmarks/results/checkpoint-combined-latest.json "$@"
