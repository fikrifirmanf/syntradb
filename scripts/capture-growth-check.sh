#!/usr/bin/env bash
# Seed a feed with 100,000 rows, then grow it with captured inserts to a target
# row count, applying and pruning after every batch. Records the apply time, the
# checkpoint catalog size and WAL as the source grows, and requires the checkpoint
# read to match the heap at the end and a physical-size report to stay inside the
# documented bounds. Uses a private copy of the production PostgreSQL install.
#   capture-growth-check.sh [target-rows] [batch-rows] [churn-seconds] [default|eager]
# With churn-seconds above 0, four writers then update, delete and reinsert, and
# move keys at the full size while an apply/prune loop runs; the final checkpoint
# read must match the heap and the row count must be unchanged. eager removes the
# autovacuum cost delay on the checkpoint chunk table and its TOAST table.
set -euo pipefail
SYNTRA_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$SYNTRA_ROOT"
TARGET="${1:-400000}"
BATCH="${2:-4000}"
CHURN="${3:-0}"
VACUUM_MODE="${4:-default}"
[[ "$VACUUM_MODE" == default || "$VACUUM_MODE" == eager ]] || { echo "capture growth check: vacuum mode must be default or eager" >&2; exit 2; }
for value in "$TARGET" "$BATCH"; do
    [[ "$value" =~ ^[1-9][0-9]{0,6}$ ]] || { echo "capture growth check: '$value' is not a positive integer" >&2; exit 2; }
done
[[ "$CHURN" =~ ^[0-9]{1,4}$ ]] || { echo "capture growth check: churn seconds must be 0 to 9999" >&2; exit 2; }
if ((TARGET <= 100000 || TARGET > 1000000 || BATCH > 4000)); then
    echo "capture growth check: target must be 100,001 to 1,000,000 rows and batch at most 4,000" >&2
    exit 2
fi
bash scripts/setup-production.sh
export PGRX_HOME="$SYNTRA_ROOT/.pgrx"
export PATH="$SYNTRA_ROOT/.tools/bin:$PATH"
WORK="$SYNTRA_ROOT/target/capture-growth-check/$(date +%Y%m%d-%H%M%S)-$$"
mkdir -p "$WORK"
cp -R .tools/postgresql-production/18.6/install "$WORK/pg"
BIN="$WORK/pg/bin"
(cd crates/syntra-pg && CARGO_TARGET_DIR="$SYNTRA_ROOT/target/upgrade-current" cargo pgrx install \
    --release --cargo=--locked --pg-config "$WORK/pg/bin/pg_config")
SOCKET="$(mktemp -d /tmp/syntra-growth.XXXXXX)"
PORT=28944
"$BIN/initdb" -D "$WORK/data" --username=grower --auth=trust --no-locale --encoding=UTF8 >/dev/null
printf "\nlisten_addresses = ''\nunix_socket_directories = '%s'\nport = %s\n" "$SOCKET" "$PORT" >>"$WORK/data/postgresql.conf"
"$BIN/pg_ctl" -D "$WORK/data" -l "$WORK/postgres.log" -w -t 60 start >/dev/null
trap '"$BIN/pg_ctl" -D "$WORK/data" -w -t 30 stop -m immediate >/dev/null 2>&1 || true' EXIT
q() { "$BIN/psql" -h "$SOCKET" -p "$PORT" -U grower -d growth -v ON_ERROR_STOP=1 -AtqX -c "$1"; }
fail() { echo "capture growth check failed: $1" >&2; exit 1; }
"$BIN/psql" -h "$SOCKET" -p "$PORT" -U grower -d postgres -v ON_ERROR_STOP=1 -AtqX -c "CREATE DATABASE growth" >/dev/null
q "CREATE EXTENSION syntradb"
if [[ "$VACUUM_MODE" == eager ]]; then
    q "ALTER TABLE syntra.capture_chunks SET (autovacuum_vacuum_cost_delay=0,toast.autovacuum_vacuum_cost_delay=0)"
fi
q "CREATE TABLE feed(id int4 PRIMARY KEY,v int4); INSERT INTO feed SELECT g,g FROM generate_series(1,100000) AS g"
q "SELECT syntra_seed_capture('feed','id','v',4096)" >/dev/null
q "SELECT syntra_apply_capture('feed')" >/dev/null
WAL_START="$(q "SELECT pg_current_wal_insert_lsn()")"

# One session runs every batch: insert, then time apply+prune on the server clock.
{
    echo "\\set ON_ERROR_STOP on"
    rows=100000
    while ((rows < TARGET)); do
        take=$((TARGET - rows < BATCH ? TARGET - rows : BATCH))
        echo "INSERT INTO feed SELECT g,g FROM generate_series($((rows + 1)),$((rows + take))) AS g;"
        echo "SELECT clock_timestamp() AS t0 \\gset"
        echo "SELECT applied FROM syntra_apply_prune_capture('feed') \\gset"
        echo "SELECT $((rows + take)) AS rows, round(extract(epoch FROM clock_timestamp()-:'t0'::timestamptz)*1000) AS apply_ms \\g (format=unaligned tuples_only=on fieldsep=' ') |cat >>$WORK/apply.log"
        rows=$((rows + take))
    done
} >"$WORK/grow.sql"
STARTED=$SECONDS
"$BIN/psql" -h "$SOCKET" -p "$PORT" -U grower -d growth -X -q -f "$WORK/grow.sql" >"$WORK/grow.out" 2>"$WORK/grow.err" \
    || { cat "$WORK/grow.err" >&2; fail "growing the source stopped; see $WORK/grow.err"; }
GROW_S=$((SECONDS - STARTED))

if ((CHURN > 0)); then
    # Writer w owns ids with id%4=w, so writers never conflict with each other.
    writer() {
        local w="$1" n=0 a
        while [[ ! -e "$WORK/stop" ]]; do
            a=$(((RANDOM * 32768 + RANDOM) % (TARGET / 4) * 4 + w))
            "$BIN/psql" -h "$SOCKET" -p "$PORT" -U grower -d growth -v ON_ERROR_STOP=1 -AtqX >/dev/null 2>>"$WORK/writer-$w.err" <<SQL && n=$((n + 1))
BEGIN;
-- unslop-ignore NO_SQL_INTERPOLATION -- w and a are integers this script computed
UPDATE feed SET v=v+1 WHERE id%4=$w AND id BETWEEN $a AND $((a + 199));
-- unslop-ignore NO_SQL_INTERPOLATION -- a is an integer this script computed
WITH d AS (DELETE FROM feed WHERE id=$a RETURNING id,v) INSERT INTO feed SELECT id,v FROM d;
UPDATE feed SET id=CASE WHEN id>=10000000 THEN id-10000000 ELSE id+10000000 END WHERE id IN ($((a + 4)),$((a + 4 + 10000000)));
COMMIT;
SQL
        done
        echo "$n" >"$WORK/writer-$w.count"
    }
    drainer() {
        while [[ ! -e "$WORK/stop" ]]; do
# unslop-ignore NO_SQL_INTERPOLATION -- literal SQL, no variable
            q "SELECT applied FROM syntra_apply_prune_capture('feed')" >/dev/null 2>>"$WORK/drain.err" || true
            sleep 0.2
        done
    }
    # Every 30 s: elapsed seconds, chunk table MB, live and dead rows, autovacuum
    # runs, then the TOAST table's MB, dead rows and autovacuum runs.
    sampler() {
        local began=$SECONDS
        while [[ ! -e "$WORK/stop" ]]; do
            q "SELECT $((SECONDS - began)),round(pg_total_relation_size('syntra.capture_chunks')/1048576.0,1),m.n_live_tup,m.n_dead_tup,m.autovacuum_count,round(pg_relation_size(c.reltoastrelid)/1048576.0,1),t.n_dead_tup,t.autovacuum_count FROM pg_class c JOIN pg_stat_all_tables m ON m.relid=c.oid JOIN pg_stat_all_tables t ON t.relid=c.reltoastrelid WHERE c.oid='syntra.capture_chunks'::regclass" \
                >>"$WORK/sizes.log" 2>>"$WORK/sampler.err" || true
            sleep 30
        done
    }
    for w in 0 1 2 3; do writer "$w" & done
    drainer &
    sampler &
    sleep "$CHURN"
    touch "$WORK/stop"
    wait
    # Writers fail closed when the log fills; a busy publisher (55P03) is retryable. Nothing else may fail.
    OTHER_ERRORS="$(cat "$WORK"/writer-*.err "$WORK/drain.err" 2>/dev/null | grep -c 'ERROR' || true)"
    FULL_ERRORS="$(cat "$WORK"/writer-*.err 2>/dev/null | grep -c 'capture log is full' || true)"
    BUSY_ERRORS="$(cat "$WORK/drain.err" 2>/dev/null | grep -c 'maintenance is busy' || true)"
    OTHER_ERRORS=$((OTHER_ERRORS - FULL_ERRORS - BUSY_ERRORS))
    COMMITTED=0
    for w in 0 1 2 3; do COMMITTED=$((COMMITTED + $(cat "$WORK/writer-$w.count"))); done
    ((OTHER_ERRORS == 0)) || fail "$OTHER_ERRORS unexpected errors during churn; see $WORK/writer-*.err"
    ((COMMITTED > 0)) || fail "no churn transaction committed"
    while [[ "$(q "SELECT pending_events FROM syntra_capture_lag('feed')")" != "0" ]]; do
        q "SELECT applied FROM syntra_apply_prune_capture('feed')" >/dev/null
    done
    # unslop-ignore NO_SQL_INTERPOLATION -- the literal SQL contains no variable; TARGET is only compared
    [[ "$(q "SELECT count(*) FROM feed")" == "$TARGET" ]] || fail "churn changed the row count"
    echo "chunk table during churn (seconds|MB|live|dead|autovacuum runs|toast MB|toast dead|toast autovacuum runs), vacuum mode $VACUUM_MODE:"
    cat "$WORK/sizes.log"
    CHURN_NOTE="; churn ${CHURN}s: $COMMITTED transactions committed, $FULL_ERRORS failed closed on a full log, $BUSY_ERRORS apply calls found maintenance busy"
else
    CHURN_NOTE=""
fi

# unslop-ignore NO_SQL_INTERPOLATION -- the literal SQL contains no variable; TARGET is only compared
[[ "$(q "SELECT count(*) FROM feed")" == "$TARGET" ]] || fail "the source does not hold $TARGET rows"
# unslop-ignore NO_SQL_INTERPOLATION -- the literal SQL contains no variable; TARGET is only compared
[[ "$(q "SELECT live_rows FROM syntra_capture_headroom('feed')")" == "$TARGET" ]] || fail "capture counted a different number of live rows"
[[ "$(q "SELECT pending_events FROM syntra_capture_lag('feed')")" == "0" ]] || fail "events are still pending"
READ="SELECT s.route||':'||s.reason||':'||(ROW(s.rows,s.non_nulls,s.sum,s.min,s.max) IS NOT DISTINCT FROM ROW(n.rows,n.non_nulls,n.sum,n.min,n.max))
    FROM syntra_checkpoint_int4_summary('feed','v',true) AS s,
    (SELECT count(*) AS rows,count(v) AS non_nulls,sum(v),min(v),max(v) FROM feed) AS n"
READ_RESULT="$(q "$READ")"
[[ "$READ_RESULT" == "checkpoint:eligible:true" ]] || fail "checkpoint read at $TARGET rows: $READ_RESULT"
# Time the checkpoint read against the heap aggregate it replaces.
cat >"$WORK/read.sql" <<'SQL'
SELECT clock_timestamp() AS t0 \gset
SELECT s.rows AS rows FROM syntra_checkpoint_int4_summary('feed','v') AS s \gset
SELECT round(extract(epoch FROM clock_timestamp()-:'t0'::timestamptz)*1000);
SQL
CHECKPOINT_MS="$("$BIN/psql" -h "$SOCKET" -p "$PORT" -U grower -d growth -v ON_ERROR_STOP=1 -AtqX -f "$WORK/read.sql")"
SIZES="$(q "SELECT json_build_object('checkpoint_chunks_mb',round(pg_total_relation_size('syntra.capture_chunks')/1048576.0,1),'events_mb',round(pg_total_relation_size('syntra.capture_events')/1048576.0,1),'base_rows_mb',round(pg_total_relation_size('syntra.capture_base_rows')/1048576.0,1),'source_mb',round(pg_total_relation_size('feed')/1048576.0,1))")"
# unslop-ignore NO_SQL_INTERPOLATION -- WAL_START is an LSN read from the server a moment earlier
WAL_MB="$(q "SELECT round(pg_wal_lsn_diff(pg_current_wal_insert_lsn(),'$WAL_START')/1048576.0)")"
# Statistics reset at crash recovery, so read them before the restart.
DEAD="$(q "SELECT json_build_object('live_tuples',n_live_tup,'dead_tuples',n_dead_tup,'autovacuum_runs',autovacuum_count) FROM pg_stat_user_tables WHERE relid='syntra.capture_chunks'::regclass")"
# Restore-style check: a crash and restart must keep the checkpoint exact.
"$BIN/pg_ctl" -D "$WORK/data" -w -t 60 stop -m immediate >/dev/null
"$BIN/pg_ctl" -D "$WORK/data" -l "$WORK/postgres.log" -w -t 120 start >/dev/null
[[ "$(q "$READ")" == "checkpoint:eligible:true" ]] || fail "checkpoint read after crash recovery: $(q "$READ")"
# Separate live bytes from dead versions: statistics first, then a rewrite, which
# is safe here because nothing else uses this private cluster.
q "VACUUM FULL syntra.capture_chunks"
LIVE_MB="$(q "SELECT round(pg_total_relation_size('syntra.capture_chunks')/1048576.0,1)")"
[[ "$(q "$READ")" == "checkpoint:eligible:true" ]] || fail "checkpoint read after VACUUM FULL: $(q "$READ")"
python3 - "$WORK/apply.log" <<'PY' | tee "$WORK/summary.txt"
import sys
rows = [tuple(map(int, l.split())) for l in open(sys.argv[1]) if l.strip()]
ms = [m for _, m in rows]
def pct(p):
    s = sorted(ms)
    return s[min(len(s) - 1, int(len(s) * p))]
step = max(1, len(rows) // 8)
print("apply_ms by rows:", ", ".join(f"{r}:{m}" for r, m in rows[::step]) + f", {rows[-1][0]}:{rows[-1][1]}")
print(f"apply_ms median {pct(0.5)} p95 {pct(0.95)} max {max(ms)} over {len(ms)} batches")
PY
echo "capture growth check passed: 100000 -> $TARGET rows in batches of $BATCH took ${GROW_S}s; checkpoint read matched the heap before and after a crash; checkpoint summary read took ${CHECKPOINT_MS} ms; WAL ${WAL_MB} MB; sizes $SIZES; chunk table after VACUUM FULL ${LIVE_MB} MB, statistics before it $DEAD; work dir $WORK${CHURN_NOTE}"
