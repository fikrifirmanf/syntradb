#!/usr/bin/env bash
# Swap both libraries around the catalog-only 0.9/0.8 lifecycle. Verify SSI
# behavior, capture replay and parity with fresh catalogs at each version.
set -euo pipefail
SYNTRA_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$SYNTRA_ROOT"
bash scripts/setup-production.sh
export PGRX_HOME="$SYNTRA_ROOT/.pgrx"
export PATH="$SYNTRA_ROOT/.tools/bin:$PATH"
CURRENT="$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)"
[[ "$CURRENT" == 0.9.0 || "$CURRENT" == 0.10.0 || "$CURRENT" == 0.11.0 ]] || { echo "downgrade check requires workspace 0.9.0 or 0.10.0 or 0.11.0" >&2; exit 2; }
WORK="$SYNTRA_ROOT/target/downgrade-0.9.0-check/$(date +%Y%m%d-%H%M%S)-$$"
mkdir -p "$WORK/base"
tar xzf build-support/upgrade-baselines/syntradb-0.8.0-src.tgz -C "$WORK/base"
cp -R .tools/postgresql-production/18.6/install "$WORK/pg"
BIN="$WORK/pg/bin"
install_from() {
    (cd "$1/crates/syntra-pg" && CARGO_TARGET_DIR="$2" cargo pgrx install --release \
        --cargo=--locked --pg-config "$BIN/pg_config")
}
install_current() { install_from "$SYNTRA_ROOT" "$SYNTRA_ROOT/target/upgrade-current"; }
install_old() { install_from "$WORK/base" "$SYNTRA_ROOT/target/upgrade-baseline-0.8.0"; }
SOCKET="$(mktemp -d /tmp/syntra-downgrade9.XXXXXX)"
"$BIN/initdb" -D "$WORK/data" --username=downgrader --auth=trust --no-locale --encoding=UTF8 >/dev/null
printf "\nlisten_addresses = ''\nunix_socket_directories = '%s'\nport = 28956\n" "$SOCKET" >>"$WORK/data/postgresql.conf"
start() { "$BIN/pg_ctl" -D "$WORK/data" -l "$WORK/postgres.log" -w -t 30 start >/dev/null; }
stop() { "$BIN/pg_ctl" -D "$WORK/data" -w -t 30 stop -m fast >/dev/null; }
trap 'stop 2>/dev/null || true' EXIT
q() { "$BIN/psql" -h "$SOCKET" -p 28956 -U downgrader -d "$1" -v ON_ERROR_STOP=1 -AtqX -c "$2"; }
expect() {
    [[ "$2" == "$3" ]] || { echo "downgrade check failed: $1: got '$2', expected '$3'" >&2; exit 1; }
}
FINGERPRINT="$(cat scripts/lib/catalog-fingerprint.sql)"
READ="SELECT s.route||':'||(ROW(s.rows,s.non_nulls,s.sum,s.min,s.max) IS NOT DISTINCT FROM ROW(n.rows,n.non_nulls,n.sum,n.min,n.max))
    FROM syntra_checkpoint_int4_summary('feed','v',true) AS s,
    (SELECT count(*) AS rows,count(v) AS non_nulls,sum(v),min(v),max(v) FROM feed) AS n"
SSI="BEGIN ISOLATION LEVEL SERIALIZABLE; SELECT s.rows||':'||s.sum||':'||g.sum
    FROM syntra_heap_int4_summary('feed','v') AS s,
    (SELECT sum(sum) AS sum FROM syntra_heap_group_integer_summary('feed','v','v',2048)) AS g; COMMIT"

install_current
start
q postgres "CREATE DATABASE migrated"
q migrated "CREATE EXTENSION syntradb; CREATE TABLE feed(id int4 PRIMARY KEY,v int4); INSERT INTO feed SELECT g,g FROM generate_series(1,1000) AS g"
q migrated "SELECT syntra_seed_capture('feed','id','v',4096)" >/dev/null
q migrated "SELECT syntra_apply_capture('feed')" >/dev/null
q migrated "UPDATE feed SET v=-v WHERE id<=20"
expect "SSI before downgrade" "$(q migrated "$SSI")" "1000:500080:500080"
q migrated "ALTER EXTENSION syntradb UPDATE TO '0.8.0'"
expect "downgraded catalog version" "$(q migrated "SELECT extversion FROM pg_extension WHERE extname='syntradb'")" "0.8.0"
expect "pending capture after catalog downgrade" "$(q migrated "$READ")" "replay:true"
stop

install_old
start
expect "old binary" "$(q migrated "SELECT syntra_version()")" "0.8.0"
expect "archived source fingerprint" "$(q migrated "SELECT syntra_source_fingerprint()")" "d708aff5ce9311f513eb2e0a0d1502801d386555770cfed91e0eefca452ef72f"
REJECT_OLD_SSI="$(cat <<'SQL'
BEGIN ISOLATION LEVEL SERIALIZABLE;
DO $check$ DECLARE rejected integer := 0; BEGIN
    BEGIN PERFORM rows FROM syntra_heap_int4_summary('feed','v');
    EXCEPTION WHEN SQLSTATE '0A000' THEN rejected := rejected+1; END;
    BEGIN PERFORM group_key FROM syntra_heap_group_integer_summary('feed','v','v',2048);
    EXCEPTION WHEN SQLSTATE '0A000' THEN rejected := rejected+1; END;
    IF rejected <> 2 THEN RAISE EXCEPTION 'old library unexpectedly accepted fresh SSI'; END IF;
END $check$; COMMIT
SQL
)"
q migrated "$REJECT_OLD_SSI"
q postgres "CREATE DATABASE fresh_old"
q fresh_old "CREATE EXTENSION syntradb"
q migrated "$FINGERPRINT" >"$WORK/downgraded.catalog"
q fresh_old "$FINGERPRINT" >"$WORK/fresh_old.catalog"
diff -u "$WORK/fresh_old.catalog" "$WORK/downgraded.catalog" >"$WORK/old.diff" || { cat "$WORK/old.diff"; exit 1; }
q migrated "UPDATE feed SET v=v+1 WHERE id<=10"
q migrated "SELECT syntra_apply_capture('feed')" >/dev/null
expect "old binary pending history and writes" "$(q migrated "$READ")" "checkpoint:true"
stop

install_current
start
expect "SSI after new library restart" "$(q migrated "$SSI")" "1000:500090:500090"
# unslop-ignore NO_SQL_INTERPOLATION -- CURRENT is restricted to literal 0.9.0 or 0.10.0 or 0.11.0 above; ALTER EXTENSION cannot bind its version.
q migrated "ALTER EXTENSION syntradb UPDATE TO '$CURRENT'"
q postgres "CREATE DATABASE fresh_current"
q fresh_current "CREATE EXTENSION syntradb"
q migrated "UPDATE feed SET v=NULL WHERE id=1"
q migrated "SELECT syntra_apply_capture('feed')" >/dev/null
expect "capture after second upgrade" "$(q migrated "$READ")" "checkpoint:true"
expect "SSI after second upgrade" "$(q migrated "$SSI")" "1000:500090:500090"
q migrated "$FINGERPRINT" >"$WORK/upgraded.catalog"
q fresh_current "$FINGERPRINT" >"$WORK/fresh_current.catalog"
diff -u "$WORK/fresh_current.catalog" "$WORK/upgraded.catalog" >"$WORK/current.diff" || { cat "$WORK/current.diff"; exit 1; }
echo "downgrade check passed: 0.9.0 -> 0.8.0 -> 0.9.0; fresh SSI support follows the loaded library, capture stays exact, catalogs match fresh installs, archived source fingerprint is unchanged; work dir $WORK"
