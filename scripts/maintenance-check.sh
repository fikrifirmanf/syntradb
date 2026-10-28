#!/usr/bin/env bash
# Run the optional maintenance worker in a private cluster and check that it
# drains captured sources, skips a busy lock, isolates a broken source, and
# stops cleanly. Uses a private copy of the production PostgreSQL install.
set -euo pipefail
SYNTRA_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$SYNTRA_ROOT"
bash scripts/setup-production.sh
export PGRX_HOME="$SYNTRA_ROOT/.pgrx"
export PATH="$SYNTRA_ROOT/.tools/bin:$PATH"
WORK="$SYNTRA_ROOT/target/maintenance-check/$(date +%Y%m%d-%H%M%S)-$$"
mkdir -p "$WORK"
cp -R .tools/postgresql-production/18.6/install "$WORK/pg"
BIN="$WORK/pg/bin"
(cd crates/syntra-pg && CARGO_TARGET_DIR="$SYNTRA_ROOT/target/upgrade-current" cargo pgrx install \
    --release --cargo=--locked --pg-config "$WORK/pg/bin/pg_config")

SOCKET="$(mktemp -d /tmp/syntra-maint.XXXXXX)"
DATA="$WORK/data"
"$BIN/initdb" -D "$DATA" --username=maint --auth=trust --no-locale --encoding=UTF8 >/dev/null
printf "\nlisten_addresses = ''\nunix_socket_directories = '%s'\nport = 28935\n" "$SOCKET" >>"$DATA/postgresql.conf"
printf "shared_preload_libraries = 'syntradb'\n" >>"$DATA/postgresql.conf"
LOG="$WORK/postgres.log"
"$BIN/pg_ctl" -D "$DATA" -l "$LOG" -w -t 60 start >/dev/null
trap '"$BIN/pg_ctl" -D "$DATA" -w -t 30 stop -m fast >/dev/null 2>&1 || true' EXIT
q() { "$BIN/psql" -h "$SOCKET" -p 28935 -U maint -d "${DB:-feeds}" -v ON_ERROR_STOP=1 -AtqX -c "$1"; }
fail() {
    echo "maintenance check failed: $1" >&2
    exit 1
}
# Poll until the query returns the expected value, for at most 30 s.
wait_for() {
    local tries=0
    until [[ "$(q "$2")" == "$3" ]]; do
        tries=$((tries + 1))
        if ((tries > 150)); then
            fail "$1: got '$(q "$2")', expected '$3' within 30 s"
        fi
        sleep 0.2
    done
}
READ="SELECT s.route||':'||(ROW(s.rows,s.non_nulls,s.sum,s.min,s.max) IS NOT DISTINCT FROM ROW(n.rows,n.non_nulls,n.sum,n.min,n.max))
    FROM syntra_checkpoint_int4_summary('feed','v',true) AS s,
    (SELECT count(*) AS rows,count(v) AS non_nulls,sum(v),min(v),max(v) FROM feed) AS n"
DRAINED="SELECT applied IS NOT NULL AND pending_events=0 FROM syntra_capture_lag('feed')"

# Preloaded without a maintenance database: no worker, extension still usable.
DB=postgres q "CREATE DATABASE feeds" >/dev/null
q "CREATE EXTENSION syntradb"
[[ "$(q "SELECT current_setting('syntradb.maintenance_database')")" == "" ]] || fail "maintenance database set by default"
[[ "$(q "SELECT count(*) FROM pg_stat_activity WHERE backend_type='syntradb maintenance'")" == "0" ]] \
    || fail "a worker started without a maintenance database"
"$BIN/pg_ctl" -D "$DATA" -w -t 30 stop -m fast >/dev/null
printf "syntradb.maintenance_database = 'feeds'\nsyntradb.maintenance_interval_ms = 200\n" >>"$DATA/postgresql.conf"
"$BIN/pg_ctl" -D "$DATA" -l "$LOG" -w -t 60 start >/dev/null
wait_for "worker running" "SELECT count(*) FROM pg_stat_activity WHERE backend_type='syntradb maintenance'" "1"
RUNNING_FROM="$(wc -l <"$LOG" | tr -d ' ')"
q "CREATE TABLE feed(id int4 PRIMARY KEY,v int4); INSERT INTO feed SELECT g,g FROM generate_series(1,1000) AS g"
q "SELECT syntra_seed_capture('feed','id','v',4096)" >/dev/null
# Nobody calls apply here: the first checkpoint and every later drain are the worker's.
wait_for "first checkpoint" "$DRAINED" "t"
q "UPDATE feed SET v=-v WHERE id<=200"
q "DELETE FROM feed WHERE id>990"
wait_for "drain after writes" "$DRAINED" "t"
[[ "$(q "$READ")" == "checkpoint:true" ]] || fail "drained read: $(q "$READ")"

# A session that holds the maintenance lock makes the worker skip, not fail.
"$BIN/psql" -h "$SOCKET" -p 28935 -U maint -d feeds -v ON_ERROR_STOP=1 -AtqX \
    -c "BEGIN; LOCK TABLE syntra.capture_checkpoints IN SHARE ROW EXCLUSIVE MODE; SELECT pg_sleep(2); COMMIT;" >/dev/null &
HOLDER=$!
sleep 0.5
q "UPDATE feed SET v=0 WHERE id<=10"
[[ "$(q "SELECT pending_events FROM syntra_capture_lag('feed')")" == "10" ]] || fail "lock holder did not block maintenance"
wait "$HOLDER"
wait_for "drain after the lock is released" "$DRAINED" "t"

# A source whose binding no longer matches must not stop the others.
q "CREATE TABLE broken(id int4 PRIMARY KEY,v int4); INSERT INTO broken VALUES(1,1)"
q "SELECT syntra_seed_capture('broken','id','v',64)" >/dev/null
q "UPDATE syntra.capture_bindings SET source_identity='tampered' WHERE source_relation='broken'::regclass"
q "UPDATE feed SET v=v+1 WHERE id BETWEEN 500 AND 520"
wait_for "drain beside a broken source" "$DRAINED" "t"
BROKEN="$(q "SELECT 'broken'::regclass::oid")"
grep -q "syntradb maintenance skipped relation $BROKEN" "$LOG" || fail "no warning for the broken source"

if tail -n +"$RUNNING_FROM" "$LOG" | grep -q "syntradb maintenance.*exited with exit code"; then
    fail "the worker exited during the run"
fi
"$BIN/pg_ctl" -D "$DATA" -w -t 30 stop -m fast >/dev/null
grep -q "syntradb maintenance stopped" "$LOG" || fail "the worker did not log a clean stop"

# Two databases, one worker each. The second one's feed drains with nobody calling apply.
"$BIN/pg_ctl" -D "$DATA" -l "$LOG" -w -t 60 start >/dev/null
DB=postgres q "CREATE DATABASE reports" >/dev/null
DB=reports q "CREATE EXTENSION syntradb"
DB=reports q "CREATE TABLE feed(id int4 PRIMARY KEY,v int4); INSERT INTO feed SELECT g,g FROM generate_series(1,100) AS g"
DB=reports q "SELECT syntra_seed_capture('feed','id','v',64)" >/dev/null
"$BIN/pg_ctl" -D "$DATA" -w -t 30 stop -m fast >/dev/null
printf "syntradb.maintenance_database = 'feeds, reports'\n" >>"$DATA/postgresql.conf"
"$BIN/pg_ctl" -D "$DATA" -l "$LOG" -w -t 60 start >/dev/null
wait_for "two workers" "SELECT string_agg(datname,',' ORDER BY datname) FROM pg_stat_activity WHERE backend_type='syntradb maintenance'" "feeds,reports"
DB=reports q "UPDATE feed SET v=-v WHERE id<=30"
q "UPDATE feed SET v=v+1 WHERE id<=30"
DB=reports wait_for "drain in the second database" "$DRAINED" "t"
wait_for "drain in the first database" "$DRAINED" "t"
[[ "$(DB=reports q "$READ")" == "checkpoint:true" ]] || fail "second database drained read: $(DB=reports q "$READ")"
"$BIN/pg_ctl" -D "$DATA" -w -t 30 stop -m fast >/dev/null

# A malformed list stops the server from starting instead of skipping a database.
printf "syntradb.maintenance_database = 'feeds,feeds'\n" >>"$DATA/postgresql.conf"
if "$BIN/pg_ctl" -D "$DATA" -l "$LOG" -w -t 30 start >/dev/null 2>&1; then
    fail "the server started with a database listed twice"
fi
grep -q "invalid syntradb.maintenance_database: database listed twice" "$LOG" \
    || fail "no error for a database listed twice"
echo "maintenance check passed: worker drained a feed, skipped a held lock, isolated a broken source, stopped cleanly, ran one worker per listed database and refused a malformed list; work dir $WORK"
