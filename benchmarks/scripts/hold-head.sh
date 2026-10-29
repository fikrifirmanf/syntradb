#!/usr/bin/env bash
# Isolates the feed head row. With one old snapshot held, adds versions to the
# head row only (a no-op UPDATE as superuser), then times 2,000 captured
# single-row updates and 2,000 plain ones. Repeats at growing version counts.
set -euo pipefail
SYNTRA_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$SYNTRA_ROOT"
bash scripts/setup-production.sh >/dev/null
export PGRX_HOME="$SYNTRA_ROOT/.pgrx"
export PATH="$SYNTRA_ROOT/.tools/bin:$PATH"
WORK="$SYNTRA_ROOT/target/hold-head/$(date +%Y%m%d-%H%M%S)-$$"
mkdir -p "$WORK"
cp -R .tools/postgresql-production/18.6/install "$WORK/pg"
BIN="$WORK/pg/bin"
(cd crates/syntra-pg && CARGO_TARGET_DIR="$SYNTRA_ROOT/target/upgrade-current" cargo pgrx install \
    --release --cargo=--locked --pg-config "$WORK/pg/bin/pg_config") >/dev/null 2>&1
SOCKET="$(mktemp -d /tmp/syntra-head.XXXXXX)"
PORT=28962
"$BIN/initdb" -D "$WORK/data" --username=hold --auth=trust --no-locale --encoding=UTF8 >/dev/null
printf "\nlisten_addresses = ''\nunix_socket_directories = '%s'\nport = %s\n" "$SOCKET" "$PORT" >>"$WORK/data/postgresql.conf"
HOLDER=""
cleanup() {
    if [[ -n "$HOLDER" ]]; then kill "$HOLDER" 2>/dev/null || true; fi
    "$BIN/pg_ctl" -D "$WORK/data" -w -t 30 stop -m immediate >/dev/null 2>&1 || true
}
trap cleanup EXIT
"$BIN/pg_ctl" -D "$WORK/data" -l "$WORK/data.log" -w -t 60 start >/dev/null
q() { "$BIN/psql" -h "$SOCKET" -p "$PORT" -U hold -d feeds -v ON_ERROR_STOP=1 -AtqX -c "$1"; }
"$BIN/createdb" -h "$SOCKET" -p "$PORT" -U hold feeds
q "CREATE EXTENSION syntradb"
for t in plain captured; do
    q "CREATE TABLE $t(id int4 PRIMARY KEY, v int4); INSERT INTO $t SELECT g, 1000 FROM generate_series(1,10000) AS g"
done
q "SELECT syntra_seed_capture('captured','id','v',4096)"
q "SELECT syntra_apply_capture('captured')"
"$BIN/psql" -h "$SOCKET" -p "$PORT" -U hold -d feeds -AtqX \
    -c "BEGIN ISOLATION LEVEL REPEATABLE READ" -c "SELECT count(*) FROM plain" -c "SELECT pg_sleep(86400)" >/dev/null 2>&1 &
HOLDER=$!
/bin/sleep 2
for t in plain captured; do
    printf '\\set a random(1, 10000)\nUPDATE %s SET v = v + 1 WHERE id = :a;\n' "$t" >"$WORK/$t.sql"
done
time_writes() {
    "$BIN/pgbench" -h "$SOCKET" -p "$PORT" -U hold -n -c 1 -t 2000 -f "$WORK/$1.sql" feeds 2>&1 | awk '/^latency average/{print $4}'
}
echo "head_versions plain_ms captured_ms"
added=0
for target in 0 2000 5000 10000 20000; do
    while ((added < target)); do
        q "DO \$\$ BEGIN FOR i IN 1..1000 LOOP UPDATE syntra.capture_heads SET live_rows = live_rows WHERE source_relation = 'captured'::regclass; END LOOP; END \$\$"
        added=$((added + 1000))
    done
    plain="$(time_writes plain)"
    captured="$(time_writes captured)"
    # Drain so the feed never fills; this adds its own few head versions.
    q "SELECT syntra_apply_prune_capture('captured')" >/dev/null
    echo "$added $plain $captured"
done
echo "work dir $WORK"
