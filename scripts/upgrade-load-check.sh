#!/usr/bin/env bash
# Upgrade a database while captured writers are running. Writers start right after
# the new library is installed. From 0.1.0 or 0.2.0 they fail closed until ALTER
# EXTENSION UPDATE commits, then succeed; from 0.3.0 through 0.9.0 every write
# must succeed: the new library captures with the old schema's row trigger, and
# the update only makes writers wait while it replaces that trigger. From 0.6.0
# the apply loop fails until the update creates the delta table. From 0.7.0,
# grouping is additive and capture maintenance works before the update. From
# 0.8.0, the update changes only the version marker. Meanwhile a
# loop applies and prunes as the maintenance worker would. Every write a writer sees succeed adds
# one to the feed's sum, so the heap sum must equal the starting sum plus that
# count: no committed write may be lost, and none may commit twice. After the
# update the checkpoint read must match the heap, and the feed must drain.
#   upgrade-load-check.sh [from-version] [seconds] [tps]
# Uses a private copy of the production PostgreSQL install.
set -euo pipefail
SYNTRA_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$SYNTRA_ROOT"
FROM="${1:-0.2.0}"
SECONDS_RUN="${2:-30}"
TPS="${3:-100}"
[[ "$FROM" =~ ^0\.[1-9]\.0$ ]] || { echo "upgrade load check: from-version must be 0.1.0 through 0.9.0" >&2; exit 2; }
for value in "$SECONDS_RUN" "$TPS"; do
    [[ "$value" =~ ^[1-9][0-9]{0,3}$ ]] || { echo "upgrade load check: '$value' is not a positive integer" >&2; exit 2; }
done
((SECONDS_RUN >= 15)) || { echo "upgrade load check: run at least 15 seconds" >&2; exit 2; }
bash scripts/setup-production.sh
export PGRX_HOME="$SYNTRA_ROOT/.pgrx"
export PATH="$SYNTRA_ROOT/.tools/bin:$PATH"
CURRENT="$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)"
[[ "$CURRENT" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "upgrade load check: workspace version '$CURRENT' is not x.y.z" >&2; exit 1; }
WORK="$SYNTRA_ROOT/target/upgrade-load-check/$(date +%Y%m%d-%H%M%S)-$$"
mkdir -p "$WORK/base"
cp -R .tools/postgresql-production/18.6/install "$WORK/pg"
PG_CONFIG="$WORK/pg/bin/pg_config"
BIN="$WORK/pg/bin"
tar xzf "build-support/upgrade-baselines/syntradb-$FROM-src.tgz" -C "$WORK/base"
install_from() {
    (cd "$1/crates/syntra-pg" && CARGO_TARGET_DIR="$2" cargo pgrx install --release \
        --cargo=--locked --pg-config "$PG_CONFIG")
}
install_from "$WORK/base" "$SYNTRA_ROOT/target/upgrade-baseline-$FROM"

SOCKET="$(mktemp -d /tmp/syntra-upload.XXXXXX)"
PORT=28940
"$BIN/initdb" -D "$WORK/data" --username=upgrader --auth=trust --no-locale --encoding=UTF8 >/dev/null
printf "\nlisten_addresses = ''\nunix_socket_directories = '%s'\nport = %s\n" "$SOCKET" "$PORT" >>"$WORK/data/postgresql.conf"
start() { "$BIN/pg_ctl" -D "$WORK/data" -l "$WORK/postgres.log" -w -t 30 start >/dev/null; }
stop() { "$BIN/pg_ctl" -D "$WORK/data" -w -t 30 stop -m fast >/dev/null; }
BENCH=""
cleanup() {
    # shellcheck disable=SC2086 -- BENCH holds writer process IDs separated by spaces.
    [[ -z "$BENCH" ]] || kill $BENCH 2>/dev/null || true
    stop 2>/dev/null || true
}
trap cleanup EXIT
q() { "$BIN/psql" -h "$SOCKET" -p "$PORT" -U upgrader -d upgraded -v ON_ERROR_STOP=1 -AtqX -c "$1"; }
fail() {
    echo "upgrade load check failed: $1" >&2
    exit 1
}
expect() { [[ "$2" == "$3" ]] || fail "$1: got '$2', expected '$3'"; }
READ="SELECT s.route||':'||(ROW(s.rows,s.non_nulls,s.sum,s.min,s.max) IS NOT DISTINCT FROM ROW(n.rows,n.non_nulls,n.sum,n.min,n.max))
    FROM syntra_checkpoint_int4_summary('feed','v',true) AS s,
    (SELECT count(*) AS rows,count(v) AS non_nulls,sum(v),min(v),max(v) FROM feed) AS n"

start
"$BIN/psql" -h "$SOCKET" -p "$PORT" -U upgrader -d postgres -v ON_ERROR_STOP=1 -AtqX -c "CREATE DATABASE upgraded" >/dev/null
# unslop-ignore NO_SQL_INTERPOLATION -- FROM is a literal x.y.0 version, checked above.
q "CREATE EXTENSION syntradb VERSION '$FROM'"
q "CREATE TABLE feed(id int4 PRIMARY KEY,v int4); INSERT INTO feed SELECT g,g FROM generate_series(1,5000) AS g"
q "SELECT syntra_seed_capture('feed','id','v',4096)" >/dev/null
q "SELECT syntra_apply_capture('feed')" >/dev/null
q "UPDATE feed SET v=v+1 WHERE id<=300"
START_SUM="$(q "SELECT sum(v) FROM feed")"
[[ "$START_SUM" =~ ^[0-9]+$ ]] || fail "unexpected starting sum '$START_SUM'"
stop

install_from "$SYNTRA_ROOT" "$SYNTRA_ROOT/target/upgrade-current"
start
# Four writers, each paced to TPS/4 transactions per second. A write that fails
# is counted and not retried; a write that succeeds is counted exactly once.
writer() {
    local ok=0 bad=0 end=$((SECONDS + SECONDS_RUN)) id sql
    while ((SECONDS < end)); do
        id=$((RANDOM % 5000 + 1))
        # unslop-ignore NO_SQL_INTERPOLATION -- id is integer arithmetic on $RANDOM, set just above.
        sql="UPDATE feed SET v = v + 1 WHERE id = $id"
        if "$BIN/psql" -h "$SOCKET" -p "$PORT" -U upgrader -d upgraded -v ON_ERROR_STOP=1 -AtqX \
            -c "$sql" >/dev/null 2>>"$WORK/writer-$1.err"; then
            ok=$((ok + 1))
        else
            bad=$((bad + 1))
        fi
        sleep "$PACE"
    done
    echo "$ok $bad" >"$WORK/writer-$1.count"
}
# Each psql start costs a few milliseconds, so pace on top of that.
PACE="$(awk -v t="$TPS" 'BEGIN { printf "%.3f", 4 / t }')"
# Stand-in for the maintenance worker: apply and prune twice a second. Until the
# update commits the new library cannot apply, so those calls are allowed to fail;
# once it has, a log that fills up is a failure, and the writers report it.
maintainer() {
    local end=$((SECONDS + SECONDS_RUN))
    while ((SECONDS < end)); do
        q "SELECT applied FROM syntra_apply_prune_capture('feed')" >/dev/null 2>&1 || true
        sleep 0.5
    done
}
WRITERS=()
for w in 1 2 3 4; do
    writer "$w" &
    WRITERS+=($!)
done
maintainer &
WRITERS+=($!)
BENCH="${WRITERS[*]}"
sleep 4
cat >"$WORK/update.sql" <<SQL
SELECT clock_timestamp() AS started \\gset
-- unslop-ignore NO_SQL_INTERPOLATION -- CURRENT is checked as x.y.z above; ALTER EXTENSION takes no bind parameters.
ALTER EXTENSION syntradb UPDATE TO '$CURRENT';
SELECT round(extract(epoch FROM clock_timestamp()-:'started'::timestamptz)*1000);
SQL
UPDATE_MS="$("$BIN/psql" -h "$SOCKET" -p "$PORT" -U upgrader -d upgraded -v ON_ERROR_STOP=1 -AtqX -f "$WORK/update.sql")"
for pid in "${WRITERS[@]}"; do wait "$pid" || fail "a writer process exited with an error"; done
BENCH=""
PROCESSED=0
FAILED=0
for w in 1 2 3 4; do
    read -r ok bad <"$WORK/writer-$w.count" || fail "writer $w left no count"
    PROCESSED=$((PROCESSED + ok))
    FAILED=$((FAILED + bad))
done
if [[ "$FROM" != 0.1.0 && "$FROM" != 0.2.0 ]]; then
    ((FAILED == 0)) || fail "$FAILED writes failed during the $FROM update; see $WORK/writer-*.err"
else
    grep -q "capture configuration is missing or changed" "$WORK"/writer-*.err || fail "no writer failed closed before the update"
    if grep -hv "capture configuration is missing or changed\|^$\|^psql: error\|^CONTEXT:\|^PL/\|^LINE\|^ *\^" "$WORK"/writer-*.err | grep -q .; then
        fail "a writer failed with an unexpected error; see $WORK/writer-*.err"
    fi
    ((FAILED > 0)) || fail "no writer failed before the update; the load did not overlap the closed window"
fi
((PROCESSED > 0)) || fail "no write succeeded after the update"

# unslop-ignore NO_SQL_INTERPOLATION -- CURRENT is only the expected value.
expect "version" "$(q "SELECT extversion FROM pg_extension WHERE extname='syntradb'")" "$CURRENT"
# unslop-ignore NO_SQL_INTERPOLATION -- constant SQL; the sum is only the expected value.
expect "heap sum" "$(q "SELECT sum(v) FROM feed")" "$((START_SUM + PROCESSED))"
# With many events pending the bounded replay tail may be exceeded, and the read
# then falls back to the heap. Either route must match the heap.
AFTER_LOAD="$(q "$READ")"
[[ "$AFTER_LOAD" == "replay:true" || "$AFTER_LOAD" == "heap:true" || "$AFTER_LOAD" == "checkpoint:true" ]] || fail "read after the load: got '$AFTER_LOAD'"
q "SELECT applied FROM syntra_apply_prune_capture('feed')" >/dev/null
expect "drained lag" "$(q "SELECT pending_events FROM syntra_capture_lag('feed')")" "0"
expect "drained read" "$(q "$READ")" "checkpoint:true"
# unslop-ignore NO_SQL_INTERPOLATION -- constant SQL; the sum is only the expected value.
expect "emitted events" "$(q "SELECT emitted FROM syntra_capture_lag('feed')")" "$((PROCESSED + 300))"
# unslop-ignore NO_SQL_INTERPOLATION -- a result message, not SQL.
echo "upgrade load check passed: $FROM -> $CURRENT with $SECONDS_RUN s of writers at $TPS tps; $PROCESSED writes committed and $FAILED failed closed before the update; heap sum, event count and the read after the load ($AFTER_LOAD) agree, and the drained feed reads from its checkpoint; ALTER EXTENSION UPDATE took $UPDATE_MS ms; work dir $WORK"
