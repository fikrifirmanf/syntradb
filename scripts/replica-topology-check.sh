#!/usr/bin/env bash
# A primary with a synchronous standby (s1) and a standby cascading from it (s2).
# Captured writers run against the primary; it is stopped in immediate mode and
# s1 is promoted. Checks:
#  - every write a client saw acknowledged is on the promoted node (synchronous
#    commit loses none), and it holds at most one extra write per writer;
#  - the cascading standby follows the new timeline and its reads match its heap;
#  - the promoted node's worker drains new writes;
#  - the old primary, rewound with pg_rewind, rejoins as a standby of the new one,
#    with reads that match its heap and the same rows as the new primary.
# Uses a private copy of the production PostgreSQL install.
# With "numeric" the feed's value column is numeric(12,2): writers add 0.01 and
# acknowledged writes are counted in hundredths, the unit it is stored in.
TYPE="${1:-int4}"
case "$TYPE" in
int4) VALUE=int4 SEED=g STEP=1 READER=int4 WIDTHS=4/4 SUM_SQL="SELECT sum(v) FROM feed" ;;
numeric) VALUE="numeric(12,2)" SEED="g*1.25" STEP=0.01 READER=numeric WIDTHS=4/8 SUM_SQL="SELECT (sum(v)*100)::int8 FROM feed" ;;
*) echo "replica topology check: type must be int4 or numeric" >&2; exit 2 ;;
esac
set -euo pipefail
SYNTRA_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$SYNTRA_ROOT"
bash scripts/setup-production.sh
export PGRX_HOME="$SYNTRA_ROOT/.pgrx"
export PATH="$SYNTRA_ROOT/.tools/bin:$PATH"
WORK="$SYNTRA_ROOT/target/replica-topology-check/$(date +%Y%m%d-%H%M%S)-$$"
mkdir -p "$WORK"
cp -R .tools/postgresql-production/18.6/install "$WORK/pg"
BIN="$WORK/pg/bin"
(cd crates/syntra-pg && CARGO_TARGET_DIR="$SYNTRA_ROOT/target/upgrade-current" cargo pgrx install \
    --release --cargo=--locked --pg-config "$WORK/pg/bin/pg_config")

SOCKET="$(mktemp -d /tmp/syntra-topo.XXXXXX)"
PRIMARY=28941
S1=28942
S2=28943
WRITER_COUNT=4
# Checksums let pg_rewind work on the old primary later. It also needs the WAL from
# before the divergence, which the old primary keeps with wal_keep_size.
"$BIN/initdb" -D "$WORK/primary" --username=topo --auth=trust --no-locale --encoding=UTF8 --data-checksums >/dev/null
printf "\nlisten_addresses = ''\nunix_socket_directories = '%s'\nport = %s\nwal_level = replica\nmax_wal_senders = 10\nwal_keep_size = 512MB\n" "$SOCKET" "$PRIMARY" \
    >>"$WORK/primary/postgresql.conf"
printf "shared_preload_libraries = 'syntradb'\nsyntradb.maintenance_database = 'feeds'\nsyntradb.maintenance_interval_ms = 200\n" \
    >>"$WORK/primary/postgresql.conf"
printf "local replication topo trust\n" >>"$WORK/primary/pg_hba.conf"
RUNNING=()
WRITER_PIDS=()
start() { "$BIN/pg_ctl" -D "$1" -l "$1.log" -w -t 60 start >/dev/null; RUNNING+=("$1"); }
cleanup() {
    # Expansion guarded: an empty array is unbound under set -u in bash 3.2.
    for pid in ${WRITER_PIDS[@]+"${WRITER_PIDS[@]}"}; do kill "$pid" 2>/dev/null || true; done
    for d in ${RUNNING[@]+"${RUNNING[@]}"}; do "$BIN/pg_ctl" -D "$d" -w -t 30 stop -m immediate >/dev/null 2>&1 || true; done
}
trap cleanup EXIT
q() { "$BIN/psql" -h "$SOCKET" -p "$1" -U topo -d feeds -v ON_ERROR_STOP=1 -AtqX -c "$2"; }
fail() {
    echo "replica topology check failed: $1" >&2
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
# The node on port $2 has replayed everything the node on port $1 has written.
caught_up() {
    local lsn
    lsn="$(q "$1" "SELECT CASE WHEN pg_is_in_recovery() THEN pg_last_wal_replay_lsn() ELSE pg_current_wal_lsn() END")"
    [[ "$lsn" =~ ^[0-9A-F]+/[0-9A-F]+$ ]] || fail "unexpected WAL position '$lsn'"
    # unslop-ignore NO_SQL_INTERPOLATION -- lsn is checked as a WAL position just above.
    wait_for "node $2 replay" "$2" "SELECT pg_last_wal_replay_lsn() >= '$lsn'::pg_lsn" "t"
}
standby() {
    # $1 data dir, $2 port, $3 source port, $4 application name, $5 source of the base backup
    "$BIN/pg_basebackup" -h "$SOCKET" -p "$5" -U topo -D "$1" -X stream -R -c fast
    sed -i.bak "s/^port = .*$/port = $2/" "$1/postgresql.conf"
    printf "primary_conninfo = 'host=%s port=%s user=topo application_name=%s'\n" "$SOCKET" "$3" "$4" >>"$1/postgresql.auto.conf"
    chmod 700 "$1"
    start "$1"
}

start "$WORK/primary"
"$BIN/psql" -h "$SOCKET" -p "$PRIMARY" -U topo -d postgres -v ON_ERROR_STOP=1 -AtqX -c "CREATE DATABASE feeds" >/dev/null
q "$PRIMARY" "CREATE EXTENSION syntradb"
# unslop-ignore NO_SQL_INTERPOLATION -- VALUE and SEED are literals chosen at the top.
q "$PRIMARY" "CREATE TABLE feed(id int4 PRIMARY KEY,v $VALUE); INSERT INTO feed SELECT g,$SEED FROM generate_series(1,5000) AS g"
q "$PRIMARY" "SELECT syntra_seed_capture('feed','id','v',4096)" >/dev/null
# unslop-ignore NO_SQL_INTERPOLATION -- constant SQL; WIDTHS is only the expected value.
[[ "$(q "$PRIMARY" "SELECT key_width||'/'||value_width FROM syntra.capture_bindings")" == "$WIDTHS" ]] || fail "binding widths for $TYPE"
wait_for "first checkpoint" "$PRIMARY" "$DRAINED" "t"

standby "$WORK/s1" "$S1" "$PRIMARY" s1 "$PRIMARY"
# s2 is built from s1, so it cascades: its primary_conninfo points at s1.
standby "$WORK/s2" "$S2" "$S1" s2 "$S1"
for node in "$S1" "$S2"; do
    # unslop-ignore NO_SQL_INTERPOLATION -- constant SQL; only the port argument varies.
    [[ "$(q "$node" "SELECT pg_is_in_recovery()")" == "t" ]] || fail "node $node is not in recovery"
    [[ "$(q "$node" "$WORKERS")" == "0" ]] || fail "a maintenance worker runs on standby $node"
done
printf "\nsynchronous_standby_names = 's1'\n" >>"$WORK/primary/postgresql.conf"
"$BIN/pg_ctl" -D "$WORK/primary" reload >/dev/null
wait_for "s1 synchronous" "$PRIMARY" "SELECT sync_state FROM pg_stat_replication WHERE application_name='s1'" "sync"
wait_for "s2 streaming from s1" "$S1" "SELECT count(*) FROM pg_stat_replication WHERE application_name='s2' AND state='streaming'" "1"
caught_up "$PRIMARY" "$S2"
START_SUM="$(q "$PRIMARY" "$SUM_SQL")"
[[ "$START_SUM" =~ ^[0-9]+$ ]] || fail "unexpected starting sum '$START_SUM'"

# Four writers on the primary. A write counts as acknowledged only when psql
# returned success, which with synchronous commit means s1 has flushed it.
writer() {
    local ok=0 id sql
    while [[ ! -e "$WORK/stop-writers" ]]; do
        id=$((RANDOM % 5000 + 1))
        # unslop-ignore NO_SQL_INTERPOLATION -- id is integer arithmetic on $RANDOM, set just above; STEP a literal chosen at the top.
        sql="UPDATE feed SET v = v + $STEP WHERE id = $id"
        if "$BIN/psql" -h "$SOCKET" -p "$PRIMARY" -U topo -d feeds -v ON_ERROR_STOP=1 -AtqX -c "$sql" >/dev/null 2>&1; then
            ok=$((ok + 1))
            echo "$ok" >"$WORK/acked-$1"
        else
            break
        fi
    done
}
for w in $(seq 1 "$WRITER_COUNT"); do
    echo 0 >"$WORK/acked-$w"
    writer "$w" &
    WRITER_PIDS+=($!)
done
# Reads on the cascading standby while the primary is loaded.
sleep 2
READS=0
MISMATCHES=0
END=$((SECONDS + 8))
while ((SECONDS < END)); do
    ROUTE="$(q "$S2" "$READ")"
    READS=$((READS + 1))
    [[ "$ROUTE" == *":true" ]] || { MISMATCHES=$((MISMATCHES + 1)); echo "$ROUTE" >>"$WORK/s2-mismatches.log"; }
done
((MISMATCHES == 0)) || fail "$MISMATCHES of $READS cascading standby reads did not match its heap"

# Fail over with writes in flight.
"$BIN/pg_ctl" -D "$WORK/primary" -w -t 30 stop -m immediate >/dev/null
touch "$WORK/stop-writers"
for pid in ${WRITER_PIDS[@]+"${WRITER_PIDS[@]}"}; do wait "$pid" || true; done
WRITER_PIDS=()
ACKED=0
for w in $(seq 1 "$WRITER_COUNT"); do ACKED=$((ACKED + $(cat "$WORK/acked-$w"))); done
((ACKED > 0)) || fail "no write was acknowledged before the failover"
"$BIN/pg_ctl" -D "$WORK/s1" -w -t 60 promote >/dev/null
wait_for "promotion" "$S1" "SELECT pg_is_in_recovery()" "f"
DELTA=$(( $(q "$S1" "$SUM_SQL") - START_SUM ))
((DELTA >= ACKED)) || fail "lost acknowledged writes: $ACKED acknowledged, $DELTA present on the promoted node"
((DELTA <= ACKED + WRITER_COUNT)) || fail "$DELTA writes on the promoted node for $ACKED acknowledged; more than one in flight per writer"

wait_for "worker after promotion" "$S1" "$WORKERS" "1"
wait_for "promoted node drains" "$S1" "$DRAINED" "t"
[[ "$(q "$S1" "$READ")" == "checkpoint:true" ]] || fail "promoted read: $(q "$S1" "$READ")"
q "$S1" "UPDATE feed SET v=v*2 WHERE id<=300; INSERT INTO feed VALUES(9001,1),(9002,NULL)"
wait_for "promoted node drains new writes" "$S1" "$DRAINED" "t"
# The cascading standby follows the new timeline.
caught_up "$S1" "$S2"
[[ "$(q "$S2" "SELECT pg_is_in_recovery()")" == "t" ]] || fail "the cascading standby left recovery"
# unslop-ignore NO_SQL_INTERPOLATION -- constant SQL; only the port argument varies.
[[ "$(q "$S2" "SELECT sum(v)||','||count(*) FROM feed")" == "$(q "$S1" "SELECT sum(v)||','||count(*) FROM feed")" ]] \
    || fail "the cascading standby's heap differs from the promoted node's"
[[ "$(q "$S2" "$READ")" == *":true" ]] || fail "cascading standby read after the failover: $(q "$S2" "$READ")"
[[ "$(q "$S2" "$WORKERS")" == "0" ]] || fail "a worker runs on the cascading standby"

# Failback: the old primary may hold writes s1 never got. Let it finish crash
# recovery with replication and the worker off, stop it cleanly, rewind it to the
# new primary and start it as a standby of that node.
printf "\nsynchronous_standby_names = ''\nsyntradb.maintenance_database = ''\n" >>"$WORK/primary/postgresql.conf"
"$BIN/pg_ctl" -D "$WORK/primary" -l "$WORK/primary.log" -w -t 60 start >/dev/null
"$BIN/pg_ctl" -D "$WORK/primary" -w -t 30 stop -m fast >/dev/null
"$BIN/pg_rewind" --target-pgdata="$WORK/primary" --source-server="host=$SOCKET port=$S1 user=topo dbname=postgres" \
    --write-recovery-conf >"$WORK/rewind.log" 2>&1 || fail "pg_rewind failed; see $WORK/rewind.log"
# pg_rewind copies the source's configuration files, including its port and the
# worker setting, so only the port needs putting back.
sed -i.bak "s/^port = .*$/port = $PRIMARY/" "$WORK/primary/postgresql.conf"
start "$WORK/primary"
wait_for "old primary is a standby" "$PRIMARY" "SELECT pg_is_in_recovery()" "t"
caught_up "$S1" "$PRIMARY"
[[ "$(q "$PRIMARY" "$WORKERS")" == "0" ]] || fail "a worker runs on the failed-back node"
# unslop-ignore NO_SQL_INTERPOLATION -- constant SQL; only the port argument varies.
[[ "$(q "$PRIMARY" "SELECT sum(v)||','||count(*) FROM feed")" == "$(q "$S1" "SELECT sum(v)||','||count(*) FROM feed")" ]] \
    || fail "the failed-back node's heap differs from the new primary's"
[[ "$(q "$PRIMARY" "$READ")" == *":true" ]] || fail "failed-back read: $(q "$PRIMARY" "$READ")"
# And it keeps following: a further write on the new primary reaches it.
q "$S1" "UPDATE feed SET v=v+1 WHERE id=1"
caught_up "$S1" "$PRIMARY"
# unslop-ignore NO_SQL_INTERPOLATION -- constant SQL; only the port argument varies.
[[ "$(q "$PRIMARY" "SELECT v FROM feed WHERE id=1")" == "$(q "$S1" "SELECT v FROM feed WHERE id=1")" ]] || fail "the failed-back node stopped following"

echo "replica topology check passed ($TYPE): with a synchronous standby and a cascading standby, $ACKED acknowledged writes all survived an immediate stop of the primary ($DELTA present, at most $WRITER_COUNT unacknowledged); $READS cascading reads under load matched; the cascading standby followed the new timeline; the promoted node's worker drained new writes; the old primary was rewound and followed the new primary; work dir $WORK"
