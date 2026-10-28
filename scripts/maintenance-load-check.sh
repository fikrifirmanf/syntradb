#!/usr/bin/env bash
# Run the optional maintenance worker as the only applier under paced pgbench load,
# with an old snapshot held for the first half. The feed must never fill, no client
# may fail, and the worker must drain the feed once load stops. Uses a private
# copy of the production PostgreSQL install.
set -euo pipefail
SYNTRA_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$SYNTRA_ROOT"
SECONDS_TOTAL="${1:-300}"
RATE="${2:-200}"
bash scripts/setup-production.sh
export PGRX_HOME="$SYNTRA_ROOT/.pgrx"
export PATH="$SYNTRA_ROOT/.tools/bin:$PATH"
WORK="$SYNTRA_ROOT/target/maintenance-load-check/$(date +%Y%m%d-%H%M%S)-$$"
mkdir -p "$WORK"
cp -R .tools/postgresql-production/18.6/install "$WORK/pg"
BIN="$WORK/pg/bin"
(cd crates/syntra-pg && CARGO_TARGET_DIR="$SYNTRA_ROOT/target/upgrade-current" cargo pgrx install \
    --release --cargo=--locked --pg-config "$WORK/pg/bin/pg_config")

SOCKET="$(mktemp -d /tmp/syntra-mload.XXXXXX)"
DATA="$WORK/data"
"$BIN/initdb" -D "$DATA" --username=maint --auth=trust --no-locale --encoding=UTF8 >/dev/null
printf "\nlisten_addresses = ''\nunix_socket_directories = '%s'\nport = 28936\n" "$SOCKET" >>"$DATA/postgresql.conf"
printf "shared_preload_libraries = 'syntradb'\nsyntradb.maintenance_database = 'feeds'\n" >>"$DATA/postgresql.conf"
LOG="$WORK/postgres.log"
"$BIN/pg_ctl" -D "$DATA" -l "$LOG" -w -t 60 start >/dev/null
trap '"$BIN/pg_ctl" -D "$DATA" -w -t 30 stop -m fast >/dev/null 2>&1 || true' EXIT
q() { "$BIN/psql" -h "$SOCKET" -p 28936 -U maint -d feeds -v ON_ERROR_STOP=1 -AtqX -c "$1"; }
fail() {
    echo "maintenance load check failed: $1" >&2
    exit 1
}
READ="SELECT s.route||':'||(ROW(s.rows,s.non_nulls,s.sum,s.min,s.max) IS NOT DISTINCT FROM ROW(n.rows,n.non_nulls,n.sum,n.min,n.max))
    FROM syntra_checkpoint_int4_summary('feed','v',true) AS s,
    (SELECT count(*) AS rows,count(v) AS non_nulls,sum(v),min(v),max(v) FROM feed) AS n"

"$BIN/psql" -h "$SOCKET" -p 28936 -U maint -d postgres -v ON_ERROR_STOP=1 -AtqX -c "CREATE DATABASE feeds" >/dev/null
# The worker exits until the database exists and is restarted 10 s later.
tries=0
until grep -q 'syntradb maintenance started in database "feeds"' "$LOG"; do
    tries=$((tries + 1))
    ((tries <= 150)) || fail "the worker did not connect within 30 s of the database being created"
    sleep 0.2
done
RUNNING_FROM="$(wc -l <"$LOG" | tr -d ' ')"
q "CREATE EXTENSION syntradb"
q "CREATE TABLE feed(id int4 PRIMARY KEY,v int4); INSERT INTO feed SELECT g,g FROM generate_series(1,10000) AS g"
q "SELECT syntra_seed_capture('feed','id','v',4096)" >/dev/null
printf '\\set id random(1, 10000)\nUPDATE feed SET v = v + 1 WHERE id = :id;\n' >"$WORK/write.sql"
printf 'SELECT rows FROM syntra_checkpoint_int4_summary(%s, %s, true);\n' "'feed'" "'v'" >"$WORK/read.sql"

# The old snapshot is held for the first half of the run.
"$BIN/psql" -h "$SOCKET" -p 28936 -U maint -d feeds -v ON_ERROR_STOP=1 -AtqX \
    -c "BEGIN ISOLATION LEVEL REPEATABLE READ; SELECT count(*) FROM feed; SELECT pg_sleep($((SECONDS_TOTAL / 2))); COMMIT;" >/dev/null &
HOLDER=$!
"$BIN/pgbench" -h "$SOCKET" -p 28936 -U maint -n -c 4 -j 4 -T "$SECONDS_TOTAL" -R "$RATE" -P 30 \
    -f "$WORK/write.sql@9" -f "$WORK/read.sql@1" feeds >"$WORK/pgbench.log" 2>&1 &
BENCH=$!
MAX_PENDING=0
MAX_AGE=0
while kill -0 "$BENCH" 2>/dev/null; do
    PENDING="$(q "SELECT pending_events FROM syntra_capture_lag('feed')")"
    AGE="$(q "SELECT syntra_cleanup_horizon_age()")"
    echo "$(date +%s) pending=$PENDING age=$AGE" >>"$WORK/samples.log"
    ((PENDING > MAX_PENDING)) && MAX_PENDING=$PENDING
    ((AGE > MAX_AGE)) && MAX_AGE=$AGE
    sleep 1
done
wait "$BENCH" || fail "pgbench exited with an error; see $WORK/pgbench.log"
wait "$HOLDER"
grep -q "number of failed transactions: 0 " "$WORK/pgbench.log" || fail "pgbench reported failed transactions"
if grep -q "aborted" "$WORK/pgbench.log"; then fail "a pgbench client aborted"; fi
if grep -q "capture log is full" "$LOG"; then fail "the feed filled during the run"; fi
((MAX_PENDING < 2048)) || fail "pending events reached $MAX_PENDING, half the feed"

tries=0
until [[ "$(q "SELECT applied IS NOT NULL AND pending_events=0 FROM syntra_capture_lag('feed')")" == "t" ]]; do
    tries=$((tries + 1))
    ((tries <= 150)) || fail "the worker did not drain the feed within 30 s of the load ending"
    sleep 0.2
done
[[ "$(q "$READ")" == "checkpoint:true" ]] || fail "drained read: $(q "$READ")"
if tail -n +"$RUNNING_FROM" "$LOG" | grep -q "syntradb maintenance.*exited with exit code\|syntradb maintenance skipped"; then
    fail "the worker exited or skipped the feed"
fi
TPS="$(sed -n 's/^tps = \([0-9.]*\).*/\1/p' "$WORK/pgbench.log")"
LAT="$(sed -n 's/^latency average = \(.*\)$/\1/p' "$WORK/pgbench.log")"
echo "maintenance load check passed: ${SECONDS_TOTAL} s at ${RATE} tps (measured $TPS, latency average $LAT), worker as the only applier, max pending $MAX_PENDING, max horizon age $MAX_AGE; work dir $WORK"
