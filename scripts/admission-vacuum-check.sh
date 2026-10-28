#!/usr/bin/env bash
# Nonblocking admission must not fail because vacuum is visiting a catalog.
# VACUUM and ANALYZE hold SHARE UPDATE EXCLUSIVE, which conflicts with the
# SHARE ROW EXCLUSIVE admission lock. Another session holds that mode on each
# admission catalog, briefly and then for longer than the retry window:
#   brief hold (300 ms): checkpoint apply, capture seed and trusted publication succeed;
#   long hold (3 s):     each fails with the documented busy error (SQLSTATE 55P03).
# A third case holds the admission mode itself, as a concurrent caller does: the
# attempt must fail as busy at once, without waiting out the vacuum retry.
# Uses a private copy of the production PostgreSQL install.
#   admission-vacuum-check.sh
set -euo pipefail
SYNTRA_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$SYNTRA_ROOT"
bash scripts/setup-production.sh
export PGRX_HOME="$SYNTRA_ROOT/.pgrx"
export PATH="$SYNTRA_ROOT/.tools/bin:$PATH"
WORK="$SYNTRA_ROOT/target/admission-vacuum-check/$(date +%Y%m%d-%H%M%S)-$$"
mkdir -p "$WORK"
cp -R .tools/postgresql-production/18.6/install "$WORK/pg"
BIN="$WORK/pg/bin"
(cd crates/syntra-pg && CARGO_TARGET_DIR="$SYNTRA_ROOT/target/upgrade-current" cargo pgrx install \
    --release --cargo=--locked --pg-config "$WORK/pg/bin/pg_config")
SOCKET="$(mktemp -d /tmp/syntra-admit.XXXXXX)"
PORT=28945
"$BIN/initdb" -D "$WORK/data" --username=admitter --auth=trust --no-locale --encoding=UTF8 >/dev/null
# Autovacuum off, so only the held lock below can conflict with admission.
printf "\nlisten_addresses = ''\nunix_socket_directories = '%s'\nport = %s\nautovacuum = off\n" "$SOCKET" "$PORT" >>"$WORK/data/postgresql.conf"
"$BIN/pg_ctl" -D "$WORK/data" -l "$WORK/postgres.log" -w -t 60 start >/dev/null
trap '"$BIN/pg_ctl" -D "$WORK/data" -w -t 30 stop -m immediate >/dev/null 2>&1 || true' EXIT
q() { "$BIN/psql" -h "$SOCKET" -p "$PORT" -U admitter -d admit -v ON_ERROR_STOP=1 -AtqX -c "$1"; }
fail() { echo "admission vacuum check failed: $1" >&2; exit 1; }
"$BIN/psql" -h "$SOCKET" -p "$PORT" -U admitter -d postgres -v ON_ERROR_STOP=1 -AtqX -c "CREATE DATABASE admit" >/dev/null
q "CREATE EXTENSION syntradb"
q "CREATE TABLE feed(id int4 PRIMARY KEY,v int4); INSERT INTO feed SELECT g,g FROM generate_series(1,1000) AS g"
q "SELECT syntra_seed_capture('feed','id','v',4096)" >/dev/null
q "SELECT syntra_apply_capture('feed')" >/dev/null
q "CREATE TABLE trusted(v int4); INSERT INTO trusted SELECT g FROM generate_series(1,1000) AS g"
q "SELECT syntra_track_source('trusted','v')" >/dev/null

# Hold a lock on a catalog for a number of seconds, SHARE UPDATE EXCLUSIVE as VACUUM
# does unless a mode is given, and wait until it is granted before returning.
hold() {
    local catalog="$1" seconds="$2" mode="${3:-SHARE UPDATE EXCLUSIVE}"
    local granted
    granted="$(echo "$mode" | python3 -c 'import sys; print("".join(w.capitalize() for w in sys.stdin.read().split())+"Lock")')"
    # unslop-ignore NO_SQL_INTERPOLATION -- catalog, seconds and mode are fixed literals in this script
    q "BEGIN; LOCK TABLE syntra.$catalog IN $mode MODE; SELECT pg_sleep($seconds); COMMIT" >/dev/null &
    HOLDER=$!
    # unslop-ignore NO_SQL_INTERPOLATION -- catalog and granted are fixed literals in this script
    until [[ "$(q "SELECT count(*) FROM pg_locks WHERE relation='syntra.$catalog'::regclass AND mode='$granted' AND granted")" == 1 ]]; do
        sleep 0.01
    done
}
# Run one admission statement; print "ok" or the error message.
attempt() {
    local out
    if out="$(q "$1" 2>&1 >/dev/null)"; then echo ok; else echo "${out#ERROR:  }"; fi
}
# $2 names the attempt, so each capture seed gets its own table.
statement() {
    case "$1" in
        capture_checkpoints) echo "UPDATE feed SET v=v+1 WHERE id=1; SELECT syntra_apply_capture('feed')" ;;
        capture_heads) echo "CREATE TABLE extra_$2(id int4 PRIMARY KEY,v int4); SELECT syntra_seed_capture('extra_$2','id','v',16)" ;;
        trusted_int4_projections) echo "UPDATE trusted SET v=v+1 WHERE v=1; SELECT syntra_publish_trusted_int4('trusted','v')" ;;
    esac
}
busy_message() {
    case "$1" in
        capture_checkpoints) echo "checkpoint maintenance is busy; retry after its transaction ends" ;;
        capture_heads) echo "capture setup is busy; retry after active capture transactions end" ;;
        trusted_int4_projections) echo "another trusted publisher is active; retry after its transaction ends" ;;
    esac
}
for catalog in capture_checkpoints capture_heads trusted_int4_projections; do
    hold "$catalog" 0.3
    result="$(attempt "$(statement "$catalog" brief)")"
    wait "$HOLDER"
    [[ "$result" == ok ]] || fail "$catalog held 300 ms by a vacuum-mode lock: $result"
    echo "$catalog: brief vacuum-mode hold, admission succeeded"

    hold "$catalog" 3
    started=$(python3 -c 'import time; print(time.monotonic())')
    result="$(attempt "$(statement "$catalog" long)")"
    waited=$(python3 -c "import time; print(round((time.monotonic()-$started)*1000))")
    wait "$HOLDER"
    [[ "$result" == "$(busy_message "$catalog")" ]] || fail "$catalog held 3 s: expected the busy error, got: $result"
    echo "$catalog: 3 s vacuum-mode hold, busy error after ${waited} ms"

    hold "$catalog" 3 "SHARE ROW EXCLUSIVE"
    started=$(python3 -c 'import time; print(time.monotonic())')
    result="$(attempt "$(statement "$catalog" peer)")"
    waited=$(python3 -c "import time; print(round((time.monotonic()-$started)*1000))")
    wait "$HOLDER"
    [[ "$result" == "$(busy_message "$catalog")" ]] || fail "$catalog held by another admission: expected the busy error, got: $result"
    ((waited < 200)) || fail "$catalog held by another admission: busy error took ${waited} ms, so admission waited for it"
    echo "$catalog: held by another admission, busy error after ${waited} ms"
done
echo "admission vacuum check passed; work dir $WORK"
