#!/usr/bin/env bash
# Downgrade a live database from 0.4.0 to 0.3.0 and back, both built from their
# archived sources (the workspace is newer; see downgrade-0.5.0-check.sh):
# create on 0.3.0, update to 0.4.0, write partition deltas, fold them, update to
# 0.3.0 while the 0.4.0 library is installed, install the 0.3.0 library and restart.
# The downgraded catalog must match a fresh 0.3.0 install, history captured on
# 0.4.0 must replay on 0.3.0, and a second update to 0.4.0 must work. Builds into a
# private copy of the production PostgreSQL install; the original is untouched.
set -euo pipefail
SYNTRA_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$SYNTRA_ROOT"
bash scripts/setup-production.sh
export PGRX_HOME="$SYNTRA_ROOT/.pgrx"
export PATH="$SYNTRA_ROOT/.tools/bin:$PATH"
OLD=0.3.0
CURRENT=0.4.0
WORK="$SYNTRA_ROOT/target/downgrade-check/$(date +%Y%m%d-%H%M%S)-$$"
mkdir -p "$WORK/base" "$WORK/current"
cp -R .tools/postgresql-production/18.6/install "$WORK/pg"
PG_CONFIG="$WORK/pg/bin/pg_config"
BIN="$WORK/pg/bin"
tar xzf "build-support/upgrade-baselines/syntradb-$OLD-src.tgz" -C "$WORK/base"
tar xzf "build-support/upgrade-baselines/syntradb-$CURRENT-src.tgz" -C "$WORK/current"
install_from() {
    (cd "$1/crates/syntra-pg" && CARGO_TARGET_DIR="$2" cargo pgrx install --release \
        --cargo=--locked --pg-config "$PG_CONFIG")
}
install_old() { install_from "$WORK/base" "$SYNTRA_ROOT/target/upgrade-baseline-$OLD"; }
install_current() { install_from "$WORK/current" "$SYNTRA_ROOT/target/upgrade-baseline-$CURRENT"; }

SOCKET="$(mktemp -d /tmp/syntra-downgrade.XXXXXX)"
"$BIN/initdb" -D "$WORK/data" --username=downgrader --auth=trust --no-locale --encoding=UTF8 >/dev/null
printf "\nlisten_addresses = ''\nunix_socket_directories = '%s'\nport = 28935\n" "$SOCKET" \
    >>"$WORK/data/postgresql.conf"
start() { "$BIN/pg_ctl" -D "$WORK/data" -l "$WORK/postgres.log" -w -t 30 start >/dev/null; }
stop() { "$BIN/pg_ctl" -D "$WORK/data" -w -t 30 stop -m fast >/dev/null; }
trap 'stop 2>/dev/null || true' EXIT
q() { "$BIN/psql" -h "$SOCKET" -p 28935 -U downgrader -d "$1" -v ON_ERROR_STOP=1 -AtqX -c "$2"; }
expect() {
    if [[ "$2" != "$3" ]]; then
        echo "downgrade check failed: $1: got '$2', expected '$3'" >&2
        exit 1
    fi
}
READ="SELECT s.route||':'||(ROW(s.rows,s.non_nulls,s.sum,s.min,s.max) IS NOT DISTINCT FROM ROW(n.rows,n.non_nulls,n.sum,n.min,n.max))
    FROM syntra_checkpoint_int4_summary('feed','v',true) AS s,
    (SELECT count(*) AS rows,count(v) AS non_nulls,sum(v),min(v),max(v) FROM feed) AS n"
READ8="SELECT s.route||':'||(ROW(s.rows,s.non_nulls,s.sum,s.min,s.max) IS NOT DISTINCT FROM ROW(n.rows,n.non_nulls,n.sum,n.min,n.max))
    FROM syntra_checkpoint_int8_summary('wide','v',true) AS s,
    (SELECT count(*) AS rows,count(v) AS non_nulls,sum(v),min(v),max(v) FROM wide) AS n"
LAG="SELECT emitted||','||applied||','||pending_events FROM syntra_capture_lag('feed')"
now_ms() { python3 -c 'import time; print(time.monotonic_ns() // 1000000)'; }
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

install_old
start
q postgres "CREATE DATABASE downgraded"
q postgres "CREATE DATABASE fresh"
q downgraded "CREATE EXTENSION syntradb VERSION '$OLD'"
q downgraded "CREATE TABLE feed(id int4 PRIMARY KEY,v int4); INSERT INTO feed SELECT g,g FROM generate_series(1,4000) AS g"
q downgraded "SELECT syntra_seed_capture('feed','id','v',4096)" >/dev/null
q downgraded "SELECT syntra_apply_capture('feed')" >/dev/null
q downgraded "CREATE TABLE wide(id int8 PRIMARY KEY,v int8); INSERT INTO wide SELECT g*5000000000,g*3000000000 FROM generate_series(1,2000) AS g"
q downgraded "SELECT syntra_seed_capture('wide','id','v',4096)" >/dev/null
q downgraded "SELECT syntra_apply_capture('wide')" >/dev/null
stop

install_current
start
# unslop-ignore NO_SQL_INTERPOLATION -- OLD and CURRENT are literals; ALTER EXTENSION takes no bind parameters.
q downgraded "ALTER EXTENSION syntradb UPDATE TO '$CURRENT'"
# Small changes on 0.4.0 write partition deltas and version 2 manifests.
q downgraded "UPDATE feed SET v=-v WHERE id%500=0"
q downgraded "UPDATE wide SET v=-9000000000000000000 WHERE id%(5000000000*300)=0"
q downgraded "SELECT applied FROM syntra_apply_prune_capture('feed')" >/dev/null
q downgraded "SELECT applied FROM syntra_apply_prune_capture('wide')" >/dev/null
expect "deltas on $CURRENT" "$(q downgraded "SELECT count(DISTINCT source_relation) FROM syntra.capture_chunks WHERE delta IS NOT NULL")" "2"
expect "manifests on $CURRENT" "$(q downgraded "SELECT string_agg(octet_length(manifest)::text,',') FROM syntra.capture_checkpoints")" "8272,8272"

# With deltas present the downgrade must refuse and change nothing.
# unslop-ignore NO_SQL_INTERPOLATION -- OLD and CURRENT are literals; ALTER EXTENSION takes no bind parameters.
if q downgraded "ALTER EXTENSION syntradb UPDATE TO '$OLD'" >/dev/null 2>"$WORK/refused.err"; then
    echo "downgrade check failed: the downgrade ran with partition deltas present" >&2
    exit 1
fi
grep -q "partition deltas remain" "$WORK/refused.err" || {
    echo "downgrade check failed: the downgrade was refused for another reason: $(head -1 "$WORK/refused.err")" >&2
    exit 1
}
# unslop-ignore NO_SQL_INTERPOLATION -- OLD and CURRENT are literals; ALTER EXTENSION takes no bind parameters.
expect "version after refusal" "$(q downgraded "SELECT extversion FROM pg_extension WHERE extname='syntradb'")" "$CURRENT"
# unslop-ignore NO_SQL_INTERPOLATION -- READ and READ8 are constant queries defined above.
expect "read after refusal" "$(q downgraded "$READ")" "checkpoint:true"

# Fold, then leave history captured on 0.4.0 pending for 0.3.0 to replay.
expect "fold" "$(q downgraded "SELECT bool_and(syntra_fold_capture(source_relation::regclass)>0) FROM syntra.capture_checkpoints")" "t"
expect "folded manifests" "$(q downgraded "SELECT string_agg(octet_length(manifest)::text,',') FROM syntra.capture_checkpoints")" "5200,5200"
# A numeric feed, which 0.3.0 cannot capture, must also block the downgrade
# until it is stopped.
q downgraded "CREATE TABLE prices(id int4 PRIMARY KEY,v numeric(10,2)); INSERT INTO prices VALUES(1,12.30),(2,NULL)"
q downgraded "SELECT syntra_seed_capture('prices','id','v',16)" >/dev/null
# unslop-ignore NO_SQL_INTERPOLATION -- OLD and CURRENT are literals; ALTER EXTENSION takes no bind parameters.
if q downgraded "ALTER EXTENSION syntradb UPDATE TO '$OLD'" >/dev/null 2>"$WORK/numeric.err"; then
    echo "downgrade check failed: the downgrade ran with a numeric feed" >&2
    exit 1
fi
grep -q "numeric capture feeds remain" "$WORK/numeric.err" || {
    echo "downgrade check failed: the numeric feed's downgrade was refused for another reason: $(head -1 "$WORK/numeric.err")" >&2
    exit 1
}
expect "stop the numeric feed" "$(q downgraded "SELECT syntra_stop_capture('prices')")" "t"

q downgraded "UPDATE feed SET v=NULL WHERE id BETWEEN 11 AND 13"
q downgraded "DELETE FROM feed WHERE id=100"
q downgraded "INSERT INTO feed VALUES(5001,7)"
# unslop-ignore NO_SQL_INTERPOLATION -- READ and READ8 are constant queries defined above.
expect "pending read on $CURRENT" "$(q downgraded "$READ")" "replay:true"
LAG_BEFORE="$(q downgraded "$LAG")"

# A capture writer must not wait for the downgrade: it locks only checkpoint
# tables. The update stays open two seconds.
# unslop-ignore NO_SQL_INTERPOLATION -- OLD and CURRENT are literals; ALTER EXTENSION takes no bind parameters.
q downgraded "BEGIN; ALTER EXTENSION syntradb UPDATE TO '$OLD'; SELECT pg_sleep(2); COMMIT;" >/dev/null &
UPDATER=$!
sleep 0.5
STARTED=$(now_ms)
q downgraded "UPDATE feed SET v=v+100 WHERE id=60"
WAITED=$(( $(now_ms) - STARTED ))
wait "$UPDATER"
if ((WAITED >= 1000)); then
    echo "downgrade check failed: a capture write waited ${WAITED} ms for the downgrade" >&2
    exit 1
fi
# unslop-ignore NO_SQL_INTERPOLATION -- OLD and CURRENT are literals; ALTER EXTENSION takes no bind parameters.
expect "version after the downgrade" "$(q downgraded "SELECT extversion FROM pg_extension WHERE extname='syntradb'")" "$OLD"
stop

install_old
start
# The downgraded catalog must match a fresh 0.3.0 install except for the delta
# column, which the downgrade keeps, NULL in every row, and its length check.
q fresh "CREATE EXTENSION syntradb VERSION '$OLD'"
q downgraded "$FINGERPRINT" >"$WORK/downgraded.catalog"
q fresh "$FINGERPRINT" >"$WORK/fresh.catalog"
KEPT='^(col capture_chunks\.delta |con syntra\.capture_chunks capture_chunks_delta_check )'
expect "kept delta column" "$(grep -cE "$KEPT" "$WORK/downgraded.catalog")" "2"
expect "delta values on $OLD" "$(q downgraded "SELECT count(*) FROM syntra.capture_chunks WHERE delta IS NOT NULL")" "0"
grep -vE "$KEPT" "$WORK/downgraded.catalog" >"$WORK/downgraded-without-delta.catalog"
if ! diff -u "$WORK/fresh.catalog" "$WORK/downgraded-without-delta.catalog" >"$WORK/catalog.diff"; then
    cat "$WORK/catalog.diff" >&2
    echo "downgrade check failed: downgraded catalog differs from a fresh $OLD install" >&2
    exit 1
fi
# History from 0.4.0, including the write made during the downgrade, replays on 0.3.0.
expect "write during the downgrade" "$(q downgraded "SELECT v FROM feed WHERE id=60")" "160"
expect "lag on $OLD" "$(q downgraded "$LAG")" "$(awk -F, '{print $1+1","$2","$3+1}' <<<"$LAG_BEFORE")"
# unslop-ignore NO_SQL_INTERPOLATION -- READ and READ8 are constant queries defined above.
expect "replayed read on $OLD" "$(q downgraded "$READ")" "replay:true"
# unslop-ignore NO_SQL_INTERPOLATION -- READ and READ8 are constant queries defined above.
expect "int8 read on $OLD" "$(q downgraded "$READ8")" "checkpoint:true"
q downgraded "UPDATE feed SET v=v+5 WHERE id<=20"
q downgraded "SELECT applied FROM syntra_apply_prune_capture('feed')" >/dev/null
q downgraded "UPDATE wide SET v=1 WHERE id=5000000000"
q downgraded "SELECT applied FROM syntra_apply_prune_capture('wide')" >/dev/null
expect "drained lag on $OLD" "$(q downgraded "SELECT pending_events FROM syntra_capture_lag('feed')")" "0"
# unslop-ignore NO_SQL_INTERPOLATION -- READ and READ8 are constant queries defined above.
expect "drained read on $OLD" "$(q downgraded "$READ")" "checkpoint:true"
# unslop-ignore NO_SQL_INTERPOLATION -- READ and READ8 are constant queries defined above.
expect "drained int8 read on $OLD" "$(q downgraded "$READ8")" "checkpoint:true"
stop

# And up again: 0.4.0 reads the checkpoints 0.3.0 wrote and writes deltas.
install_current
start
# unslop-ignore NO_SQL_INTERPOLATION -- OLD and CURRENT are literals; ALTER EXTENSION takes no bind parameters.
q downgraded "ALTER EXTENSION syntradb UPDATE TO '$CURRENT'"
q downgraded "UPDATE feed SET v=0 WHERE id=1"
q downgraded "SELECT applied FROM syntra_apply_prune_capture('feed')" >/dev/null
# unslop-ignore NO_SQL_INTERPOLATION -- READ and READ8 are constant queries defined above.
expect "read after the second update" "$(q downgraded "$READ")" "checkpoint:true"
# unslop-ignore NO_SQL_INTERPOLATION -- READ and READ8 are constant queries defined above.
expect "int8 read after the second update" "$(q downgraded "$READ8")" "checkpoint:true"
expect "deltas after the second update" "$(q downgraded "SELECT count(*) FROM syntra.capture_chunks WHERE delta IS NOT NULL")" "1"
echo "downgrade check passed: $CURRENT -> $OLD -> $CURRENT with captured history, $(wc -l <"$WORK/fresh.catalog" | tr -d ' ') catalog entries identical to a fresh $OLD install apart from the kept delta column, a capture write waited ${WAITED} ms during the downgrade; work dir $WORK"
