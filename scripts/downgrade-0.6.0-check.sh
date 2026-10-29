#!/usr/bin/env bash
# Downgrade a live database from 0.6.0 to 0.5.0 and back, both built from baselines.
# 0.6.0 fires the capture row trigger at commit; 0.5.0 accepts only an immediate
# trigger, so the downgrade replaces it. A writer that has queued changes but not
# committed when the downgrade starts must make the downgrade wait, and its
# changes must be captured. History captured on 0.6.0 and left pending must apply
# on 0.5.0. Checks the trigger kind after each step, that reads match the heap,
# the catalog against fresh 0.5.0 and 0.6.0 installs, and a second update.
# Builds into a private copy of the production PostgreSQL install; the original
# is untouched.
set -euo pipefail
SYNTRA_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$SYNTRA_ROOT"
bash scripts/setup-production.sh
export PGRX_HOME="$SYNTRA_ROOT/.pgrx"
export PATH="$SYNTRA_ROOT/.tools/bin:$PATH"
OLD=0.5.0
CURRENT=0.6.0
WORK="$SYNTRA_ROOT/target/downgrade-0.6.0-check/$(date +%Y%m%d-%H%M%S)-$$"
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

SOCKET="$(mktemp -d /tmp/syntra-downgrade6.XXXXXX)"
"$BIN/initdb" -D "$WORK/data" --username=downgrader --auth=trust --no-locale --encoding=UTF8 >/dev/null
printf "\nlisten_addresses = ''\nunix_socket_directories = '%s'\nport = 28948\n" "$SOCKET" \
    >>"$WORK/data/postgresql.conf"
start() { "$BIN/pg_ctl" -D "$WORK/data" -l "$WORK/postgres.log" -w -t 30 start >/dev/null; }
stop() { "$BIN/pg_ctl" -D "$WORK/data" -w -t 30 stop -m fast >/dev/null; }
trap 'stop 2>/dev/null || true' EXIT
q() { "$BIN/psql" -h "$SOCKET" -p 28948 -U downgrader -d "$1" -v ON_ERROR_STOP=1 -AtqX -c "$2"; }
expect() {
    if [[ "$2" != "$3" ]]; then
        echo "downgrade check failed: $1: got '$2', expected '$3'" >&2
        exit 1
    fi
}
now_ms() { python3 -c 'import time; print(time.monotonic_ns() // 1000000)'; }
READ="SELECT s.route||':'||(ROW(s.rows,s.non_nulls,s.sum,s.min,s.max) IS NOT DISTINCT FROM ROW(n.rows,n.non_nulls,n.sum,n.min,n.max))
    FROM syntra_checkpoint_int4_summary('feed','v',true) AS s,
    (SELECT count(*) AS rows,count(v) AS non_nulls,sum(v),min(v),max(v) FROM feed) AS n"
TRIGGER="SELECT t.tgdeferrable::text||':'||t.tginitdeferred::text||':'||t.tgenabled::text
    FROM syntra.capture_heads AS h JOIN pg_trigger AS t ON t.oid=h.row_trigger AND t.tgrelid=h.source_relation
    JOIN syntra.capture_bindings AS b ON b.source_relation=h.source_relation AND b.row_trigger=h.row_trigger"
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
q postgres "CREATE DATABASE fresh_old"
# unslop-ignore NO_SQL_INTERPOLATION -- OLD is a literal; CREATE EXTENSION takes no bind parameters.
q fresh_old "CREATE EXTENSION syntradb VERSION '$OLD'"
q fresh_old "$FINGERPRINT" >"$WORK/fresh_old.catalog"
stop

install_current
start
q postgres "CREATE DATABASE downgraded"
q postgres "CREATE DATABASE fresh_current"
q downgraded "CREATE EXTENSION syntradb"
q downgraded "CREATE TABLE feed(id int4 PRIMARY KEY,v int4); INSERT INTO feed SELECT g,g FROM generate_series(1,100) AS g"
q downgraded "SELECT syntra_seed_capture('feed','id','v',64)" >/dev/null
expect "trigger on $CURRENT" "$(q downgraded "$TRIGGER")" "true:true:A"
q downgraded "UPDATE feed SET v=v+1 WHERE id<=10"
q downgraded "SELECT applied FROM syntra_apply_prune_capture('feed')" >/dev/null
# Pending history captured on 0.6.0, then a writer that holds queued changes
# open across the start of the downgrade.
q downgraded "UPDATE feed SET v=-v WHERE id BETWEEN 11 AND 13; DELETE FROM feed WHERE id=100"
expect "pending on $CURRENT" "$(q downgraded "SELECT count(*) FROM syntra.capture_events")" "4"
q downgraded "BEGIN; UPDATE feed SET v=v+1000 WHERE id=7; INSERT INTO feed VALUES(101,NULL); SELECT pg_sleep(3); COMMIT;" >/dev/null &
WRITER=$!
sleep 0.5
expect "queued changes not yet written" "$(q downgraded "SELECT count(*) FROM syntra.capture_events")" "4"
STARTED=$(now_ms)
# unslop-ignore NO_SQL_INTERPOLATION -- OLD is a literal; ALTER EXTENSION takes no bind parameters.
q downgraded "ALTER EXTENSION syntradb UPDATE TO '$OLD'"
WAITED=$(( $(now_ms) - STARTED ))
wait "$WRITER"
if [ "$WAITED" -lt 1500 ]; then
    echo "downgrade check failed: the downgrade finished in ${WAITED} ms while a writer held queued changes" >&2
    exit 1
fi
expect "writer captured across the downgrade" "$(q downgraded "SELECT count(*) FROM syntra.capture_events")" "6"
# unslop-ignore NO_SQL_INTERPOLATION -- constant SQL; OLD is only the expected value.
expect "version" "$(q downgraded "SELECT extversion FROM pg_extension WHERE extname='syntradb'")" "$OLD"
expect "trigger after the downgrade" "$(q downgraded "$TRIGGER")" "false:false:A"
# unslop-ignore NO_SQL_INTERPOLATION -- READ is the constant query defined above.
expect "read with the $CURRENT library" "$(q downgraded "$READ")" "replay:true"
stop

install_old
start
q downgraded "UPDATE feed SET v=v+5 WHERE id<=20"
expect "pending on $OLD" "$(q downgraded "SELECT count(*) FROM syntra.capture_events")" "26"
# unslop-ignore NO_SQL_INTERPOLATION -- READ is the constant query defined above.
expect "replay on $OLD" "$(q downgraded "$READ")" "replay:true"
q downgraded "SELECT applied FROM syntra_apply_prune_capture('feed')" >/dev/null
# unslop-ignore NO_SQL_INTERPOLATION -- READ is the constant query defined above.
expect "checkpoint on $OLD" "$(q downgraded "$READ")" "checkpoint:true"
q downgraded "$FINGERPRINT" >"$WORK/downgraded.catalog"
if ! diff -u "$WORK/fresh_old.catalog" "$WORK/downgraded.catalog" >"$WORK/old.diff"; then
    cat "$WORK/old.diff" >&2
    echo "downgrade check failed: downgraded catalog differs from a fresh $OLD install" >&2
    exit 1
fi
stop

install_current
start
# unslop-ignore NO_SQL_INTERPOLATION -- CURRENT is the literal 0.6.0 above; ALTER EXTENSION takes no bind parameters.
q downgraded "ALTER EXTENSION syntradb UPDATE TO '$CURRENT'"
# unslop-ignore NO_SQL_INTERPOLATION -- TRIGGER is the constant query defined above.
expect "trigger after the second update" "$(q downgraded "$TRIGGER")" "true:true:A"
q fresh_current "CREATE EXTENSION syntradb"
q downgraded "$FINGERPRINT" >"$WORK/upgraded.catalog"
q fresh_current "$FINGERPRINT" >"$WORK/fresh_current.catalog"
if ! diff -u "$WORK/fresh_current.catalog" "$WORK/upgraded.catalog" >"$WORK/current.diff"; then
    cat "$WORK/current.diff" >&2
    # unslop-ignore NO_SQL_INTERPOLATION -- an error message, not SQL.
    echo "downgrade check failed: catalog after the second update differs from a fresh $CURRENT install" >&2
    exit 1
fi
q downgraded "UPDATE feed SET v=v-1 WHERE id=1"
# unslop-ignore NO_SQL_INTERPOLATION -- READ is the constant query defined above.
expect "replay after the second update" "$(q downgraded "$READ")" "replay:true"
q downgraded "SELECT applied FROM syntra_apply_prune_capture('feed')" >/dev/null
# unslop-ignore NO_SQL_INTERPOLATION -- READ is the constant query defined above.
expect "checkpoint after the second update" "$(q downgraded "$READ")" "checkpoint:true"
echo "downgrade check passed: $CURRENT -> $OLD -> $CURRENT; the downgrade waited ${WAITED} ms for a writer with queued changes and captured them, $(wc -l <"$WORK/fresh_old.catalog" | tr -d ' ') catalog entries identical to a fresh $OLD install, $(wc -l <"$WORK/fresh_current.catalog" | tr -d ' ') to a fresh $CURRENT install after the second update; work dir $WORK"
