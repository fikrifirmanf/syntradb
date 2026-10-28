#!/usr/bin/env bash
# Separate optimized PostgreSQL build; leaves the assertion-enabled dev build alone.
set -euo pipefail
SYNTRA_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SYNTRA_PG_VERSION=18.6
SYNTRA_PROD_ROOT="$SYNTRA_ROOT/.tools/postgresql-production/$SYNTRA_PG_VERSION"
SYNTRA_PROD_CONFIG="$SYNTRA_PROD_ROOT/install/bin/pg_config"
cd "$SYNTRA_ROOT"

if [[ ! -x .tools/bin/cargo-pgrx ]]; then
    echo "Run bash scripts/setup.sh first." >&2
    exit 1
fi
if command -v brew >/dev/null 2>&1; then
    SYNTRA_ICU_PREFIX="$(brew --prefix icu4c 2>/dev/null || true)"
    if [[ -d "$SYNTRA_ICU_PREFIX/lib/pkgconfig" ]]; then
        export PKG_CONFIG_PATH="$SYNTRA_ICU_PREFIX/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
    fi
fi

if [[ ! -x "$SYNTRA_PROD_CONFIG" ]]; then
    mkdir -p .tools/downloads "$SYNTRA_PROD_ROOT"
    SYNTRA_ARCHIVE="postgresql-$SYNTRA_PG_VERSION.tar.bz2"
    SYNTRA_URL="https://ftp.postgresql.org/pub/source/v$SYNTRA_PG_VERSION/$SYNTRA_ARCHIVE"
    curl --fail --location --retry 3 --connect-timeout 10 --max-time 300 "$SYNTRA_URL" -o ".tools/downloads/$SYNTRA_ARCHIVE"
    curl --fail --location --retry 3 --connect-timeout 10 --max-time 60 "$SYNTRA_URL.sha256" -o ".tools/downloads/$SYNTRA_ARCHIVE.sha256"
    (cd .tools/downloads && shasum -a 256 -c "$SYNTRA_ARCHIVE.sha256")
    if [[ ! -d "$SYNTRA_PROD_ROOT/postgresql-$SYNTRA_PG_VERSION" ]]; then
        tar -xjf ".tools/downloads/$SYNTRA_ARCHIVE" -C "$SYNTRA_PROD_ROOT"
    fi
    (
        cd "$SYNTRA_PROD_ROOT/postgresql-$SYNTRA_PG_VERSION"
        # Do not inherit the development build's assertion/randomization flags.
        env -u CPPFLAGS -u CFLAGS -u CXXFLAGS ./configure \
            --prefix="$SYNTRA_PROD_ROOT/install" --with-pgport=28918 \
            --disable-cassert --disable-debug CFLAGS=-O2
        make -j "${SYNTRA_BUILD_JOBS:-4}"
        make install
    )
fi

if [[ "$("$SYNTRA_PROD_CONFIG" --version)" != "PostgreSQL $SYNTRA_PG_VERSION" ]]; then
    echo "Production baseline requires PostgreSQL $SYNTRA_PG_VERSION." >&2
    exit 1
fi
SYNTRA_BUILD_FLAGS="$("$SYNTRA_PROD_CONFIG" --configure)"
if [[ "$SYNTRA_BUILD_FLAGS" != *--disable-cassert* || "$SYNTRA_BUILD_FLAGS" != *--disable-debug* || "$SYNTRA_BUILD_FLAGS" != *CFLAGS=-O2* ]]; then
    echo "Production baseline build flags do not match the pinned configuration." >&2
    exit 1
fi
