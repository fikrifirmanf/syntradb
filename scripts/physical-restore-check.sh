#!/usr/bin/env bash
# Take a physical base backup while captured history is pending, archive WAL, and
# restore it twice: to a named restore point and to the end of the archive. Each
# restored cluster must agree with the state the primary had at that point, and
# capture, apply and prune must keep working on it. Uses a private copy of the
# production PostgreSQL install; the original is untouched.
#   physical-restore-check.sh [int2|int4|int8|numeric] [writers] [feeds]
# With int2, point A adds keys and values at the int2 limits; int2 is stored at 4
# bytes and read through the int4 summary.
# With int8, every value starts past the int4 range and point A adds keys past it.
# With numeric, keys are int4 and values numeric(12,2), stored as 8-byte integers
# of hundredths; writers add 0.01 and point A adds values at the column's limits.
# With more than one writer, that many sessions commit captured updates to
# overlapping rows during the backup, and one more session applies and prunes.
# With more than one feed, writers are spread over that many captured tables, and
# every check runs on each of them.
set -euo pipefail
SYNTRA_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$SYNTRA_ROOT"
TYPE="${1:-int4}"
# int2 is stored at 4 bytes: the binding records 4/4 and the int4 summary reads it.
KEY="$TYPE" VALUE="$TYPE" READER="${TYPE/int2/int4}" STEP=1
case "$TYPE" in
int2 | int4) SEED="g" ;;
int8) SEED="g+5000000000" ;;
numeric) KEY=int4 VALUE="numeric(12,2)" READER=numeric STEP=0.01 SEED="g*1.25-500.5" ;;
*) echo "physical restore check: type must be int2, int4, int8 or numeric" >&2; exit 2 ;;
esac
if [[ "$READER" == numeric ]]; then WIDTHS=4/8; else WIDTHS="${READER#int}/${READER#int}"; fi
WRITERS="${2:-1}"
[[ "$WRITERS" =~ ^[1-9][0-9]?$ ]] && ((WRITERS <= 64)) || { echo "physical restore check: writers must be 1 to 64" >&2; exit 2; }
FEED_COUNT="${3:-1}"
[[ "$FEED_COUNT" =~ ^[1-9][0-9]?$ ]] && ((FEED_COUNT <= 16)) || { echo "physical restore check: feeds must be 1 to 16" >&2; exit 2; }
((FEED_COUNT == 1 || WRITERS >= FEED_COUNT)) || { echo "physical restore check: more than one feed needs at least one writer per feed" >&2; exit 2; }
# The first feed keeps the name feed; the others are feed2, feed3 and so on.
FEEDS=(feed)
for ((f = 2; f <= FEED_COUNT; f++)); do FEEDS+=("feed$f"); done
bash scripts/setup-production.sh
export PGRX_HOME="$SYNTRA_ROOT/.pgrx"
export PATH="$SYNTRA_ROOT/.tools/bin:$PATH"
WORK="$SYNTRA_ROOT/target/physical-restore-check/$(date +%Y%m%d-%H%M%S)-$$"
mkdir -p "$WORK/archive"
cp -R .tools/postgresql-production/18.6/install "$WORK/pg"
BIN="$WORK/pg/bin"
(cd crates/syntra-pg && CARGO_TARGET_DIR="$SYNTRA_ROOT/target/upgrade-current" cargo pgrx install \
    --release --cargo=--locked --pg-config "$WORK/pg/bin/pg_config")

SOCKET="$(mktemp -d /tmp/syntra-restore.XXXXXX)"
"$BIN/initdb" -D "$WORK/primary" --username=restorer --auth=trust --no-locale --encoding=UTF8 >/dev/null
configure() {
    printf "\nlisten_addresses = ''\nunix_socket_directories = '%s'\nport = %s\n" "$SOCKET" "$2" >>"$1/postgresql.conf"
}
configure "$WORK/primary" 28932
printf "wal_level = replica\narchive_mode = on\narchive_command = 'cp %%p %s/archive/%%f'\n" "$WORK" \
    >>"$WORK/primary/postgresql.conf"
RUNNING=()
start() { "$BIN/pg_ctl" -D "$1" -l "$1.log" -w -t 60 start >/dev/null; RUNNING+=("$1"); }
stop() { "$BIN/pg_ctl" -D "$1" -w -t 30 stop -m fast >/dev/null; }
cleanup() { for d in "${RUNNING[@]}"; do stop "$d" 2>/dev/null || true; done; }
trap cleanup EXIT
q() { "$BIN/psql" -h "$SOCKET" -p "$1" -U restorer -d restored -v ON_ERROR_STOP=1 -AtqX -c "$2"; }
expect() {
    if [[ "$2" != "$3" ]]; then
        echo "physical restore check failed: $1: got '$2', expected '$3'" >&2
        exit 1
    fi
}
# Each query takes a feed name, which comes from FEEDS above.
# unslop-ignore NO_SQL_INTERPOLATION -- READER is one of three literals, chosen above, and $1 a fixed feed name.
read_sql() {
    echo "SELECT s.route||':'||(ROW(s.rows,s.non_nulls,s.sum,s.min,s.max) IS NOT DISTINCT FROM ROW(n.rows,n.non_nulls,n.sum,n.min,n.max))
    FROM syntra_checkpoint_${READER}_summary('$1','v',true) AS s,
    (SELECT count(*) AS rows,count(v) AS non_nulls,sum(v),min(v),max(v) FROM $1) AS n"
}
# unslop-ignore NO_SQL_INTERPOLATION -- $1 is a fixed feed name.
heap_sql() { echo "SELECT concat_ws(',',count(*),count(v),sum(v),min(v),max(v)) FROM $1"; }
# unslop-ignore NO_SQL_INTERPOLATION -- $1 is a fixed feed name.
lag_sql() { echo "SELECT concat_ws(',',emitted,applied,pending_events,generation) FROM syntra_capture_lag('$1')"; }
# Runs SQL from $2 (a function of the feed name) on every feed of port $1.
each() { for f in "${FEEDS[@]}"; do q "$1" "$("$2" "$f")"; done; }
# Heap aggregates and lag of every feed, in feed order, as one line.
state() { for f in "${FEEDS[@]}"; do echo "$f=$(q "$1" "$(heap_sql "$f")")/$(q "$1" "$(lag_sql "$f")")"; done | paste -sd ' ' -; }
# unslop-ignore NO_SQL_INTERPOLATION -- f is a fixed feed name from FEEDS.
emitted() { for f in "${FEEDS[@]}"; do q "$1" "SELECT emitted FROM syntra_capture_lag('$f')"; done | paste -sd + - | bc; }
# Every feed's checkpoint read must give the route in $2 and match the heap.
expect_reads() {
    local f
    for f in "${FEEDS[@]}"; do expect "$3 $f read" "$(q "$1" "$(read_sql "$f")")" "$2:true"; done
}

start "$WORK/primary"
"$BIN/psql" -h "$SOCKET" -p 28932 -U restorer -d postgres -v ON_ERROR_STOP=1 -AtqX -c "CREATE DATABASE restored" >/dev/null
q 28932 "CREATE EXTENSION syntradb"
for f in "${FEEDS[@]}"; do
    # unslop-ignore NO_SQL_INTERPOLATION -- KEY, VALUE and SEED are literals chosen above, f a fixed feed name.
    q 28932 "CREATE TABLE $f(id $KEY PRIMARY KEY,v $VALUE); INSERT INTO $f SELECT g,$SEED FROM generate_series(1,1000) AS g"
    # unslop-ignore NO_SQL_INTERPOLATION -- f is a fixed feed name from FEEDS.
    q 28932 "SELECT syntra_seed_capture('$f','id','v',4096)" >/dev/null
done
# unslop-ignore NO_SQL_INTERPOLATION -- constant SQL; WIDTHS is only the expected value.
expect "binding widths" "$(q 28932 "SELECT string_agg(DISTINCT key_width||'/'||value_width,' ')||' '||count(*) FROM syntra.capture_bindings")" "$WIDTHS $FEED_COUNT"
for f in "${FEEDS[@]}"; do
    # unslop-ignore NO_SQL_INTERPOLATION -- f is a fixed feed name from FEEDS.
    q 28932 "SELECT syntra_apply_capture('$f')" >/dev/null
    # unslop-ignore NO_SQL_INTERPOLATION -- f is a fixed feed name from FEEDS.
    q 28932 "UPDATE $f SET v=v+1 WHERE id<=100"
    # unslop-ignore NO_SQL_INTERPOLATION -- f is a fixed feed name from FEEDS.
    q 28932 "SELECT applied FROM syntra_apply_prune_capture('$f')" >/dev/null
    # History captured before the backup and never applied.
    # unslop-ignore NO_SQL_INTERPOLATION -- f is a fixed feed name from FEEDS.
    q 28932 "UPDATE $f SET v=-v WHERE id BETWEEN 101 AND 150"
done
# Captured writes, with apply and prune every 100, keep committing while the base
# backup is taken. The feed position must move during the backup.
if ((WRITERS == 1)); then
    for i in $(seq 1 3000); do
        # unslop-ignore NO_SQL_INTERPOLATION -- STEP is one of two literals chosen above; the id is computed.
        echo "UPDATE feed SET v=coalesce(v,0)+$STEP WHERE id=$((i % 900 + 1));"
        if ((i % 100 == 0)); then echo "SELECT applied FROM syntra_apply_prune_capture('feed');"; fi
    done >"$WORK/writes-1.sql"
else
    # Writers overlap on rows and apply nothing; the maintainer below drains.
    # Writer w writes to feed number (w-1) mod FEED_COUNT.
    for w in $(seq 1 "$WRITERS"); do
        f="${FEEDS[$(((w - 1) % FEED_COUNT))]}"
        for i in $(seq 1 3000); do
            # unslop-ignore NO_SQL_INTERPOLATION -- f is a fixed feed name from FEEDS.
            echo "UPDATE $f SET v=coalesce(v,0)+$STEP WHERE id=$(((i * 7 + w) % 900 + 1));"
        done >"$WORK/writes-$w.sql"
    done
fi
STARTED_AT="$(emitted 28932)"
WRITER_PIDS=()
for w in $(seq 1 "$WRITERS"); do
    "$BIN/psql" -h "$SOCKET" -p 28932 -U restorer -d restored -v ON_ERROR_STOP=1 -AtqX -f "$WORK/writes-$w.sql" >/dev/null &
    WRITER_PIDS+=($!)
done
if ((WRITERS > 1)); then
    (
        any_running() { for pid in "${WRITER_PIDS[@]}"; do kill -0 "$pid" 2>/dev/null && return 0; done; return 1; }
        while any_running; do
            for f in "${FEEDS[@]}"; do
                # unslop-ignore NO_SQL_INTERPOLATION -- f is a fixed feed name from FEEDS.
                q 28932 "SELECT applied FROM syntra_apply_prune_capture('$f')" >/dev/null 2>&1 || true
            done
            sleep 0.02
        done
    ) &
    MAINTAINER=$!
fi
until (($(emitted 28932) > STARTED_AT + 50)); do sleep 0.05; done
BEFORE_BACKUP="$(emitted 28932)"
"$BIN/pg_basebackup" -h "$SOCKET" -p 28932 -U restorer -D "$WORK/backup" -X stream -c fast
AFTER_BACKUP="$(emitted 28932)"
for pid in "${WRITER_PIDS[@]}"; do wait "$pid"; done
if ((WRITERS > 1)); then wait "$MAINTAINER"; fi
if ((AFTER_BACKUP <= BEFORE_BACKUP)); then
    echo "physical restore check failed: no captured write committed during the base backup" >&2
    exit 1
fi

# Point A: written after the backup, partly applied, with history still pending.
for f in "${FEEDS[@]}"; do
    # unslop-ignore NO_SQL_INTERPOLATION -- f is a fixed feed name from FEEDS.
    q 28932 "DELETE FROM $f WHERE id BETWEEN 990 AND 1000"
    # unslop-ignore NO_SQL_INTERPOLATION -- f is a fixed feed name from FEEDS.
    q 28932 "INSERT INTO $f SELECT g,NULL FROM generate_series(2001,2020) AS g"
    if [[ "$TYPE" == int2 ]]; then
        # unslop-ignore NO_SQL_INTERPOLATION -- f is a fixed feed name from FEEDS.
        q 28932 "INSERT INTO $f VALUES(-32768,32767),(32767,-32768),(-1,NULL)"
    fi
    if [[ "$TYPE" == int8 ]]; then
        # unslop-ignore NO_SQL_INTERPOLATION -- f is a fixed feed name from FEEDS.
        q 28932 "INSERT INTO $f SELECT 6000000000+g,-9000000000000000000+g FROM generate_series(1,20) AS g"
    fi
    if [[ "$TYPE" == numeric ]]; then
        # Keys 3001 and 3002 are not multiples of 3, so point B's +1000 skips them.
        # unslop-ignore NO_SQL_INTERPOLATION -- f is a fixed feed name from FEEDS.
        q 28932 "INSERT INTO $f VALUES(3001,9999999999.99),(3002,-9999999999.99),(3004,-0.01)"
    fi
    # unslop-ignore NO_SQL_INTERPOLATION -- f is a fixed feed name from FEEDS.
    q 28932 "SELECT applied FROM syntra_apply_prune_capture('$f')" >/dev/null
    # unslop-ignore NO_SQL_INTERPOLATION -- f is a fixed feed name from FEEDS.
    q 28932 "UPDATE $f SET v=7 WHERE id BETWEEN 200 AND 230"
done
expect_reads 28932 replay "primary at A"
STATE_A="$(state 28932)"
q 28932 "SELECT pg_create_restore_point('syntra_a')" >/dev/null
# Point B: everything after A, applied and pruned, then more pending history.
for f in "${FEEDS[@]}"; do
    # unslop-ignore NO_SQL_INTERPOLATION -- f is a fixed feed name from FEEDS.
    q 28932 "UPDATE $f SET v=coalesce(v,0)+1000 WHERE id%3=0"
    # unslop-ignore NO_SQL_INTERPOLATION -- f is a fixed feed name from FEEDS.
    q 28932 "SELECT applied FROM syntra_apply_prune_capture('$f')" >/dev/null
    # unslop-ignore NO_SQL_INTERPOLATION -- f is a fixed feed name from FEEDS.
    q 28932 "UPDATE $f SET id=id+5000 WHERE id BETWEEN 300 AND 310"
done
expect_reads 28932 replay "primary at B"
STATE_B="$(state 28932)"
# Every feed must differ between A and B, in its heap and in its lag.
for f in "${FEEDS[@]}"; do
    a="$(tr ' ' '\n' <<<"$STATE_A" | grep "^$f=")"
    b="$(tr ' ' '\n' <<<"$STATE_B" | grep "^$f=")"
    if [[ "${a%%/*}" == "${b%%/*}" || "${a#*/}" == "${b#*/}" ]]; then
        echo "physical restore check failed: points A and B are indistinguishable for $f" >&2
        exit 1
    fi
done
q 28932 "SELECT pg_switch_wal()" >/dev/null
q 28932 "CHECKPOINT"
stop "$WORK/primary"

# Restore the same base backup into $1 on port $2, replaying archived WAL up to
# the recovery target given in $3 (empty means the end of the archive).
restore() {
    cp -R "$WORK/backup" "$1"
    chmod 700 "$1"
    configure "$1" "$2"
    printf "archive_mode = off\nrestore_command = 'cp %s/archive/%%f %%p'\nrecovery_target_action = 'promote'\n%s\n" \
        "$WORK" "$3" >>"$1/postgresql.conf"
    touch "$1/recovery.signal"
    start "$1"
    local tries=0
    until [[ "$(q "$2" "SELECT pg_is_in_recovery()")" == "f" ]]; do
        tries=$((tries + 1))
        if ((tries > 300)); then
            echo "physical restore check failed: $1 did not finish recovery in 60 s" >&2
            exit 1
        fi
        sleep 0.2
    done
}
check_restored() {
    expect "$1 heap and lag" "$(state "$2")" "$3"
    expect_reads "$2" replay "$1"
    # Each restored feed must keep capturing, applying and pruning.
    local f
    for f in "${FEEDS[@]}"; do
        # unslop-ignore NO_SQL_INTERPOLATION -- f is a fixed feed name from FEEDS.
        q "$2" "UPDATE $f SET v=1 WHERE id=1"
        # unslop-ignore NO_SQL_INTERPOLATION -- f is a fixed feed name from FEEDS.
        q "$2" "SELECT applied FROM syntra_apply_prune_capture('$f')" >/dev/null
        # unslop-ignore NO_SQL_INTERPOLATION -- f is a fixed feed name from FEEDS.
        expect "$1 $f drained lag" "$(q "$2" "SELECT pending_events FROM syntra_capture_lag('$f')")" "0"
        # unslop-ignore NO_SQL_INTERPOLATION -- f is a fixed feed name from FEEDS.
        expect "$1 $f headroom" "$(q "$2" "SELECT retained_events FROM syntra_capture_headroom('$f')")" "0"
    done
    expect_reads "$2" checkpoint "$1 drained"
}
restore "$WORK/at-a" 28933 "recovery_target_name = 'syntra_a'"
check_restored "restore point A" 28933 "$STATE_A"
stop "$WORK/at-a"
restore "$WORK/at-end" 28934 ""
check_restored "end of archive" 28934 "$STATE_B"
echo "physical restore check passed ($TYPE, $WRITERS writers, $FEED_COUNT feeds): base backup taken during captured writes ($((AFTER_BACKUP - BEFORE_BACKUP)) events), with pending history, restored to a named point and to the end of archived WAL; work dir $WORK"
