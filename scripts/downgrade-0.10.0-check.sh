#!/usr/bin/env bash
# Prove that 0.10 temporal APIs can be removed without changing captured data or the
# 0.9 catalog, then added back with a catalog equal to a fresh workspace install.
set -euo pipefail
SYNTRA_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$SYNTRA_ROOT"
bash scripts/setup-production.sh
export PGRX_HOME="$SYNTRA_ROOT/.pgrx"
export PATH="$SYNTRA_ROOT/.tools/bin:$PATH"
CURRENT="$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)"
[[ "$CURRENT" == 0.10.0 || "$CURRENT" == 0.11.0 ]] || { echo "downgrade check requires workspace 0.10.0 or 0.11.0" >&2; exit 2; }
WORK="$SYNTRA_ROOT/target/downgrade-0.10.0-check/$(date +%Y%m%d-%H%M%S)-$$"
mkdir -p "$WORK/base"
tar xzf build-support/upgrade-baselines/syntradb-0.9.0-src.tgz -C "$WORK/base"
cp -R .tools/postgresql-production/18.6/install "$WORK/pg"
BIN="$WORK/pg/bin"
install_from() {
    (cd "$1/crates/syntra-pg" && CARGO_TARGET_DIR="$2" cargo pgrx install --release \
        --cargo=--locked --pg-config "$BIN/pg_config")
}
install_current() { install_from "$SYNTRA_ROOT" "$SYNTRA_ROOT/target/upgrade-current"; }
install_old() { install_from "$WORK/base" "$SYNTRA_ROOT/target/upgrade-baseline-0.9.0"; }
SOCKET="$(mktemp -d /tmp/syntra-downgrade10.XXXXXX)"
"$BIN/initdb" -D "$WORK/data" --username=downgrader --auth=trust --no-locale --encoding=UTF8 >/dev/null
printf "\nlisten_addresses = ''\nunix_socket_directories = '%s'\nport = 28957\n" "$SOCKET" >>"$WORK/data/postgresql.conf"
start() { "$BIN/pg_ctl" -D "$WORK/data" -l "$WORK/postgres.log" -w -t 30 start >/dev/null; }
stop() { "$BIN/pg_ctl" -D "$WORK/data" -w -t 30 stop -m fast >/dev/null; }
trap 'stop 2>/dev/null || true' EXIT
q() { "$BIN/psql" -h "$SOCKET" -p 28957 -U downgrader -d "$1" -v ON_ERROR_STOP=1 -AtqX -c "$2"; }
expect() {
    [[ "$2" == "$3" ]] || { echo "downgrade check failed: $1: got '$2', expected '$3'" >&2; exit 1; }
}
FINGERPRINT="$(cat scripts/lib/catalog-fingerprint.sql)"
READ="SELECT s.route||':'||(ROW(s.rows,s.non_nulls,s.sum,s.min,s.max) IS NOT DISTINCT FROM ROW(n.rows,n.non_nulls,n.sum,n.min,n.max))
    FROM syntra_checkpoint_int4_summary('feed','v',true) AS s,
    (SELECT count(*) AS rows,count(v) AS non_nulls,sum(v),min(v),max(v) FROM feed) AS n"
TIME_READ="SELECT ROW(s.rows,s.non_nulls,s.sum,s.min,s.max) IS NOT DISTINCT FROM ROW(n.rows,n.non_nulls,n.sum,n.min,n.max)
    FROM syntra_heap_date_summary('feed','v','t','2026-01-01','2026-01-02') AS s,
    (SELECT count(*) AS rows,count(v) AS non_nulls,sum(v)::numeric AS sum,min(v)::numeric AS min,max(v)::numeric AS max
     FROM feed WHERE t>=date '2026-01-01' AND t<date '2026-01-02') AS n LIMIT 1"

install_current
start
q postgres "CREATE DATABASE downgraded"
q downgraded "CREATE EXTENSION syntradb; CREATE TABLE feed(id int4 PRIMARY KEY,v int4,t date DEFAULT '2026-01-01'); INSERT INTO feed(id,v) SELECT g,g FROM generate_series(1,1000) AS g"
q downgraded "SELECT syntra_seed_capture('feed','id','v',4096)" >/dev/null
q downgraded "SELECT syntra_apply_capture('feed')" >/dev/null
q downgraded "UPDATE feed SET v=-v WHERE id<=20"
expect "temporal API before downgrade" "$(q downgraded "$TIME_READ")" "t"
q downgraded "CREATE VIEW user_temporal AS SELECT sum FROM syntra_heap_date_summary('feed','v','t','2026-01-01','2026-01-02')"
# unslop-ignore NO_SQL_INTERPOLATION -- SQL is constant; only the stderr file path varies.
if q downgraded "ALTER EXTENSION syntradb UPDATE TO '0.9.0'" >/dev/null 2>"$WORK/dependency.err"; then
    echo "downgrade check failed: a dependent user view was removed or ignored" >&2
    exit 1
fi
# unslop-ignore NO_SQL_INTERPOLATION -- CURRENT is only the expected result, not part of the constant SQL.
expect "failed downgrade is atomic" "$(q downgraded "SELECT extversion FROM pg_extension WHERE extname='syntradb'")" "$CURRENT"
expect "dependent view remains readable" "$(q downgraded "SELECT count(*)>0 FROM user_temporal")" "t"
q downgraded "DROP VIEW user_temporal; ALTER EXTENSION syntradb UPDATE TO '0.9.0'"
expect "temporal API removed" "$(q downgraded "SELECT to_regprocedure('public.syntra_heap_date_summary(regclass,text,text,date,date)') IS NULL")" "t"
expect "capture still readable" "$(q downgraded "$READ")" "replay:true"
stop

install_old
start
expect "old binary" "$(q downgraded "SELECT syntra_version()")" "0.9.0"
expect "old source fingerprint" "$(q downgraded "SELECT syntra_source_fingerprint()")" "1c4518ec479c2179c6dfea89bc60a2a9fbb8a76e1567519c83615be7b89d699f"
q postgres "CREATE DATABASE fresh_old"
q fresh_old "CREATE EXTENSION syntradb"
q downgraded "UPDATE feed SET v=v+1 WHERE id<=10"
q downgraded "SELECT syntra_apply_capture('feed')" >/dev/null
expect "old binary capture" "$(q downgraded "$READ")" "checkpoint:true"
q downgraded "$FINGERPRINT" >"$WORK/downgraded.catalog"
q fresh_old "$FINGERPRINT" >"$WORK/fresh_old.catalog"
diff -u "$WORK/fresh_old.catalog" "$WORK/downgraded.catalog" >"$WORK/old.diff" || { cat "$WORK/old.diff"; exit 1; }
stop

install_current
start
# unslop-ignore NO_SQL_INTERPOLATION -- CURRENT is checked against supported literal versions above; ALTER EXTENSION cannot bind its version.
q downgraded "ALTER EXTENSION syntradb UPDATE TO '$CURRENT'"
q postgres "CREATE DATABASE fresh_current"
q fresh_current "CREATE EXTENSION syntradb"
expect "temporal API restored" "$(q downgraded "$TIME_READ")" "t"
q downgraded "UPDATE feed SET v=NULL WHERE id=1"
q downgraded "SELECT syntra_apply_capture('feed')" >/dev/null
expect "capture after second upgrade" "$(q downgraded "$READ")" "checkpoint:true"
expect "temporal API after second upgrade" "$(q downgraded "$TIME_READ")" "t"
q downgraded "$FINGERPRINT" >"$WORK/upgraded.catalog"
q fresh_current "$FINGERPRINT" >"$WORK/fresh_current.catalog"
diff -u "$WORK/fresh_current.catalog" "$WORK/upgraded.catalog" >"$WORK/current.diff" || { cat "$WORK/current.diff"; exit 1; }
echo "downgrade check passed: $CURRENT -> 0.9.0 -> $CURRENT; dependent views block downgrade atomically, catalogs equal fresh installs, capture remains exact with both libraries, temporal API returns after upgrade; work dir $WORK"
