#!/usr/bin/env bash
# Paced at RATE transactions per second in each round.
# Two identical 10,000-row tables, one captured, one plain, take the same
# paced two-row transfers in alternating rounds while one repeatable-read
# session holds an old snapshot. The maintenance worker drains the captured
# feed. Prints per-round tps and dead tuples for each table.
set -euo pipefail
ROUNDS="${1:-20}"
SECONDS_PER_ROUND="${2:-30}"
RATE="${3:-200}"
SYNTRA_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$SYNTRA_ROOT"
bash scripts/setup-production.sh >/dev/null
export PGRX_HOME="$SYNTRA_ROOT/.pgrx"
export PATH="$SYNTRA_ROOT/.tools/bin:$PATH"
WORK="$SYNTRA_ROOT/target/hold-control/$(date +%Y%m%d-%H%M%S)-$$"
mkdir -p "$WORK"
cp -R .tools/postgresql-production/18.6/install "$WORK/pg"
BIN="$WORK/pg/bin"
(cd crates/syntra-pg && CARGO_TARGET_DIR="$SYNTRA_ROOT/target/upgrade-current" cargo pgrx install \
    --release --cargo=--locked --pg-config "$WORK/pg/bin/pg_config") >/dev/null 2>&1
SOCKET="$(mktemp -d /tmp/syntra-hold.XXXXXX)"
PORT=28961
"$BIN/initdb" -D "$WORK/data" --username=hold --auth=trust --no-locale --encoding=UTF8 >/dev/null
printf "\nlisten_addresses = ''\nunix_socket_directories = '%s'\nport = %s\nshared_preload_libraries = 'syntradb'\nsyntradb.maintenance_database = 'feeds'\nsyntradb.maintenance_interval_ms = 200\n" \
    "$SOCKET" "$PORT" >>"$WORK/data/postgresql.conf"
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
    q "CREATE TABLE $t(id int4 PRIMARY KEY, v int4); INSERT INTO $t SELECT g, 1000 FROM generate_series(1,10000) AS g; ALTER TABLE $t SET (autovacuum_enabled = true)"
done
q "SELECT syntra_seed_capture('captured','id','v',4096)"
for t in plain captured; do
    cat >"$WORK/$t.sql" <<EOF
\set a random(1, 10000)
\set b random(1, 10000)
BEGIN;
UPDATE $t SET v = v - 1 WHERE id = :a;
UPDATE $t SET v = v + 1 WHERE id = :b;
COMMIT;
EOF
done
# The holder keeps one snapshot open for the whole run.
"$BIN/psql" -h "$SOCKET" -p "$PORT" -U hold -d feeds -AtqX \
    -c "BEGIN ISOLATION LEVEL REPEATABLE READ" -c "SELECT count(*) FROM plain" -c "SELECT pg_sleep(86400)" >/dev/null 2>&1 &
HOLDER=$!
/bin/sleep 2
echo "round table tps latency_ms failed dead_tuples horizon_age"
for round in $(seq 1 "$ROUNDS"); do
    for t in plain captured; do
        tps="$("$BIN/pgbench" -h "$SOCKET" -p "$PORT" -U hold -n -c 2 -j 2 -R "$RATE" -T "$SECONDS_PER_ROUND" -f "$WORK/$t.sql" feeds 2>&1 \
            | awk '/^tps = /{tps=$3} /^latency average/{lat=$4} /^number of failed/{fail=$5} END{print tps, lat, fail}')"
        dead="$(q "SELECT n_dead_tup FROM pg_stat_user_tables WHERE relname='$t'")"
        echo "$round $t $tps $dead $(q "SELECT syntra_cleanup_horizon_age()")"
    done
done
q "SELECT 'pending '||pending_events FROM syntra_capture_lag('captured')"
q "SELECT 'sums '||(SELECT sum(v) FROM plain)||' '||(SELECT sum(v) FROM captured)"
echo "work dir $WORK"
