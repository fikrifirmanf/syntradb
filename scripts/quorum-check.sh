#!/usr/bin/env bash
# A primary with three standbys and synchronous_standby_names = 'ANY 2 (q1,q2,q3)'.
# Captured writers run against the primary throughout. Checks:
#  - with one standby stopped, writes keep being acknowledged;
#  - with two stopped, none is acknowledged, and they resume when one returns;
#  - with the WAL receiver of q3 stopped by SIGSTOP, so its link stalls without
#    closing, writes keep being acknowledged;
#  - after an immediate stop of the primary and of the standby that had received
#    the most WAL, the more advanced of the other two holds every acknowledged
#    write, and at most one more per writer, once promoted;
#  - the last standby follows the promoted node, and on both the checkpoint reads
#    match the heap and the promoted node's worker drains new writes.
# Uses a private copy of the production PostgreSQL install, on one machine.
# With "numeric" the feed's value column is numeric(12,2): writers add 0.01 and
# acknowledged writes are counted in hundredths, the unit it is stored in.
TYPE="${1:-int4}"
case "$TYPE" in
int4) VALUE=int4 SEED=g STEP=1 READER=int4 WIDTHS=4/4 SUM_SQL="SELECT sum(v) FROM feed" ;;
numeric) VALUE="numeric(12,2)" SEED="g*1.25" STEP=0.01 READER=numeric WIDTHS=4/8 SUM_SQL="SELECT (sum(v)*100)::int8 FROM feed" ;;
*) echo "quorum check: type must be int4 or numeric" >&2; exit 2 ;;
esac
set -euo pipefail
SYNTRA_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$SYNTRA_ROOT"
source scripts/lib/ack-counter.sh
bash scripts/setup-production.sh
export PGRX_HOME="$SYNTRA_ROOT/.pgrx"
export PATH="$SYNTRA_ROOT/.tools/bin:$PATH"
WORK="$SYNTRA_ROOT/target/quorum-check/$(date +%Y%m%d-%H%M%S)-$$"
mkdir -p "$WORK"
cp -R .tools/postgresql-production/18.6/install "$WORK/pg"
BIN="$WORK/pg/bin"
(cd crates/syntra-pg && CARGO_TARGET_DIR="$SYNTRA_ROOT/target/upgrade-current" cargo pgrx install \
    --release --cargo=--locked --pg-config "$WORK/pg/bin/pg_config")

SOCKET="$(mktemp -d /tmp/syntra-quorum.XXXXXX)"
PRIMARY=28951
PORTS=(28952 28953 28954)
NAMES=(q1 q2 q3)
WRITER_COUNT=4
# Standbys whose WAL receiver is stalled before the failover, counted from q3 down.
STALL=1
STALLED_PIDS=()
"$BIN/initdb" -D "$WORK/primary" --username=quorum --auth=trust --no-locale --encoding=UTF8 >/dev/null
printf "\nlisten_addresses = ''\nunix_socket_directories = '%s'\nport = %s\nwal_level = replica\nmax_wal_senders = 10\nwal_keep_size = 512MB\n" "$SOCKET" "$PRIMARY" \
    >>"$WORK/primary/postgresql.conf"
printf "shared_preload_libraries = 'syntradb'\nsyntradb.maintenance_database = 'feeds'\nsyntradb.maintenance_interval_ms = 200\n" \
    >>"$WORK/primary/postgresql.conf"
printf "local replication quorum trust\n" >>"$WORK/primary/pg_hba.conf"
RUNNING=()
WRITER_PIDS=()
start() { "$BIN/pg_ctl" -D "$1" -l "$1.log" -w -t 60 start >/dev/null; RUNNING+=("$1"); }
kill_node() { "$BIN/pg_ctl" -D "$1" -w -t 30 stop -m immediate >/dev/null; }
cleanup() {
    # Expansion guarded: an empty array is unbound under set -u in bash 3.2.
    for pid in ${WRITER_PIDS[@]+"${WRITER_PIDS[@]}"}; do kill "$pid" 2>/dev/null || true; done
    for pid in ${STALLED_PIDS[@]+"${STALLED_PIDS[@]}"}; do kill -CONT "$pid" 2>/dev/null || true; done
    for d in ${RUNNING[@]+"${RUNNING[@]}"}; do "$BIN/pg_ctl" -D "$d" -w -t 30 stop -m immediate >/dev/null 2>&1 || true; done
}
trap cleanup EXIT
q() { "$BIN/psql" -h "$SOCKET" -p "$1" -U quorum -d feeds -v ON_ERROR_STOP=1 -AtqX -c "$2"; }
fail() {
    echo "quorum check failed: $1" >&2
    exit 1
}
wait_for() {
    local tries=0
    until [[ "$(q "$2" "$3" 2>/dev/null || true)" == "$4" ]]; do
        tries=$((tries + 1))
        ((tries <= 200)) || fail "$1: got '$(q "$2" "$3" 2>&1 || true)', expected '$4' within 40 s"
        sleep 0.2
    done
}
READ="SELECT s.route||':'||(ROW(s.rows,s.non_nulls,s.sum,s.min,s.max) IS NOT DISTINCT FROM ROW(n.rows,n.non_nulls,n.sum,n.min,n.max))
    FROM syntra_checkpoint_${READER}_summary('feed','v',true) AS s,
    (SELECT count(*) AS rows,count(v) AS non_nulls,sum(v),min(v),max(v) FROM feed) AS n"
WORKERS="SELECT count(*) FROM pg_stat_activity WHERE backend_type='syntradb maintenance'"
DRAINED="SELECT applied IS NOT NULL AND pending_events=0 FROM syntra_capture_lag('feed')"
caught_up() {
    local lsn
    lsn="$(q "$1" "SELECT CASE WHEN pg_is_in_recovery() THEN pg_last_wal_replay_lsn() ELSE pg_current_wal_lsn() END")"
    [[ "$lsn" =~ ^[0-9A-F]+/[0-9A-F]+$ ]] || fail "unexpected WAL position '$lsn'"
    # unslop-ignore NO_SQL_INTERPOLATION -- lsn is checked as a WAL position just above.
    wait_for "node $2 replay" "$2" "SELECT pg_last_wal_replay_lsn() >= '$lsn'::pg_lsn" "t"
}
# Points standby $1 (data dir) at the node on port $2 under application name $3.
follow() {
    printf "primary_conninfo = 'host=%s port=%s user=quorum application_name=%s'\n" "$SOCKET" "$2" "$3" >>"$1/postgresql.auto.conf"
}
acked() {
    local total=0 w count
    for w in $(seq 1 "$WRITER_COUNT"); do
        count="$(cat "$WORK/acked-$w")"
        [[ "$count" =~ ^(0|[1-9][0-9]*)$ ]] || fail "invalid acknowledgement counter for writer $w"
        total=$((total + count))
    done
    echo "$total"
}

start "$WORK/primary"
"$BIN/psql" -h "$SOCKET" -p "$PRIMARY" -U quorum -d postgres -v ON_ERROR_STOP=1 -AtqX -c "CREATE DATABASE feeds" >/dev/null
q "$PRIMARY" "CREATE EXTENSION syntradb"
# unslop-ignore NO_SQL_INTERPOLATION -- VALUE and SEED are literals chosen at the top.
q "$PRIMARY" "CREATE TABLE feed(id int4 PRIMARY KEY,v $VALUE); INSERT INTO feed SELECT g,$SEED FROM generate_series(1,5000) AS g"
q "$PRIMARY" "SELECT syntra_seed_capture('feed','id','v',4096)" >/dev/null
# unslop-ignore NO_SQL_INTERPOLATION -- constant SQL; WIDTHS is only the expected value.
[[ "$(q "$PRIMARY" "SELECT key_width||'/'||value_width FROM syntra.capture_bindings")" == "$WIDTHS" ]] || fail "binding widths for $TYPE"
wait_for "first checkpoint" "$PRIMARY" "$DRAINED" "t"
for i in 0 1 2; do
    dir="$WORK/${NAMES[$i]}"
    "$BIN/pg_basebackup" -h "$SOCKET" -p "$PRIMARY" -U quorum -D "$dir" -X stream -R -c fast
    sed -i.bak "s/^port = .*$/port = ${PORTS[$i]}/" "$dir/postgresql.conf"
    follow "$dir" "$PRIMARY" "${NAMES[$i]}"
    chmod 700 "$dir"
    start "$dir"
    [[ "$(q "${PORTS[$i]}" "$WORKERS")" == "0" ]] || fail "a maintenance worker runs on ${NAMES[$i]}"
done
printf "\nsynchronous_standby_names = 'ANY 2 (q1,q2,q3)'\n" >>"$WORK/primary/postgresql.conf"
"$BIN/pg_ctl" -D "$WORK/primary" reload >/dev/null
wait_for "quorum standbys" "$PRIMARY" "SELECT string_agg(application_name||':'||sync_state,',' ORDER BY application_name) FROM pg_stat_replication" "q1:quorum,q2:quorum,q3:quorum"
START_SUM="$(q "$PRIMARY" "$SUM_SQL")"
[[ "$START_SUM" =~ ^[0-9]+$ ]] || fail "unexpected starting sum '$START_SUM'"

# A write counts as acknowledged only when psql returned success, which with
# synchronous commit means two of the three standbys have flushed it.
writer() {
    local ok=0 id sql
    while [[ ! -e "$WORK/stop-writers" ]]; do
        id=$((RANDOM % 5000 + 1))
        # unslop-ignore NO_SQL_INTERPOLATION -- id is integer arithmetic on $RANDOM, set just above; STEP a literal chosen at the top.
        sql="UPDATE feed SET v = v + $STEP WHERE id = $id"
        if "$BIN/psql" -h "$SOCKET" -p "$PRIMARY" -U quorum -d feeds -v ON_ERROR_STOP=1 -AtqX -c "$sql" >/dev/null 2>>"$WORK/writer-$1.log"; then
            ok=$((ok + 1))
            syntra_publish_ack "$WORK/acked-$1" "$ok" || return
        else
            if [[ ! -e "$WORK/primary-stopping" ]]; then
                echo "quorum writer $1 failed before the primary shutdown; see $WORK/writer-$1.log" >&2
                return 1
            fi
            break
        fi
    done
}
for w in $(seq 1 "$WRITER_COUNT"); do
    echo 0 >"$WORK/acked-$w"
    writer "$w" &
    WRITER_PIDS+=($!)
done
sleep 2
ALL_UP=$(acked)
((ALL_UP > 0)) || fail "no write was acknowledged with all standbys up"

# One standby down: two remain, which is a quorum.
kill_node "$WORK/q3"
BEFORE=$(acked)
sleep 3
ONE_DOWN=$(( $(acked) - BEFORE ))
((ONE_DOWN > 0)) || fail "no write was acknowledged with one standby down"

# Two down: one remains, below the quorum, so commits wait.
kill_node "$WORK/q2"
sleep 1
BEFORE=$(acked)
sleep 3
TWO_DOWN=$(( $(acked) - BEFORE ))
((TWO_DOWN == 0)) || fail "$TWO_DOWN writes were acknowledged with two of three standbys down"
"$BIN/pg_ctl" -D "$WORK/q2" -l "$WORK/q2.log" -w -t 60 start >/dev/null
"$BIN/pg_ctl" -D "$WORK/q3" -l "$WORK/q3.log" -w -t 60 start >/dev/null
wait_for "quorum restored" "$PRIMARY" "SELECT count(*) FROM pg_stat_replication WHERE sync_state='quorum' AND state='streaming'" "3"
BEFORE=$(acked)
sleep 3
RESUMED=$(( $(acked) - BEFORE ))
((RESUMED > 0)) || fail "writes did not resume once the quorum returned"

# Stall the last STALL standbys: their WAL receivers stop reading, as on a link
# that stops forwarding without closing. The primary still sees them connected.
for ((i = 3 - STALL; i < 3; i++)); do
    pid="$(q "${PORTS[$i]}" "SELECT pid FROM pg_stat_wal_receiver")"
    [[ "$pid" =~ ^[0-9]+$ ]] || fail "no WAL receiver on ${NAMES[$i]}"
    kill -STOP "$pid"
    STALLED_PIDS+=("$pid")
done
BEFORE=$(acked)
sleep 3
STALLED=$(( $(acked) - BEFORE ))
((STALLED > 0)) || fail "no write was acknowledged with $STALL standby stalled"

# Fail over with writes in flight, losing the primary and the most advanced standby.
touch "$WORK/primary-stopping"
kill_node "$WORK/primary"
touch "$WORK/stop-writers"
for pid in ${WRITER_PIDS[@]+"${WRITER_PIDS[@]}"}; do wait "$pid"; done
WRITER_PIDS=()
ACKED=$(acked)
# unslop-ignore NO_SQL_INTERPOLATION -- constant SQL; only the port argument varies.
LSN_SQL="SELECT pg_last_wal_receive_lsn()-'0/0'::pg_lsn"
best=-1
best_lsn=-1
for i in 0 1 2; do
    lsn="$(q "${PORTS[$i]}" "$LSN_SQL")"
    [[ "$lsn" =~ ^[0-9]+$ ]] || fail "unexpected received WAL position '$lsn' on ${NAMES[$i]}"
    if ((lsn > best_lsn)); then best=$i; best_lsn=$lsn; fi
done
LOST="${NAMES[$best]}"
kill_node "$WORK/$LOST"
# The stalled links recover, with nothing left to receive from: promotion needs
# a WAL receiver that can exit. WAL already in their socket buffers arrives.
for pid in ${STALLED_PIDS[@]+"${STALLED_PIDS[@]}"}; do kill -CONT "$pid" 2>/dev/null || true; done
STALLED_PIDS=()
sleep 1
SURVIVORS=()
for i in 0 1 2; do ((i == best)) || SURVIVORS+=("$i"); done
a="${SURVIVORS[0]}"
b="${SURVIVORS[1]}"
if (($(q "${PORTS[$b]}" "$LSN_SQL") > $(q "${PORTS[$a]}" "$LSN_SQL"))); then
    a="${SURVIVORS[1]}"
    b="${SURVIVORS[0]}"
fi
NEW="${PORTS[$a]}"
OTHER="${PORTS[$b]}"
# The stalled standbys stay behind; the promoted one must not be among them.
for ((i = 3 - STALL; i < 3; i++)); do
    ((i != a)) || fail "the most advanced survivor, ${NAMES[$a]}, was stalled"
done
"$BIN/pg_ctl" -D "$WORK/${NAMES[$a]}" -w -t 60 promote >/dev/null
wait_for "promotion" "$NEW" "SELECT pg_is_in_recovery()" "f"
DELTA=$(( $(q "$NEW" "$SUM_SQL") - START_SUM ))
((DELTA >= ACKED)) || fail "lost acknowledged writes: $ACKED acknowledged, $DELTA present on ${NAMES[$a]}"
((DELTA <= ACKED + WRITER_COUNT)) || fail "$DELTA writes on ${NAMES[$a]} for $ACKED acknowledged; more than one in flight per writer"

wait_for "worker after promotion" "$NEW" "$WORKERS" "1"
wait_for "promoted node drains" "$NEW" "$DRAINED" "t"
[[ "$(q "$NEW" "$READ")" == "checkpoint:true" ]] || fail "promoted read: $(q "$NEW" "$READ")"

# The last standby, stalled or not, follows the promoted node on its new timeline.
"$BIN/pg_ctl" -D "$WORK/${NAMES[$b]}" -w -t 30 stop -m fast >/dev/null
follow "$WORK/${NAMES[$b]}" "$NEW" "${NAMES[$b]}"
"$BIN/pg_ctl" -D "$WORK/${NAMES[$b]}" -l "$WORK/${NAMES[$b]}.log" -w -t 60 start >/dev/null
q "$NEW" "UPDATE feed SET v=v*2 WHERE id BETWEEN 1 AND 300; INSERT INTO feed VALUES(9001,1),(9002,NULL)"
wait_for "promoted node drains new writes" "$NEW" "$DRAINED" "t"
caught_up "$NEW" "$OTHER"
# unslop-ignore NO_SQL_INTERPOLATION -- constant SQL; only the port argument and message vary.
[[ "$(q "$OTHER" "SELECT pg_is_in_recovery()")" == "t" ]] || fail "${NAMES[$b]} left recovery"
# unslop-ignore NO_SQL_INTERPOLATION -- constant SQL; only the port argument varies.
[[ "$(q "$OTHER" "SELECT sum(v)||','||count(*) FROM feed")" == "$(q "$NEW" "SELECT sum(v)||','||count(*) FROM feed")" ]] \
    || fail "${NAMES[$b]}'s heap differs from the promoted node's"
[[ "$(q "$OTHER" "$READ")" == "checkpoint:true" ]] || fail "${NAMES[$b]} read: $(q "$OTHER" "$READ")"
[[ "$(q "$OTHER" "$WORKERS")" == "0" ]] || fail "a worker runs on ${NAMES[$b]}"

echo "quorum check passed ($TYPE): ANY 2 of 3 standbys; $ALL_UP writes acknowledged in 2 s with all up, $ONE_DOWN in 3 s with one down, $TWO_DOWN in 3 s with two down, $RESUMED in 3 s after two returned, $STALLED in 3 s with $STALL stalled; after an immediate stop of the primary and of the most advanced standby ($LOST), ${NAMES[$a]} held all $ACKED acknowledged writes ($DELTA present, at most $WRITER_COUNT unacknowledged); ${NAMES[$b]} followed it and both read exactly; work dir $WORK"
