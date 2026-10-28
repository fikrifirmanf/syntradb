#!/usr/bin/env bash
# Installs development tools into this project; never starts a system service.
set -euo pipefail
SYNTRA_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SYNTRA_PG_VERSION=18.6
SYNTRA_PGRX_VERSION=0.19.3
SYNTRA_PG_SOURCE="$SYNTRA_ROOT/.pgrx/$SYNTRA_PG_VERSION"
SYNTRA_PG_CONFIG="$SYNTRA_PG_SOURCE/pgrx-install/bin/pg_config"
export PGRX_HOME="$SYNTRA_ROOT/.pgrx"
export PATH="$SYNTRA_ROOT/.tools/bin:$PATH"
cd "$SYNTRA_ROOT"

if command -v brew >/dev/null 2>&1; then
    SYNTRA_ICU_PREFIX="$(brew --prefix icu4c 2>/dev/null || true)"
    if [[ -d "$SYNTRA_ICU_PREFIX/lib/pkgconfig" ]]; then
        export PKG_CONFIG_PATH="$SYNTRA_ICU_PREFIX/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
    fi
fi

if [[ ! -x .tools/bin/cargo-pgrx ]] || [[ "$(cargo pgrx --version)" != "cargo-pgrx $SYNTRA_PGRX_VERSION" ]]; then
    cargo install cargo-pgrx --version "$SYNTRA_PGRX_VERSION" --locked --root .tools
fi

if [[ ! -x "$SYNTRA_PG_CONFIG" ]]; then
    mkdir -p .tools/downloads .pgrx
    SYNTRA_ARCHIVE="postgresql-$SYNTRA_PG_VERSION.tar.bz2"
    SYNTRA_URL="https://ftp.postgresql.org/pub/source/v$SYNTRA_PG_VERSION/$SYNTRA_ARCHIVE"
    curl --fail --location --retry 3 --connect-timeout 10 --max-time 300 "$SYNTRA_URL" -o ".tools/downloads/$SYNTRA_ARCHIVE"
    curl --fail --location --retry 3 --connect-timeout 10 --max-time 60 "$SYNTRA_URL.sha256" -o ".tools/downloads/$SYNTRA_ARCHIVE.sha256"
    (cd .tools/downloads && shasum -a 256 -c "$SYNTRA_ARCHIVE.sha256")
    if [[ ! -d "$SYNTRA_PG_SOURCE" ]]; then
        tar -xjf ".tools/downloads/$SYNTRA_ARCHIVE" -C .pgrx
        mv ".pgrx/postgresql-$SYNTRA_PG_VERSION" "$SYNTRA_PG_SOURCE"
    fi
    (
        cd "$SYNTRA_PG_SOURCE"
        ./configure --prefix="$SYNTRA_PG_SOURCE/pgrx-install" --with-pgport=28818 --enable-debug --enable-cassert
        make -j "${SYNTRA_BUILD_JOBS:-4}"
        make install
    )
fi

if [[ "$("$SYNTRA_PG_CONFIG" --version)" != "PostgreSQL $SYNTRA_PG_VERSION" ]]; then
    echo "Expected PostgreSQL $SYNTRA_PG_VERSION at $SYNTRA_PG_CONFIG" >&2
    exit 1
fi
cargo pgrx init --pg18 "$SYNTRA_PG_CONFIG" --base-port 28800 --base-testing-port 32200
cargo check --locked --workspace
