#!/usr/bin/env bash
# Upgrade a live database from an archived release (0.1.0 by default, or the
# version given as $1) to the current release and compare the result with a
# fresh install. Builds the archived source and this tree into a private copy of
# the production PostgreSQL install; the original is untouched.
set -euo pipefail
SYNTRA_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$SYNTRA_ROOT"
bash scripts/setup-production.sh
export PGRX_HOME="$SYNTRA_ROOT/.pgrx"
export PATH="$SYNTRA_ROOT/.tools/bin:$PATH"
FROM="${1:-0.1.0}"
if [[ ! -f "build-support/upgrade-baselines/syntradb-$FROM-src.tgz" ]]; then
    echo "upgrade check failed: no archived source for '$FROM'" >&2
    exit 1
fi
CURRENT="$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)"
if [[ ! "$CURRENT" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "upgrade check failed: workspace version '$CURRENT' is not x.y.z" >&2
    exit 1
fi
WORK="$SYNTRA_ROOT/target/upgrade-check/$(date +%Y%m%d-%H%M%S)-$$"
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

SOCKET="$(mktemp -d /tmp/syntra-upgrade.XXXXXX)"
"$BIN/initdb" -D "$WORK/data" --username=upgrader --auth=trust --no-locale --encoding=UTF8 >/dev/null
printf "\nlisten_addresses = ''\nunix_socket_directories = '%s'\nport = 28931\n" "$SOCKET" \
    >>"$WORK/data/postgresql.conf"
start() { "$BIN/pg_ctl" -D "$WORK/data" -l "$WORK/postgres.log" -w -t 30 start >/dev/null; }
stop() { "$BIN/pg_ctl" -D "$WORK/data" -w -t 30 stop -m fast >/dev/null; }
trap 'stop 2>/dev/null || true' EXIT
q() { "$BIN/psql" -h "$SOCKET" -p 28931 -U upgrader -d "$1" -v ON_ERROR_STOP=1 -AtqX -c "$2"; }
expect() {
    if [[ "$2" != "$3" ]]; then
        echo "upgrade check failed: $1: got '$2', expected '$3'" >&2
        exit 1
    fi
}
# The checkpoint read must match the heap exactly, through the named route:
# replay while captured history is pending, checkpoint once it is applied.
READ="SELECT s.route||':'||(ROW(s.rows,s.non_nulls,s.sum,s.min,s.max) IS NOT DISTINCT FROM ROW(n.rows,n.non_nulls,n.sum,n.min,n.max))
    FROM syntra_checkpoint_int4_summary('feed','v',true) AS s,
    (SELECT count(*) AS rows,count(v) AS non_nulls,sum(v),min(v),max(v) FROM feed) AS n"

start
q postgres "CREATE DATABASE upgraded"
q postgres "CREATE DATABASE fresh"
# unslop-ignore NO_SQL_INTERPOLATION -- FROM names an archive that exists, checked above.
q upgraded "CREATE EXTENSION syntradb VERSION '$FROM'"
q upgraded "CREATE TABLE feed(id int4 PRIMARY KEY,v int4); INSERT INTO feed SELECT g,g FROM generate_series(1,100) AS g"
q upgraded "SELECT syntra_seed_capture('feed','id','v',64)" >/dev/null
q upgraded "SELECT syntra_apply_capture('feed')" >/dev/null
q upgraded "UPDATE feed SET v=v+1 WHERE id<=10"
q upgraded "SELECT applied FROM syntra_apply_prune_capture('feed')" >/dev/null
# Leave history that the old release captured but never applied: three updates, a delete
# and an insert. The upgraded release must replay it.
q upgraded "UPDATE feed SET v=-v WHERE id BETWEEN 11 AND 13"
q upgraded "DELETE FROM feed WHERE id=100"
q upgraded "INSERT INTO feed VALUES(101,NULL)"
# Retained events: 0.2.0 records new positions as marks, not on the head row.
expect "$FROM pending events" "$(q upgraded "SELECT count(*) FROM syntra.capture_events")" "5"
expect "$FROM read" "$(q upgraded "$READ")" "replay:true"
stop

install_from "$SYNTRA_ROOT" "$SYNTRA_ROOT/target/upgrade-current"
start
if [[ "$FROM" == 0.3.0 ]]; then
    # 0.4.0 changes only checkpoint storage. Capture writes keep the 0.3.0 event
    # format and are captured; maintenance fails closed until the update, and a
    # reader that cannot read the old chunk rows falls back to the heap.
    q upgraded "UPDATE feed SET v=0 WHERE id=1"
    BEFORE_EVENTS=1
    # unslop-ignore NO_SQL_INTERPOLATION -- constant SQL; only the stderr path is a variable.
    if q upgraded "SELECT syntra_apply_capture('feed')" >/dev/null 2>"$WORK/apply-before.err"; then
        echo "upgrade check failed: maintenance succeeded before ALTER EXTENSION UPDATE" >&2
        exit 1
    fi
    # unslop-ignore NO_SQL_INTERPOLATION -- READ is the constant query defined above.
    expect "read before the update" "$(q upgraded "$READ")" "heap:true"
else
    # A new library on the old schema must refuse capture writes rather than guess.
    # unslop-ignore NO_SQL_INTERPOLATION -- constant SQL; only the stderr path is a variable.
    if q upgraded "UPDATE feed SET v=0 WHERE id=1" 2>"$WORK/closed.err"; then
        echo "upgrade check failed: a capture write succeeded before ALTER EXTENSION UPDATE" >&2
        exit 1
    fi
    grep -q "capture configuration is missing or changed" "$WORK/closed.err"
    BEFORE_EVENTS=0
fi
# A writer that arrives while the update is still open must never be captured
# against a half-updated catalog. From 0.1.0 it fails closed: the 0.2.0 step
# changes the bindings before the writer reads them. From 0.2.0 it waits: the
# 0.3.0 step holds ACCESS EXCLUSIVE on capture_bindings, and after commit the
# writer reads the new bindings and is captured. From 0.3.0 it is captured at
# once: the 0.4.0 step locks only checkpoint tables, which writers never touch.
# The update stays open two seconds, so a writer that returns in under a second
# did not wait.
# unslop-ignore NO_SQL_INTERPOLATION -- CURRENT is checked as x.y.z above; ALTER EXTENSION takes no bind parameters.
q upgraded "BEGIN; ALTER EXTENSION syntradb UPDATE TO '$CURRENT'; SELECT pg_sleep(2); COMMIT;" >/dev/null &
UPDATER=$!
sleep 0.5
now_ms() { python3 -c 'import time; print(time.monotonic_ns() // 1000000)'; }
STARTED=$(now_ms)
# unslop-ignore NO_SQL_INTERPOLATION -- constant SQL; only the stderr path is a variable.
if q upgraded "UPDATE feed SET v=v+100 WHERE id=60" 2>"$WORK/during.err"; then
    DURING=captured
else
    DURING=closed
    grep -q "capture configuration is missing or changed" "$WORK/during.err"
fi
WAITED=$(( $(now_ms) - STARTED ))
wait "$UPDATER"
case "$FROM:$DURING" in
0.1.0:closed)
    expect "write during the update rolled back" "$(q upgraded "SELECT v FROM feed WHERE id=60")" "60"
    DURING_EVENTS=0
    ;;
0.2.0:captured)
    if [ "$WAITED" -lt 1000 ]; then
        echo "upgrade check failed: a capture write finished while the update was open" >&2
        exit 1
    fi
    expect "write during the update" "$(q upgraded "SELECT v FROM feed WHERE id=60")" "160"
    DURING_EVENTS=1
    ;;
0.3.0:captured)
    if [ "$WAITED" -ge 1000 ]; then
        echo "upgrade check failed: a capture write waited ${WAITED} ms for the 0.4.0 update" >&2
        exit 1
    fi
    expect "write during the update" "$(q upgraded "SELECT v FROM feed WHERE id=60")" "160"
    DURING_EVENTS=1
    ;;
*)
    # unslop-ignore NO_SQL_INTERPOLATION -- an error message, not SQL.
    echo "upgrade check failed: write during the $FROM update was $DURING" >&2
    exit 1
    ;;
esac
# unslop-ignore NO_SQL_INTERPOLATION -- constant SQL; CURRENT is only the expected value.
expect "version" "$(q upgraded "SELECT extversion FROM pg_extension WHERE extname='syntradb'")" "$CURRENT"
expect "bindings" "$(q upgraded "SELECT count(*) FROM syntra.capture_heads AS h JOIN syntra.capture_bindings AS b USING(source_relation,epoch,tracking_epoch,key_attribute,value_attribute,row_trigger,truncate_trigger,source_identity)")" "1"
expect "replayed $FROM history" "$(q upgraded "$READ")" "replay:true"
q upgraded "UPDATE feed SET v=v+5 WHERE id<=20"
q upgraded "INSERT INTO feed VALUES(102,3)"
q upgraded "DELETE FROM feed WHERE id=50"
expect "mixed history" "$(q upgraded "$READ")" "replay:true"
# 10 applied and 5 pending on $FROM, any write captured before or during the
# update, then 22 more: 20 updates, an insert, a delete.
PENDING=$((27 + BEFORE_EVENTS + DURING_EVENTS))
EMITTED=$((10 + PENDING))
# unslop-ignore NO_SQL_INTERPOLATION -- constant SQL; the counts are only the expected value.
expect "lag" "$(q upgraded "SELECT emitted||','||applied||','||pending_events FROM syntra_capture_lag('feed')")" "$EMITTED,10,$PENDING"
# unslop-ignore NO_SQL_INTERPOLATION -- constant SQL; the counts are only the expected value.
expect "status" "$(q upgraded "SELECT concat_ws(',',source,emitted,applied,pending_events,coalesce(error,'-')) FROM syntra_capture_status()")" "feed,$EMITTED,10,$PENDING,-"
# unslop-ignore NO_SQL_INTERPOLATION -- constant SQL; the counts are only the expected value.
expect "drain" "$(q upgraded "SELECT applied||','||pruned FROM syntra_apply_prune_capture('feed')")" "$EMITTED,$PENDING"
expect "drained lag" "$(q upgraded "SELECT pending_events FROM syntra_capture_lag('feed')")" "0"
expect "headroom" "$(q upgraded "SELECT retained_events||','||live_rows FROM syntra_capture_headroom('feed')")" "0,100"
expect "drained read" "$(q upgraded "$READ")" "checkpoint:true"
# The drain changed a few rows per partition, so it wrote partition deltas.
expect "partition deltas" "$(q upgraded "SELECT count(*)>0 FROM syntra.capture_chunks WHERE delta IS NOT NULL")" "t"
expect "stop" "$(q upgraded "SELECT syntra_stop_capture('feed')")" "t"
q upgraded "SELECT syntra_seed_capture('feed','id','v',64)" >/dev/null
q upgraded "UPDATE feed SET v=1 WHERE id=1"
q upgraded "SELECT syntra_apply_capture('feed')" >/dev/null
expect "reseeded read" "$(q upgraded "$READ")" "checkpoint:true"
# An int8 feed on the upgraded database, with a sum past the bigint range.
q upgraded "CREATE TABLE wide(id int8 PRIMARY KEY,v int8); INSERT INTO wide VALUES(1,9000000000000000000),(5000000000,9000000000000000000),(2,NULL)"
q upgraded "SELECT syntra_seed_capture('wide','id','v',64)" >/dev/null
q upgraded "SELECT syntra_apply_capture('wide')" >/dev/null
q upgraded "UPDATE wide SET v=-1 WHERE id=2"
expect "int8 replay read" "$(q upgraded "SELECT route||':'||sum FROM syntra_checkpoint_int8_summary('wide','v',true)")" "replay:17999999999999999999"
q upgraded "SELECT applied FROM syntra_apply_prune_capture('wide')" >/dev/null
expect "int8 checkpoint read" "$(q upgraded "SELECT route||':'||sum FROM syntra_checkpoint_int8_summary('wide','v')")" "checkpoint:17999999999999999999"

q fresh "CREATE EXTENSION syntradb"
FINGERPRINT="SELECT f FROM (
    SELECT 'rel '||c.relname||' '||c.relkind::text||' '||coalesce(c.relacl::text,'-') AS f
        FROM pg_class AS c WHERE c.relnamespace='syntra'::regnamespace
    UNION ALL SELECT 'col '||c.relname||'.'||a.attname||' '||a.attnum||' '||format_type(a.atttypid,a.atttypmod)
        ||' '||a.attnotnull::text||' '||a.attidentity::text||' '||coalesce(pg_get_expr(d.adbin,d.adrelid),'-')
        FROM pg_attribute AS a JOIN pg_class AS c ON c.oid=a.attrelid
        LEFT JOIN pg_attrdef AS d ON d.adrelid=a.attrelid AND d.adnum=a.attnum
        WHERE c.relnamespace='syntra'::regnamespace AND c.relkind='r' AND a.attnum>0 AND NOT a.attisdropped
    UNION ALL SELECT 'con '||conrelid::regclass::text||' '||conname||' '||pg_get_constraintdef(oid)
        FROM pg_constraint WHERE connamespace='syntra'::regnamespace
    UNION ALL SELECT 'idx '||pg_get_indexdef(i.indexrelid)
        FROM pg_index AS i JOIN pg_class AS c ON c.oid=i.indrelid WHERE c.relnamespace='syntra'::regnamespace
    UNION ALL SELECT 'fn '||p.oid::regprocedure::text||' '||pg_get_function_result(p.oid)||' '||p.provolatile::text
        ||p.proparallel::text||p.prosecdef::text||p.proisstrict::text||' '||coalesce(p.proacl::text,'-')||' '||p.prosrc
        FROM pg_proc AS p JOIN pg_depend AS d ON d.classid='pg_proc'::regclass AND d.objid=p.oid AND d.deptype='e'
        JOIN pg_extension AS e ON e.oid=d.refobjid AND e.extname='syntradb'
    UNION ALL SELECT 'evt '||evtname||' '||evtevent||' '||evtenabled::text||' '||evtfoid::regproc::text FROM pg_event_trigger
    UNION ALL SELECT 'member '||pg_describe_object(classid,objid,objsubid)
        FROM pg_depend WHERE deptype='e' AND refobjid=(SELECT oid FROM pg_extension WHERE extname='syntradb')
) AS q ORDER BY 1"
q upgraded "$FINGERPRINT" >"$WORK/upgraded.catalog"
q fresh "$FINGERPRINT" >"$WORK/fresh.catalog"
if ! diff -u "$WORK/fresh.catalog" "$WORK/upgraded.catalog" >"$WORK/catalog.diff"; then
    cat "$WORK/catalog.diff" >&2
    echo "upgrade check failed: upgraded catalog differs from a fresh $CURRENT install" >&2
    exit 1
fi
echo "upgrade check passed: $FROM -> $CURRENT with captured history, $(wc -l <"$WORK/fresh.catalog" | tr -d ' ') catalog entries identical; work dir $WORK"
