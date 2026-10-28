#!/usr/bin/env bash
# Upgrade a 0.1.0, 0.2.0 or 0.3.0 database holding several large captured sources
# with pending history, time ALTER EXTENSION UPDATE, and check that every source
# drains to a checkpoint that matches its heap. From 0.1.0 or 0.2.0, capture writes
# fail closed or wait from the library install until the update commits, so the
# update's duration is the write outage after the restart. The 0.3.0 step rewrites
# capture_events and capture_base_rows to bigint, so it grows with retained
# history. The 0.4.0 step adds a column and blocks only maintenance. Uses a
# private copy of the production PostgreSQL install.
#   upgrade-scale-check.sh [sources] [rows] [pending] [from-version]
set -euo pipefail
SYNTRA_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$SYNTRA_ROOT"
SOURCES="${1:-8}"
ROWS="${2:-100000}"
PENDING="${3:-3000}"
FROM="${4:-0.1.0}"
[[ "$FROM" =~ ^0\.[123]\.0$ ]] || { echo "upgrade scale check: from-version must be 0.1.0, 0.2.0 or 0.3.0" >&2; exit 2; }
# These values are interpolated into SQL below, so accept only bounded integers.
for value in "$SOURCES" "$ROWS" "$PENDING"; do
    [[ "$value" =~ ^[1-9][0-9]{0,5}$ ]] || { echo "upgrade scale check: '$value' is not a positive integer" >&2; exit 2; }
done
if ((SOURCES > 16 || ROWS > 100000 || PENDING > 4096 || PENDING > ROWS)); then
    echo "upgrade scale check: at most 16 sources, 100,000 rows and 4,096 pending events, and pending <= rows" >&2
    exit 2
fi
bash scripts/setup-production.sh
export PGRX_HOME="$SYNTRA_ROOT/.pgrx"
export PATH="$SYNTRA_ROOT/.tools/bin:$PATH"
CURRENT="$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)"
if [[ ! "$CURRENT" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "upgrade scale check failed: workspace version '$CURRENT' is not x.y.z" >&2
    exit 1
fi
WORK="$SYNTRA_ROOT/target/upgrade-scale-check/$(date +%Y%m%d-%H%M%S)-$$"
mkdir -p "$WORK/base"
cp -R .tools/postgresql-production/18.6/install "$WORK/pg"
PG_CONFIG="$WORK/pg/bin/pg_config"
BIN="$WORK/pg/bin"
tar xzf build-support/upgrade-baselines/syntradb-$FROM-src.tgz -C "$WORK/base"
install_from() {
    (cd "$1/crates/syntra-pg" && CARGO_TARGET_DIR="$2" cargo pgrx install --release \
        --cargo=--locked --pg-config "$PG_CONFIG")
}
install_from "$WORK/base" "$SYNTRA_ROOT/target/upgrade-baseline-$FROM"

SOCKET="$(mktemp -d /tmp/syntra-upscale.XXXXXX)"
"$BIN/initdb" -D "$WORK/data" --username=upgrader --auth=trust --no-locale --encoding=UTF8 >/dev/null
printf "\nlisten_addresses = ''\nunix_socket_directories = '%s'\nport = 28937\n" "$SOCKET" \
    >>"$WORK/data/postgresql.conf"
start() { "$BIN/pg_ctl" -D "$WORK/data" -l "$WORK/postgres.log" -w -t 30 start >/dev/null; }
stop() { "$BIN/pg_ctl" -D "$WORK/data" -w -t 30 stop -m fast >/dev/null; }
trap 'stop 2>/dev/null || true' EXIT
q() { "$BIN/psql" -h "$SOCKET" -p 28937 -U upgrader -d upgraded -v ON_ERROR_STOP=1 -AtqX -c "$1"; }
fail() {
    echo "upgrade scale check failed: $1" >&2
    exit 1
}
read_route() {
    q "SELECT s.route||':'||(ROW(s.rows,s.non_nulls,s.sum,s.min,s.max) IS NOT DISTINCT FROM ROW(n.rows,n.non_nulls,n.sum,n.min,n.max))
        FROM syntra_checkpoint_int4_summary('feed_$1','v',true) AS s,
        (SELECT count(*) AS rows,count(v) AS non_nulls,sum(v),min(v),max(v) FROM feed_$1) AS n"
}

start
"$BIN/psql" -h "$SOCKET" -p 28937 -U upgrader -d postgres -v ON_ERROR_STOP=1 -AtqX -c "CREATE DATABASE upgraded" >/dev/null
# unslop-ignore NO_SQL_INTERPOLATION -- FROM is one of three literal versions, checked above.
q "CREATE EXTENSION syntradb VERSION '$FROM'"
for s in $(seq 1 "$SOURCES"); do
    # unslop-ignore NO_SQL_INTERPOLATION -- only integers validated above, the loop index and CURRENT (checked as x.y.z).
    q "CREATE TABLE feed_$s(id int4 PRIMARY KEY,v int4); INSERT INTO feed_$s SELECT g,g FROM generate_series(1,$ROWS) AS g"
    # unslop-ignore NO_SQL_INTERPOLATION -- only integers validated above, the loop index and CURRENT (checked as x.y.z).
    q "SELECT syntra_seed_capture('feed_$s','id','v',4096)" >/dev/null
    # unslop-ignore NO_SQL_INTERPOLATION -- only integers validated above, the loop index and CURRENT (checked as x.y.z).
    q "SELECT syntra_apply_capture('feed_$s')" >/dev/null
    # unslop-ignore NO_SQL_INTERPOLATION -- only integers validated above, the loop index and CURRENT (checked as x.y.z).
    q "UPDATE feed_$s SET v=-v WHERE id<=$PENDING"
done
# unslop-ignore NO_SQL_INTERPOLATION -- only integers validated above, the loop index and CURRENT (checked as x.y.z).
[[ "$(q "SELECT count(*)=$SOURCES AND bool_and(n=$PENDING) FROM (SELECT count(*) AS n FROM syntra.capture_events GROUP BY source_relation) AS e")" == "t" ]] \
    || fail "$FROM pending events"
stop

install_from "$SYNTRA_ROOT" "$SYNTRA_ROOT/target/upgrade-current"
start
# The update runs in its own transaction; the server clock times it.
cat >"$WORK/update.sql" <<SQL
SELECT clock_timestamp() AS started \\gset
-- unslop-ignore NO_SQL_INTERPOLATION -- only integers validated above, the loop index and CURRENT (checked as x.y.z).
ALTER EXTENSION syntradb UPDATE TO '$CURRENT';
SELECT round(extract(epoch FROM clock_timestamp()-:'started'::timestamptz)*1000);
SQL
UPDATE_MS="$("$BIN/psql" -h "$SOCKET" -p 28937 -U upgrader -d upgraded -v ON_ERROR_STOP=1 -AtqX -f "$WORK/update.sql")"
# unslop-ignore NO_SQL_INTERPOLATION -- only integers validated above, the loop index and CURRENT (checked as x.y.z).
[[ "$(q "SELECT extversion FROM pg_extension WHERE extname='syntradb'")" == "$CURRENT" ]] || fail "version"
for s in $(seq 1 "$SOURCES"); do
    # unslop-ignore NO_SQL_INTERPOLATION -- only integers validated above, the loop index and CURRENT (checked as x.y.z).
    [[ "$(q "SELECT pending_events FROM syntra_capture_lag('feed_$s')")" == "$PENDING" ]] || fail "feed_$s lag after the update"
    # unslop-ignore NO_SQL_INTERPOLATION -- only integers validated above, the loop index and CURRENT (checked as x.y.z).
    q "UPDATE feed_$s SET v=v+1 WHERE id BETWEEN $((ROWS - 9)) AND $ROWS"
    # unslop-ignore NO_SQL_INTERPOLATION -- only integers validated above, the loop index and CURRENT (checked as x.y.z).
    q "SELECT applied FROM syntra_apply_prune_capture('feed_$s')" >/dev/null
    [[ "$(read_route "$s")" == "checkpoint:true" ]] || fail "feed_$s drained read: $(read_route "$s")"
done
# unslop-ignore NO_SQL_INTERPOLATION -- only integers validated above, the loop index and CURRENT (checked as x.y.z).
echo "upgrade scale check passed: $FROM -> $CURRENT, $SOURCES sources x $ROWS rows with $PENDING pending events each, ALTER EXTENSION UPDATE took $UPDATE_MS ms; work dir $WORK"
