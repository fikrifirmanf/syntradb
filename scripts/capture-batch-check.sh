#!/usr/bin/env bash
# Committed-transaction checks for batched capture (docs/capture-batching-design.md).
# pg_test fixtures never commit and run inside a portal, so they cannot reach the
# commit-time path. This runs real sessions in a private cluster: exact events and
# marks for multi-row transactions, rollback, savepoints, mixed immediate and
# deferred firing, a full log, two sources, prepared transactions, and a paced
# concurrent writer load whose drained summary must match native SQL.
set -euo pipefail
SYNTRA_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$SYNTRA_ROOT"
bash scripts/setup-production.sh >/dev/null
export PGRX_HOME="$SYNTRA_ROOT/.pgrx"
export PATH="$SYNTRA_ROOT/.tools/bin:$PATH"
WORK="$SYNTRA_ROOT/target/capture-batch-check/$(date +%Y%m%d-%H%M%S)-$$"
mkdir -p "$WORK"
cp -R .tools/postgresql-production/18.6/install "$WORK/pg"
BIN="$WORK/pg/bin"
(cd crates/syntra-pg && CARGO_TARGET_DIR="$SYNTRA_ROOT/target/upgrade-current" cargo pgrx install \
    --release --cargo=--locked --pg-config "$WORK/pg/bin/pg_config") >/dev/null 2>&1
SOCKET="$(mktemp -d /tmp/syntra-batch.XXXXXX)"
PORT=28947
DATA="$WORK/data"
"$BIN/initdb" -D "$DATA" --username=batch --auth=trust --no-locale --encoding=UTF8 >/dev/null
printf "\nlisten_addresses = ''\nunix_socket_directories = '%s'\nport = %s\nmax_prepared_transactions = 8\nshared_preload_libraries = 'syntradb'\n" \
    "$SOCKET" "$PORT" >>"$DATA/postgresql.conf"
"$BIN/pg_ctl" -D "$DATA" -l "$WORK/postgres.log" -w -t 60 start >/dev/null
cleanup() { "$BIN/pg_ctl" -D "$DATA" -w -t 30 stop -m immediate >/dev/null 2>&1 || true; }
trap cleanup EXIT
psqlc() { "$BIN/psql" -h "$SOCKET" -p "$PORT" -U batch -d feeds -v ON_ERROR_STOP=1 -AtqX "$@"; }
q() { psqlc -c "$1"; }
fail() { echo "capture batch check failed: $1" >&2; exit 1; }
expect() { # name actual expected
    [[ "$2" == "$3" ]] || fail "$1: got '$2', expected '$3'"
    echo "ok $1"
}
"$BIN/createdb" -h "$SOCKET" -p "$PORT" -U batch feeds
q "CREATE EXTENSION syntradb" >/dev/null
EVENTS="SELECT coalesce(string_agg(ordinal||':'||key||':'||coalesce(before_value::text,'n')||'>'||coalesce(after_value::text,'n'),' ' ORDER BY ordinal),'') FROM syntra.capture_events WHERE source_relation='%s'::regclass"
MARKS="SELECT coalesce(string_agg(ordinal||':'||live_rows,' ' ORDER BY ordinal),'') FROM syntra.capture_marks WHERE source_relation='%s'::regclass"
events() { q "$(printf "$EVENTS" "$1")"; }
marks() { q "$(printf "$MARKS" "$1")"; }
newtable() { # name rows limit
    q "CREATE TABLE $1(id int4 PRIMARY KEY,v int4); INSERT INTO $1 SELECT g,g*10 FROM generate_series(1,$2) AS g; SELECT syntra_seed_capture('$1','id','v',$3)" >/dev/null
}

# 1. One transaction with every kind of change: one insert and one mark.
newtable t1 3 64
psqlc <<'SQL' >/dev/null
BEGIN;
UPDATE t1 SET v = 11 WHERE id = 1;
INSERT INTO t1 VALUES (4, 40);
UPDATE t1 SET id = 5 WHERE id = 2;
DELETE FROM t1 WHERE id = 3;
INSERT INTO t1 VALUES (6, 60);
COMMIT;
SQL
expect "mixed transaction events" "$(events t1)" "1:1:10>11 2:4:n>40 3:2:20>n 4:5:n>20 5:3:30>n 6:6:n>60"
expect "mixed transaction has one mark" "$(marks t1)" "6:4"
expect "mixed transaction provenance" "$(q "SELECT count(DISTINCT origin_xid)=1 AND min(command_id)>=0 FROM syntra.capture_events WHERE source_relation='t1'::regclass")" "t"

# 2. Rollback leaves nothing and no ordinal gap.
newtable t2 3 64
psqlc <<'SQL' >/dev/null
BEGIN;
UPDATE t2 SET v = 99;
ROLLBACK;
UPDATE t2 SET v = 11 WHERE id = 1;
SQL
expect "rollback then commit" "$(events t2)" "1:1:10>11"
expect "rollback then commit marks" "$(marks t2)" "1:3"

# 3. A rolled-back savepoint drops its changes.
newtable t3 3 64
psqlc <<'SQL' >/dev/null
BEGIN;
UPDATE t3 SET v = 11 WHERE id = 1;
SAVEPOINT s;
UPDATE t3 SET v = 22 WHERE id = 2;
ROLLBACK TO s;
UPDATE t3 SET v = 33 WHERE id = 3;
COMMIT;
SQL
expect "savepoint rollback events" "$(events t3)" "1:1:10>11 2:3:30>33"
expect "savepoint rollback marks" "$(marks t3)" "2:3"

# 4. Immediate firing mid-transaction is visible at once; the rest fires at commit.
newtable t4 3 64
SEEN="$(psqlc <<'SQL'
BEGIN;
SET CONSTRAINTS ALL IMMEDIATE;
UPDATE t4 SET v = 11 WHERE id = 1;
SELECT count(*) FROM syntra.capture_events WHERE source_relation = 't4'::regclass;
SET CONSTRAINTS ALL DEFERRED;
UPDATE t4 SET v = 22 WHERE id = 2;
UPDATE t4 SET v = 12 WHERE id = 1;
COMMIT;
SQL
)"
expect "immediate firing is visible mid-transaction" "$SEEN" "1"
expect "immediate then deferred events" "$(events t4)" "1:1:10>11 2:2:20>22 3:1:11>12"
expect "immediate then deferred marks" "$(marks t4)" "1:3 3:3"

# 5. A full log refuses the whole transaction with the existing message, and the
# same session writes normally afterwards: the failed commit's buffer is gone.
newtable t5 6 4
OUT="$(psqlc -v ON_ERROR_STOP=0 <<'SQL' 2>&1 || true
UPDATE t5 SET v = v + 1;
UPDATE t5 SET v = v + 1 WHERE id = 1;
SQL
)"
[[ "$OUT" == *"capture log is full or registration changed"* ]] || fail "full log message: $OUT"
expect "full log refuses then recovers in one session" "$(events t5)|$(q "SELECT sum(v) FROM t5")" "1:1:10>11|211"
expect "full log recovery mark" "$(marks t5)" "1:6"

# 6. Two sources in one transaction keep separate, gap-free logs.
newtable t6a 2 64
newtable t6b 2 64
psqlc <<'SQL' >/dev/null
BEGIN;
UPDATE t6a SET v = 11 WHERE id = 1;
UPDATE t6b SET v = 21 WHERE id = 1;
UPDATE t6a SET v = 12 WHERE id = 2;
COMMIT;
SQL
expect "two sources first" "$(events t6a)|$(marks t6a)" "1:1:10>11 2:2:20>12|2:2"
expect "two sources second" "$(events t6b)|$(marks t6b)" "1:1:10>21|1:2"

# 7. Prepared transactions keep their events with the transaction.
newtable t7 2 64
psqlc <<'SQL' >/dev/null
BEGIN;
UPDATE t7 SET v = 11 WHERE id = 1;
UPDATE t7 SET v = 12 WHERE id = 1;
PREPARE TRANSACTION 'batch_commit';
SQL
expect "prepared events are not visible yet" "$(events t7)" ""
q "COMMIT PREPARED 'batch_commit'" >/dev/null
expect "committed prepared events" "$(events t7)|$(marks t7)" "1:1:10>11 2:1:11>12|2:2"
psqlc <<'SQL' >/dev/null
BEGIN;
UPDATE t7 SET v = 50 WHERE id = 2;
PREPARE TRANSACTION 'batch_rollback';
SQL
q "ROLLBACK PREPARED 'batch_rollback'" >/dev/null
q "UPDATE t7 SET v = 13 WHERE id = 1" >/dev/null
expect "rolled back prepared leaves no gap" "$(events t7)" "1:1:10>11 2:1:11>12 3:1:12>13"

# 8. Concurrent paced writers; the drained feed must equal native SQL.
"$BIN/pg_ctl" -D "$DATA" -w -t 30 stop -m fast >/dev/null
printf "syntradb.maintenance_database = 'feeds'\nsyntradb.maintenance_interval_ms = 200\n" >>"$DATA/postgresql.conf"
"$BIN/pg_ctl" -D "$DATA" -l "$WORK/postgres.log" -w -t 60 start >/dev/null
q "CREATE TABLE feed(id int4 PRIMARY KEY,v int4); INSERT INTO feed SELECT g,1000 FROM generate_series(1,10000) AS g" >/dev/null
q "SELECT syntra_seed_capture('feed','id','v',4096)" >/dev/null
cat >"$WORK/feed.sql" <<'SQL'
\set a random(1, 10000)
\set b random(1, 10000)
\set temporary 10001 + :client_id
\set moved :temporary + 2
BEGIN;
UPDATE feed SET v = v - 1 WHERE id = :a;
UPDATE feed SET v = v + 1 WHERE id = :b;
INSERT INTO feed(id, v) VALUES (:temporary, NULL);
UPDATE feed SET id = :moved WHERE id = :temporary;
DELETE FROM feed WHERE id = :moved;
COMMIT;
SQL
RESULT="$("$BIN/pgbench" -h "$SOCKET" -p "$PORT" -U batch -n -c 4 -j 4 -R 100 -T 12 -f "$WORK/feed.sql" feeds 2>&1)"
echo "$RESULT" | grep -E "^(tps|number of failed)" || true
[[ "$RESULT" == *"number of failed transactions: 0"* ]] || fail "writers failed: $RESULT"
for _ in $(seq 1 150); do
    [[ "$(q "SELECT applied IS NOT NULL AND pending_events=0 FROM syntra_capture_lag('feed')")" == "t" ]] && break
    sleep 0.2
done
expect "load drains" "$(q "SELECT applied IS NOT NULL AND pending_events=0 FROM syntra_capture_lag('feed')")" "t"
expect "load summary equals native SQL" "$(q "SELECT s.route||':'||(ROW(s.rows,s.non_nulls,s.sum,s.min,s.max) IS NOT DISTINCT FROM ROW(n.rows,n.non_nulls,n.sum,n.min,n.max)) FROM syntra_checkpoint_int4_summary('feed','v',true) AS s,(SELECT count(*) AS rows,count(v) AS non_nulls,sum(v),min(v),max(v) FROM feed) AS n")" "checkpoint:true"
expect "load sum is conserved" "$(q "SELECT sum(v) FROM feed")" "10000000"
echo "capture batch check passed"
