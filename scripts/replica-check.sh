#!/usr/bin/env bash
# Run a streaming standby of a primary with captured sources. Checkpoint reads on
# the standby must match its heap, the maintenance worker must not run there, and
# after promotion capture, apply and the worker must work on the new primary.
# Uses a private copy of the production PostgreSQL install.
#   replica-check.sh [int2|int4|int8|numeric]
# With int2, pending history adds keys and values at the int2 limits; int2 is
# stored at 4 bytes and read through the int4 summary.
# With int8, every value starts past the int4 range and pending history adds keys
# past it.
# With numeric, keys are int4 and values numeric(12,2), stored as 8-byte integers
# of hundredths; pending history adds values at the column's limits.
set -euo pipefail
SYNTRA_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$SYNTRA_ROOT"
TYPE="${1:-int4}"
# int2 is stored at 4 bytes: the binding records 4/4 and the int4 summary reads it.
KEY="$TYPE" VALUE="$TYPE" READER="${TYPE/int2/int4}" STEP=1
case "$TYPE" in
int2 | int4) SEED="g" ;;
int8) SEED="g+5000000000" ;;
numeric) KEY=int4 VALUE="numeric(12,2)" READER=numeric STEP=0.01 SEED="g*1.25-3000.5" ;;
*) echo "replica check: type must be int2, int4, int8 or numeric" >&2; exit 2 ;;
esac
if [[ "$READER" == numeric ]]; then WIDTHS=4/8; else WIDTHS="${READER#int}/${READER#int}"; fi
bash scripts/setup-production.sh
export PGRX_HOME="$SYNTRA_ROOT/.pgrx"
export PATH="$SYNTRA_ROOT/.tools/bin:$PATH"
WORK="$SYNTRA_ROOT/target/replica-check/$(date +%Y%m%d-%H%M%S)-$$"
mkdir -p "$WORK"
cp -R .tools/postgresql-production/18.6/install "$WORK/pg"
BIN="$WORK/pg/bin"
(cd crates/syntra-pg && CARGO_TARGET_DIR="$SYNTRA_ROOT/target/upgrade-current" cargo pgrx install \
    --release --cargo=--locked --pg-config "$WORK/pg/bin/pg_config")

SOCKET="$(mktemp -d /tmp/syntra-replica.XXXXXX)"
PRIMARY=28938
STANDBY=28939
"$BIN/initdb" -D "$WORK/primary" --username=replica --auth=trust --no-locale --encoding=UTF8 >/dev/null
printf "\nlisten_addresses = ''\nunix_socket_directories = '%s'\nport = %s\nwal_level = replica\n" "$SOCKET" "$PRIMARY" \
    >>"$WORK/primary/postgresql.conf"
printf "shared_preload_libraries = 'syntradb'\nsyntradb.maintenance_database = 'feeds'\nsyntradb.maintenance_interval_ms = 200\n" \
    >>"$WORK/primary/postgresql.conf"
printf "local replication replica trust\n" >>"$WORK/primary/pg_hba.conf"
RUNNING=()
start() { "$BIN/pg_ctl" -D "$1" -l "$1.log" -w -t 60 start >/dev/null; RUNNING+=("$1"); }
cleanup() { for d in "${RUNNING[@]}"; do "$BIN/pg_ctl" -D "$d" -w -t 30 stop -m fast >/dev/null 2>&1 || true; done; }
trap cleanup EXIT
q() { "$BIN/psql" -h "$SOCKET" -p "$1" -U replica -d feeds -v ON_ERROR_STOP=1 -AtqX -c "$2"; }
fail() {
    echo "replica check failed: $1" >&2
    exit 1
}
# Poll until the query on port $2 returns the expected value, for at most 30 s.
wait_for() {
    local tries=0
    until [[ "$(q "$2" "$3")" == "$4" ]]; do
        tries=$((tries + 1))
        ((tries <= 150)) || fail "$1: got '$(q "$2" "$3")', expected '$4' within 30 s"
        sleep 0.2
    done
}
# unslop-ignore NO_SQL_INTERPOLATION -- READER is one of three literals, chosen above.
READ="SELECT s.route||':'||(ROW(s.rows,s.non_nulls,s.sum,s.min,s.max) IS NOT DISTINCT FROM ROW(n.rows,n.non_nulls,n.sum,n.min,n.max))
    FROM syntra_checkpoint_${READER}_summary('feed','v',true) AS s,
    (SELECT count(*) AS rows,count(v) AS non_nulls,sum(v),min(v),max(v) FROM feed) AS n"
WORKERS="SELECT count(*) FROM pg_stat_activity WHERE backend_type='syntradb maintenance'"
DRAINED="SELECT applied IS NOT NULL AND pending_events=0 FROM syntra_capture_lag('feed')"
# The standby has replayed everything the primary wrote so far.
caught_up() {
    local lsn
    lsn="$(q "$PRIMARY" "SELECT pg_current_wal_lsn()")"
    [[ "$lsn" =~ ^[0-9A-F]+/[0-9A-F]+$ ]] || fail "unexpected WAL position '$lsn'"
    # unslop-ignore NO_SQL_INTERPOLATION -- lsn is checked as a WAL position just above.
    wait_for "standby replay" "$STANDBY" "SELECT pg_last_wal_replay_lsn() >= '$lsn'::pg_lsn" "t"
}

start "$WORK/primary"
"$BIN/psql" -h "$SOCKET" -p "$PRIMARY" -U replica -d postgres -v ON_ERROR_STOP=1 -AtqX -c "CREATE DATABASE feeds" >/dev/null
q "$PRIMARY" "CREATE EXTENSION syntradb"
# unslop-ignore NO_SQL_INTERPOLATION -- KEY, VALUE and SEED are literals chosen above.
q "$PRIMARY" "CREATE TABLE feed(id $KEY PRIMARY KEY,v $VALUE); INSERT INTO feed SELECT g,$SEED FROM generate_series(1,5000) AS g"
q "$PRIMARY" "SELECT syntra_seed_capture('feed','id','v',4096)" >/dev/null
# unslop-ignore NO_SQL_INTERPOLATION -- constant SQL; WIDTHS is only the expected value.
[[ "$(q "$PRIMARY" "SELECT key_width||'/'||value_width FROM syntra.capture_bindings")" == "$WIDTHS" ]] \
    || fail "binding widths for $TYPE"
wait_for "first checkpoint on the primary" "$PRIMARY" "$DRAINED" "t"

"$BIN/pg_basebackup" -h "$SOCKET" -p "$PRIMARY" -U replica -D "$WORK/standby" -X stream -R -c fast
sed -i.bak "s/^port = $PRIMARY$/port = $STANDBY/" "$WORK/standby/postgresql.conf"
chmod 700 "$WORK/standby"
start "$WORK/standby"
[[ "$(q "$STANDBY" "SELECT pg_is_in_recovery()")" == "t" ]] || fail "standby is not in recovery"
[[ "$(q "$STANDBY" "$WORKERS")" == "0" ]] || fail "a maintenance worker runs on the standby"

# Applied history: the standby reads the replicated checkpoint.
q "$PRIMARY" "UPDATE feed SET v=-v WHERE id<=500"
wait_for "drain on the primary" "$PRIMARY" "$DRAINED" "t"
caught_up
[[ "$(q "$STANDBY" "$READ")" == "checkpoint:true" ]] || fail "standby checkpoint read: $(q "$STANDBY" "$READ")"
# Pending history: hold the primary's maintenance lock so the worker cannot apply,
# then read on the standby while the events are still pending.
"$BIN/psql" -h "$SOCKET" -p "$PRIMARY" -U replica -d feeds -v ON_ERROR_STOP=1 -AtqX \
    -c "BEGIN; LOCK TABLE syntra.capture_checkpoints IN SHARE ROW EXCLUSIVE MODE; SELECT pg_sleep(3); COMMIT;" >/dev/null &
HOLDER=$!
sleep 0.5
q "$PRIMARY" "UPDATE feed SET v=v+7 WHERE id BETWEEN 1000 AND 1100; DELETE FROM feed WHERE id>4990"
if [[ "$TYPE" == int8 ]]; then
    q "$PRIMARY" "INSERT INTO feed SELECT 6000000000+g,-9000000000000000000+g FROM generate_series(1,20) AS g"
elif [[ "$TYPE" == int2 ]]; then
    q "$PRIMARY" "INSERT INTO feed VALUES(-32768,32767),(32767,-32768),(-1,NULL)"
elif [[ "$TYPE" == numeric ]]; then
    q "$PRIMARY" "INSERT INTO feed VALUES(-1,9999999999.99),(-2,-9999999999.99),(-3,-0.01)"
fi
caught_up
STANDBY_LAG="$(q "$STANDBY" "SELECT pending_events FROM syntra_capture_lag('feed')")"
((STANDBY_LAG > 0)) || fail "no pending events on the standby while maintenance was blocked"
[[ "$(q "$STANDBY" "$READ")" == "replay:true" ]] || fail "standby replay read: $(q "$STANDBY" "$READ")"
STATUS="$(q "$STANDBY" "SELECT concat_ws(',',source,pending_events,coalesce(error,'-')) FROM syntra_capture_status()")"
[[ "$STATUS" == "feed,$STANDBY_LAG,-" ]] || fail "standby status: $STATUS"
wait "$HOLDER"
# Maintenance and capture writes are refused on the standby; it is read-only.
if q "$STANDBY" "SELECT syntra_apply_prune_capture('feed')" 2>/dev/null; then fail "apply ran on the standby"; fi
if q "$STANDBY" "UPDATE feed SET v=0 WHERE id=1" 2>/dev/null; then fail "a write ran on the standby"; fi
wait_for "drain after the lock" "$PRIMARY" "$DRAINED" "t"
caught_up

# Under load: paced captured writes on the primary while the standby replays and
# answers checkpoint reads. Each read compares with the standby's heap in the
# same statement, so it must match even while replay moves underneath.
printf '\\set id random(1, 5000)\nUPDATE feed SET v = coalesce(v, 0) + %s WHERE id = :id;\n' "$STEP" >"$WORK/write.sql"
"$BIN/pgbench" -h "$SOCKET" -p "$PRIMARY" -U replica -n -c 4 -j 4 -T 60 -R 200 -f "$WORK/write.sql" feeds \
    >"$WORK/pgbench.log" 2>&1 &
BENCH=$!
sleep 1
READS=0
MISMATCHES=0
END=$((SECONDS + 15))
while ((SECONDS < END)); do
    ROUTE="$(q "$STANDBY" "$READ")"
    READS=$((READS + 1))
    [[ "$ROUTE" == *":true" ]] || { MISMATCHES=$((MISMATCHES + 1)); echo "$ROUTE" >>"$WORK/standby-mismatches.log"; }
    echo "$ROUTE" >>"$WORK/standby-routes.log"
done
((MISMATCHES == 0)) || fail "$MISMATCHES of $READS standby reads under load did not match its heap"
kill -0 "$BENCH" 2>/dev/null || fail "pgbench stopped before the failover; see $WORK/pgbench.log"

# Fail over with writes in flight: stop the primary without a checkpoint, promote
# the standby, keep capturing there. Writes the standby had not received are lost,
# as with any asynchronous standby; what it has must be consistent.
"$BIN/pg_ctl" -D "$WORK/primary" -w -t 30 stop -m immediate >/dev/null
wait "$BENCH" || true
"$BIN/pg_ctl" -D "$WORK/standby" -w -t 60 promote >/dev/null
wait_for "promotion" "$STANDBY" "SELECT pg_is_in_recovery()" "f"
wait_for "worker after promotion" "$STANDBY" "$WORKERS" "1"
[[ "$(q "$STANDBY" "$READ")" == "checkpoint:true" ]] || fail "promoted read: $(q "$STANDBY" "$READ")"
q "$STANDBY" "UPDATE feed SET v=v*2 WHERE id BETWEEN 1 AND 300; INSERT INTO feed VALUES(9001,1),(9002,NULL)"
wait_for "drain on the promoted node" "$STANDBY" "$DRAINED" "t"
[[ "$(q "$STANDBY" "$READ")" == "checkpoint:true" ]] || fail "promoted drained read: $(q "$STANDBY" "$READ")"
if grep -q "syntradb maintenance skipped" "$WORK/primary.log" "$WORK/standby.log"; then
    fail "the worker skipped the feed"
fi
ROUTES="$(sort "$WORK/standby-routes.log" | uniq -c | awk '{printf "%s%s %s", sep, $2, $1; sep=", "}')"
echo "replica check passed ($TYPE): standby read checkpoint and replayed history ($STANDBY_LAG pending), refused maintenance, ran no worker; $READS standby reads under 200 tps all matched ($ROUTES); after an immediate stop of the primary under load, the promoted standby's worker drained new writes; work dir $WORK"
