#!/usr/bin/env bash
# Downgrade a live database from the workspace release to 0.6.0 and back.
# 0.7.0 appends each partition delta as a row of capture_chunk_deltas; 0.6.0
# reads it inline from the chunk row, so the downgrade moves each partition's
# newest delta back. An apply that holds the maintenance lock when the downgrade
# starts must make it wait. Checks that reads match the heap after each step,
# that pending history applies on 0.6.0, the catalog against fresh 0.6.0 and
# current installs, and that deltas are appended again after a second update.
# On 0.8.0, the first step also removes the additive grouping function.
# Builds into a private copy of the production PostgreSQL install; the original
# is untouched.
set -euo pipefail
SYNTRA_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$SYNTRA_ROOT"
bash scripts/setup-production.sh
export PGRX_HOME="$SYNTRA_ROOT/.pgrx"
export PATH="$SYNTRA_ROOT/.tools/bin:$PATH"
OLD=0.6.0
CURRENT="$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)"
if [[ "$CURRENT" != 0.7.0 && "$CURRENT" != 0.8.0 ]]; then
    echo "downgrade check failed: this check covers 0.7.0 or 0.8.0 to 0.6.0, workspace is '$CURRENT'" >&2
    exit 1
fi
WORK="$SYNTRA_ROOT/target/downgrade-0.7.0-check/$(date +%Y%m%d-%H%M%S)-$$"
mkdir -p "$WORK/base"
cp -R .tools/postgresql-production/18.6/install "$WORK/pg"
PG_CONFIG="$WORK/pg/bin/pg_config"
BIN="$WORK/pg/bin"
tar xzf "build-support/upgrade-baselines/syntradb-$OLD-src.tgz" -C "$WORK/base"
install_from() {
    (cd "$1/crates/syntra-pg" && CARGO_TARGET_DIR="$2" cargo pgrx install --release \
        --cargo=--locked --pg-config "$PG_CONFIG")
}
install_old() { install_from "$WORK/base" "$SYNTRA_ROOT/target/upgrade-baseline-$OLD"; }
install_current() { install_from "$SYNTRA_ROOT" "$SYNTRA_ROOT/target/upgrade-current"; }

SOCKET="$(mktemp -d /tmp/syntra-downgrade7.XXXXXX)"
"$BIN/initdb" -D "$WORK/data" --username=downgrader --auth=trust --no-locale --encoding=UTF8 >/dev/null
printf "\nlisten_addresses = ''\nunix_socket_directories = '%s'\nport = 28949\n" "$SOCKET" \
    >>"$WORK/data/postgresql.conf"
start() { "$BIN/pg_ctl" -D "$WORK/data" -l "$WORK/postgres.log" -w -t 30 start >/dev/null; }
stop() { "$BIN/pg_ctl" -D "$WORK/data" -w -t 30 stop -m fast >/dev/null; }
trap 'stop 2>/dev/null || true' EXIT
q() { "$BIN/psql" -h "$SOCKET" -p 28949 -U downgrader -d "$1" -v ON_ERROR_STOP=1 -AtqX -c "$2"; }
expect() {
    if [[ "$2" != "$3" ]]; then
        echo "downgrade check failed: $1: got '$2', expected '$3'" >&2
        exit 1
    fi
}
now_ms() { python3 -c 'import time; print(time.monotonic_ns() // 1000000)'; }
READ="SELECT s.route||':'||(ROW(s.rows,s.non_nulls,s.sum,s.min,s.max) IS NOT DISTINCT FROM ROW(n.rows,n.non_nulls,n.sum,n.min,n.max))
    FROM syntra_checkpoint_int4_summary('feed','v',true) AS s,
    (SELECT count(*) AS rows,count(v) AS non_nulls,sum(v),min(v),max(v) FROM feed) AS n"
# Where each partition's delta is stored: inline in its chunk row, or appended.
DELTAS="SELECT (SELECT count(*) FROM syntra.capture_chunks WHERE delta IS NOT NULL)||':'
    ||(SELECT count(*) FROM pg_class WHERE oid=to_regclass('syntra.capture_chunk_deltas'))"
APPENDED="SELECT count(*) FROM syntra.capture_chunk_deltas"
FINGERPRINT="$(cat scripts/lib/catalog-fingerprint.sql)"

install_old
start
q postgres "CREATE DATABASE fresh_old"
# unslop-ignore NO_SQL_INTERPOLATION -- OLD is a literal; CREATE EXTENSION takes no bind parameters.
q fresh_old "CREATE EXTENSION syntradb VERSION '$OLD'"
q fresh_old "$FINGERPRINT" >"$WORK/fresh_old.catalog"
stop

install_current
start
q postgres "CREATE DATABASE downgraded"
q postgres "CREATE DATABASE fresh_current"
q downgraded "CREATE EXTENSION syntradb"
q downgraded "CREATE TABLE feed(id int4 PRIMARY KEY,v int4); INSERT INTO feed SELECT g,g FROM generate_series(1,2000) AS g"
q downgraded "SELECT syntra_seed_capture('feed','id','v',4096)" >/dev/null
q downgraded "SELECT syntra_apply_capture('feed')" >/dev/null
# Two applies over the same partitions: the second replaces the first deltas.
q downgraded "UPDATE feed SET v=-v WHERE id%250=0"
q downgraded "SELECT applied FROM syntra_apply_prune_capture('feed')" >/dev/null
q downgraded "UPDATE feed SET v=v-1 WHERE id%250=0"
q downgraded "SELECT applied FROM syntra_apply_prune_capture('feed')" >/dev/null
PARTITIONS="$(q downgraded "$APPENDED")"
if [ "$PARTITIONS" -lt 2 ]; then
    echo "downgrade check failed: only $PARTITIONS appended deltas on $CURRENT" >&2
    exit 1
fi
expect "deltas on $CURRENT" "$(q downgraded "$DELTAS")" "0:1"
expect "one delta row per partition" "$(q downgraded "SELECT count(*)=count(DISTINCT partition) FROM syntra.capture_chunk_deltas")" "t"
# Pending history, then an apply that holds the maintenance lock open across
# the start of the downgrade.
q downgraded "UPDATE feed SET v=v+7 WHERE id IN (1,2,3); DELETE FROM feed WHERE id=2000"
q downgraded "BEGIN; SELECT syntra_apply_capture('feed'); SELECT pg_sleep(3); COMMIT;" >/dev/null &
APPLIER=$!
sleep 0.5
q downgraded "UPDATE feed SET v=v+1 WHERE id=500"
STARTED=$(now_ms)
# unslop-ignore NO_SQL_INTERPOLATION -- OLD is a literal; ALTER EXTENSION takes no bind parameters.
q downgraded "ALTER EXTENSION syntradb UPDATE TO '$OLD'"
WAITED=$(( $(now_ms) - STARTED ))
wait "$APPLIER"
if [ "$WAITED" -lt 1500 ]; then
    echo "downgrade check failed: the downgrade finished in ${WAITED} ms while an apply held the maintenance lock" >&2
    exit 1
fi
# unslop-ignore NO_SQL_INTERPOLATION -- constant SQL; OLD is only the expected value.
expect "version" "$(q downgraded "SELECT extversion FROM pg_extension WHERE extname='syntradb'")" "$OLD"
INLINE="$(q downgraded "SELECT count(*) FROM syntra.capture_chunks WHERE delta IS NOT NULL")"
if [ "$INLINE" -lt "$PARTITIONS" ]; then
    echo "downgrade check failed: $INLINE inline deltas after the downgrade, $PARTITIONS appended before it" >&2
    exit 1
fi
expect "delta table dropped" "$(q downgraded "SELECT to_regclass('syntra.capture_chunk_deltas') IS NULL")" "t"
# unslop-ignore NO_SQL_INTERPOLATION -- READ is the constant query defined above.
expect "read with the $CURRENT library" "$(q downgraded "$READ")" "replay:true"
stop

install_old
start
# unslop-ignore NO_SQL_INTERPOLATION -- READ is the constant query defined above.
expect "replay on $OLD" "$(q downgraded "$READ")" "replay:true"
q downgraded "UPDATE feed SET v=v+5 WHERE id<=20"
q downgraded "SELECT applied FROM syntra_apply_prune_capture('feed')" >/dev/null
# unslop-ignore NO_SQL_INTERPOLATION -- READ is the constant query defined above.
expect "checkpoint on $OLD" "$(q downgraded "$READ")" "checkpoint:true"
q downgraded "$FINGERPRINT" >"$WORK/downgraded.catalog"
if ! diff -u "$WORK/fresh_old.catalog" "$WORK/downgraded.catalog" >"$WORK/old.diff"; then
    cat "$WORK/old.diff" >&2
    echo "downgrade check failed: downgraded catalog differs from a fresh $OLD install" >&2
    exit 1
fi
stop

install_current
start
# unslop-ignore NO_SQL_INTERPOLATION -- CURRENT is checked against supported versions above; ALTER EXTENSION takes no bind parameters.
q downgraded "ALTER EXTENSION syntradb UPDATE TO '$CURRENT'"
q fresh_current "CREATE EXTENSION syntradb"
q downgraded "$FINGERPRINT" >"$WORK/upgraded.catalog"
q fresh_current "$FINGERPRINT" >"$WORK/fresh_current.catalog"
if ! diff -u "$WORK/fresh_current.catalog" "$WORK/upgraded.catalog" >"$WORK/current.diff"; then
    cat "$WORK/current.diff" >&2
    # unslop-ignore NO_SQL_INTERPOLATION -- an error message, not SQL.
    echo "downgrade check failed: catalog after the second update differs from a fresh $CURRENT install" >&2
    exit 1
fi
# unslop-ignore NO_SQL_INTERPOLATION -- READ is the constant query defined above.
expect "read after the second update" "$(q downgraded "$READ")" "checkpoint:true"
q downgraded "UPDATE feed SET v=v-1 WHERE id%250=0"
q downgraded "SELECT applied FROM syntra_apply_prune_capture('feed')" >/dev/null
expect "appended again after the second update" "$(q downgraded "SELECT count(*)>0 FROM syntra.capture_chunk_deltas")" "t"
# unslop-ignore NO_SQL_INTERPOLATION -- READ is the constant query defined above.
expect "checkpoint after the second update" "$(q downgraded "$READ")" "checkpoint:true"
echo "downgrade check passed: $CURRENT -> $OLD -> $CURRENT; the downgrade waited ${WAITED} ms for an apply holding the maintenance lock and moved $PARTITIONS appended deltas inline, $(wc -l <"$WORK/fresh_old.catalog" | tr -d ' ') catalog entries identical to a fresh $OLD install, $(wc -l <"$WORK/fresh_current.catalog" | tr -d ' ') to a fresh $CURRENT install after the second update; work dir $WORK"
