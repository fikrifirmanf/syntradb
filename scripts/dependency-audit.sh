#!/usr/bin/env bash
# Audit both independent lockfiles against the same fetched advisory snapshot.
set -euo pipefail
SYNTRA_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$SYNTRA_ROOT"
AUDIT_BIN="${SYNTRA_AUDIT_BIN:-$SYNTRA_ROOT/.tools/audit/bin/cargo-audit}"
OUT="${1:-benchmarks/results/dependency-audit}"
DB="$SYNTRA_ROOT/.tools/advisory-db"
if [[ ! -x "$AUDIT_BIN" ]]; then
    echo "Install cargo-audit 0.22.2: cargo install cargo-audit --version 0.22.2 --locked --root .tools/audit" >&2
    exit 2
fi
VERSION="$("$AUDIT_BIN" --version)"
[[ "$VERSION" == *" 0.22.2" ]] || { echo "dependency audit needs cargo-audit 0.22.2, got $VERSION" >&2; exit 2; }
mkdir -p "$OUT"
printf '%s\n' "$VERSION" >"$OUT/tool-version.txt"
git rev-parse HEAD >"$OUT/source-commit.txt"
audit_status=0
for name in root fuzz; do
    lockfile=Cargo.lock
    [[ "$name" != fuzz ]] || lockfile=fuzz/Cargo.lock
    # Keep each result even if the other lockfile contains an advisory or fails.
    audit_args=(audit --db "$DB" --file "$lockfile" --json --deny unsound --deny yanked)
    [[ "$name" != fuzz ]] || audit_args+=(--no-fetch)
    if "$AUDIT_BIN" "${audit_args[@]}" \
        >"$OUT/$name.json" 2>"$OUT/$name.log"; then
        printf '%s\n' "$name: no denied advisory findings; review JSON for informational warnings"
    else
        printf '%s\n' "$name: dependency audit failed; inspect $OUT/$name.json and $OUT/$name.log" >&2
        audit_status=1
    fi
done
if ! git -C "$DB" rev-parse HEAD >"$OUT/advisory-commit.txt"; then
    echo "dependency audit could not record the advisory database revision" >&2
    audit_status=1
fi
exit "$audit_status"
