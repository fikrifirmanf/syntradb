#!/usr/bin/env bash
# Ages a captured table under a held repeatable-read snapshot with unthrottled
# soak-writer cycles, and at chosen horizon ages runs EXPLAIN (ANALYZE, BUFFERS)
# on the statements the capture trigger runs per firing, so their buffer counts
# and times can be compared as the old snapshot ages. The SQL below mirrors
# crates/syntra-pg/src/capture.rs (syntra_capture_row, current_configuration,
# position); keep them in step. The maintenance worker drains the feed as in
# hold-control.sh. Writers are paced (RATE, default 200 per second in total):
# unthrottled writers fill the 4,096-event capture log faster than the worker
# drains it and fail with 54000. A background loop applies and prunes the feed,
# as the soak's maintenance workers do; busy refusals are expected and ignored.
# Usage: hold-explain.sh [targets, space-separated] [clients]
set -euo pipefail
TARGETS="${1:-1000 30000 60000 80000}"
CLIENTS="${2:-2}"
RATE="${RATE:-100}"
SYNTRA_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$SYNTRA_ROOT"
bash scripts/setup-production.sh >/dev/null
export PGRX_HOME="$SYNTRA_ROOT/.pgrx"
export PATH="$SYNTRA_ROOT/.tools/bin:$PATH"
WORK="$SYNTRA_ROOT/target/hold-explain/$(date +%Y%m%d-%H%M%S)-$$"
mkdir -p "$WORK"
cp -R .tools/postgresql-production/18.6/install "$WORK/pg"
BIN="$WORK/pg/bin"
(cd crates/syntra-pg && CARGO_TARGET_DIR="$SYNTRA_ROOT/target/upgrade-current" cargo pgrx install \
    --release --cargo=--locked --pg-config "$WORK/pg/bin/pg_config") >/dev/null 2>&1
SOCKET="$(mktemp -d /tmp/syntra-explain.XXXXXX)"
PORT=28963
"$BIN/initdb" -D "$WORK/data" --username=hold --auth=trust --no-locale --encoding=UTF8 >/dev/null
printf "\nlisten_addresses = ''\nunix_socket_directories = '%s'\nport = %s\nshared_preload_libraries = 'syntradb'\nsyntradb.maintenance_database = 'feeds'\nsyntradb.maintenance_interval_ms = 200\n" \
    "$SOCKET" "$PORT" >>"$WORK/data/postgresql.conf"
HOLDER=""
DRAINER=""
cleanup() {
    if [[ -n "$DRAINER" ]]; then kill "$DRAINER" 2>/dev/null || true; fi
    if [[ -n "$HOLDER" ]]; then kill "$HOLDER" 2>/dev/null || true; fi
    "$BIN/pg_ctl" -D "$WORK/data" -w -t 30 stop -m immediate >/dev/null 2>&1 || true
}
trap cleanup EXIT
"$BIN/pg_ctl" -D "$WORK/data" -l "$WORK/data.log" -w -t 60 start >/dev/null
psqlx() { "$BIN/psql" -h "$SOCKET" -p "$PORT" -U hold -d feeds -v ON_ERROR_STOP=1 -AtqX "$@"; }
q() { psqlx -c "$1"; }
"$BIN/createdb" -h "$SOCKET" -p "$PORT" -U hold feeds
q "CREATE EXTENSION syntradb"
q "CREATE TABLE captured(id int4 PRIMARY KEY, v int4); INSERT INTO captured SELECT g, 1000 FROM generate_series(1,10000) AS g"
q "SELECT syntra_seed_capture('captured','id','v',4096)"
cat >"$WORK/captured.sql" <<'SQL'
\set a random(1, 10000)
\set b random(1, 10000)
BEGIN;
UPDATE captured SET v = v - 1 WHERE id = :a;
UPDATE captured SET v = v + 1 WHERE id = :b;
\set temporary 10001 + :client_id
\set moved :temporary + 2
INSERT INTO captured(id, v) VALUES (:temporary, NULL);
UPDATE captured SET id = :moved WHERE id = :temporary;
DELETE FROM captured WHERE id = :moved;
COMMIT;
SQL
psqlx -c "BEGIN ISOLATION LEVEL REPEATABLE READ" -c "SELECT count(*) FROM captured" -c "SELECT pg_sleep(86400)" >/dev/null 2>&1 &
HOLDER=$!
/bin/sleep 2
(while true; do q "SELECT syntra_apply_prune_capture('captured')" >/dev/null 2>&1 || true; /bin/sleep 0.2; done) &
DRAINER=$!
explain_all() {
    local age="$1"
    echo "=== horizon_age=$age"
    q "SELECT 'sizes marks='||pg_total_relation_size('syntra.capture_marks')||' events='||pg_total_relation_size('syntra.capture_events')||' live_marks='||(SELECT count(*) FROM syntra.capture_marks)||' live_events='||(SELECT count(*) FROM syntra.capture_events)"
    psqlx -v ON_ERROR_STOP=0 <<'SQL'
\set src 'captured'
SELECT h.epoch AS e, h.emitted AS f, h.live_rows AS r, h.row_trigger AS tg FROM syntra.capture_heads h WHERE h.source_relation='captured'::regclass \gset
\echo -- 1 head lock and join (FOR NO KEY UPDATE)
BEGIN;
EXPLAIN (ANALYZE, BUFFERS, COSTS OFF, SUMMARY ON) SELECT h.emitted,h.pruned+h.max_events,h.live_rows FROM syntra.capture_heads AS h JOIN syntra.tracked_sources AS s ON s.source_relation=h.source_relation AND s.epoch=h.tracking_epoch WHERE h.source_relation='captured'::regclass AND h.epoch=:e AND h.row_trigger=:tg FOR NO KEY UPDATE OF h;
ROLLBACK;
\echo -- 2 newest mark above the floor (position)
EXPLAIN (ANALYZE, BUFFERS, COSTS OFF, SUMMARY ON) SELECT ordinal,live_rows FROM syntra.capture_marks WHERE epoch=:e AND ordinal>:f ORDER BY ordinal DESC LIMIT 1;
\echo -- 3 binding lookup (current_configuration)
EXPLAIN (ANALYZE, BUFFERS, COSTS OFF, SUMMARY ON) SELECT * FROM syntra.capture_bindings WHERE source_relation='captured'::regclass ORDER BY epoch DESC LIMIT 1;
\echo -- 4 tracked source lookup (current_configuration)
EXPLAIN (ANALYZE, BUFFERS, COSTS OFF, SUMMARY ON) SELECT * FROM syntra.tracked_sources WHERE source_relation='captured'::regclass;
\echo -- 5 event and mark insert (rolled back)
BEGIN;
EXPLAIN (ANALYZE, BUFFERS, COSTS OFF, SUMMARY ON) WITH inserted AS (
 INSERT INTO syntra.capture_events(source_relation,epoch,ordinal,origin_xid,command_id,key,before_present,before_value,after_present,after_value)
 SELECT 'captured'::regclass,:e,(:f+100000000)+e.position,pg_catalog.pg_current_xact_id(),0,e.key,e.bp,e.bv,e.ap,e.av
 FROM unnest(ARRAY[1::int8],ARRAY[true],ARRAY[5::int8],ARRAY[true],ARRAY[6::int8]) WITH ORDINALITY AS e(key,bp,bv,ap,av,position) RETURNING ordinal
), marked AS (
 INSERT INTO syntra.capture_marks(source_relation,epoch,ordinal,live_rows) VALUES('captured'::regclass,:e,(:f+100000000)+1,:r)
) SELECT count(*) FROM inserted;
ROLLBACK;
SQL
}
age() { q "SELECT syntra_cleanup_horizon_age()"; }
read -r -a targets <<<"$TARGETS"
next=0
failures=0
for chunk in $(seq 1 120); do
    a="$(age)"
    while (( next < ${#targets[@]} )) && (( a >= targets[next] )); do
        explain_all "$a" | tee -a "$WORK/explain.log"
        next=$((next + 1))
    done
    (( next >= ${#targets[@]} )) && break
    if (( failures >= 5 )); then echo "giving up: five consecutive failed chunks" | tee -a "$WORK/explain.log"; break; fi
    out="$("$BIN/pgbench" -h "$SOCKET" -p "$PORT" -U hold -n -c "$CLIENTS" -j "$CLIENTS" -R "$RATE" -T 30 -f "$WORK/captured.sql" feeds 2>&1)" && failures=0 || { failures=$((failures + 1)); echo "pgbench failed: $(printf '%s\n' "$out" | grep -i 'error\|abort' | head -2)" | tee -a "$WORK/explain.log"; }
    printf '%s\n' "$out" | awk '/^tps = /{tps=$3} /^latency average/{lat=$4} /^number of failed/{fail=$5} END{print "chunk tps="tps" latency_ms="lat" failed="fail}' | tee -a "$WORK/explain.log"
done
echo "work dir $WORK"
