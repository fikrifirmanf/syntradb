#!/usr/bin/env bash
# Downgrade a live database from the workspace release (0.5.0) to 0.4.0 and back.
# 0.5.0 writers to a captured source leave its counter rows unchanged, and 0.4.0
# reuses a projection whose counters match, so the downgrade must remove every
# projection of a captured source. Three sources:
#   early   captured, published on 0.4.0 only, before the update;
#   late    captured, published again on 0.5.0;
#   plain   tracked only, published on 0.5.0.
# After captured writes on 0.5.0, the downgrade runs with the 0.5.0 library
# loaded; the 0.4.0 library is then installed and the server restarted. Checks
# that no captured projection is reused on 0.4.0, the tracked-only one is, the
# catalog matches a fresh 0.4.0 install apart from the kept column, and a second
# update to 0.5.0 matches a fresh 0.5.0 install. Builds into a private copy of
# the production PostgreSQL install; the original is untouched.
set -euo pipefail
SYNTRA_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$SYNTRA_ROOT"
bash scripts/setup-production.sh
export PGRX_HOME="$SYNTRA_ROOT/.pgrx"
export PATH="$SYNTRA_ROOT/.tools/bin:$PATH"
OLD=0.4.0
CURRENT="$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)"
if [[ "$CURRENT" != 0.5.0 ]]; then
    echo "downgrade check failed: this check covers 0.5.0 to 0.4.0, workspace is '$CURRENT'" >&2
    exit 1
fi
WORK="$SYNTRA_ROOT/target/downgrade-0.5.0-check/$(date +%Y%m%d-%H%M%S)-$$"
mkdir -p "$WORK/base"
cp -R .tools/postgresql-production/18.6/install "$WORK/pg"
PG_CONFIG="$WORK/pg/bin/pg_config"
BIN="$WORK/pg/bin"
tar xzf "build-support/upgrade-baselines/syntradb-$OLD-src.tgz" -C "$WORK/base"
install_from() {
    (cd "$1/crates/syntra-pg" && CARGO_TARGET_DIR="$2" cargo pgrx install --release \
        --cargo=--locked --pg-config "$PG_CONFIG")
}
install_old() { install_from "$WORK/base" "$SYNTRA_ROOT/target/upgrade-baseline-$OLD"; }
install_current() { install_from "$SYNTRA_ROOT" "$SYNTRA_ROOT/target/upgrade-current"; }

SOCKET="$(mktemp -d /tmp/syntra-downgrade5.XXXXXX)"
"$BIN/initdb" -D "$WORK/data" --username=downgrader --auth=trust --no-locale --encoding=UTF8 >/dev/null
printf "\nlisten_addresses = ''\nunix_socket_directories = '%s'\nport = 28936\n" "$SOCKET" \
    >>"$WORK/data/postgresql.conf"
start() { "$BIN/pg_ctl" -D "$WORK/data" -l "$WORK/postgres.log" -w -t 30 start >/dev/null; }
stop() { "$BIN/pg_ctl" -D "$WORK/data" -w -t 30 stop -m fast >/dev/null; }
trap 'stop 2>/dev/null || true' EXIT
q() { "$BIN/psql" -h "$SOCKET" -p 28936 -U downgrader -d "$1" -v ON_ERROR_STOP=1 -AtqX -c "$2"; }
expect() {
    if [[ "$2" != "$3" ]]; then
        echo "downgrade check failed: $1: got '$2', expected '$3'" >&2
        exit 1
    fi
}
# Route, reason and whether the sum matches the heap, for one fixed table name.
live() {
    # unslop-ignore NO_SQL_INTERPOLATION -- $1 is one of this script's fixed table names.
    q downgraded "SELECT s.route||':'||s.reason||':'||(s.sum IS NOT DISTINCT FROM n.sum)
        FROM syntra_live_int4_summary('$1','v') AS s,(SELECT sum(v) FROM $1) AS n"
}
publish() {
    # unslop-ignore NO_SQL_INTERPOLATION -- $1 is one of this script's fixed table names.
    q downgraded "SELECT syntra_publish_trusted_int4('$1','v')" >/dev/null
}
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
q postgres "CREATE DATABASE fresh_old"
q postgres "CREATE DATABASE fresh_current"
# unslop-ignore NO_SQL_INTERPOLATION -- OLD is a literal; CREATE EXTENSION takes no bind parameters.
q downgraded "CREATE EXTENSION syntradb VERSION '$OLD'"
# unslop-ignore NO_SQL_INTERPOLATION -- OLD is a literal; CREATE EXTENSION takes no bind parameters.
q fresh_old "CREATE EXTENSION syntradb VERSION '$OLD'"
for t in early late plain; do
    # unslop-ignore NO_SQL_INTERPOLATION -- t is one of this loop's fixed table names.
    q downgraded "CREATE TABLE $t(id int4 PRIMARY KEY,v int4); INSERT INTO $t SELECT g,g FROM generate_series(1,100) AS g"
done
q downgraded "SELECT syntra_seed_capture('early','id','v',64)" >/dev/null
q downgraded "SELECT syntra_seed_capture('late','id','v',64)" >/dev/null
q downgraded "SELECT syntra_track_source('plain','v')" >/dev/null
for t in early late plain; do publish "$t"; done
expect "early on $OLD" "$(live early)" "projection:eligible:true"
stop

install_current
start
# unslop-ignore NO_SQL_INTERPOLATION -- CURRENT is checked to be 0.5.0 above; ALTER EXTENSION takes no bind parameters.
q downgraded "ALTER EXTENSION syntradb UPDATE TO '$CURRENT'"
for t in early late plain; do
    # unslop-ignore NO_SQL_INTERPOLATION -- t is one of this loop's fixed table names.
    q downgraded "UPDATE $t SET v=v+1 WHERE id<=5"
done
publish late
publish plain
expect "late on $CURRENT" "$(live late)" "projection:eligible:true"
expect "plain on $CURRENT" "$(live plain)" "projection:eligible:true"
expect "early on $CURRENT" "$(live early)" "heap:version_changed:true"
expect "late stores a position" "$(q downgraded "SELECT capture_position IS NOT NULL FROM syntra.trusted_int4_projections WHERE source_relation='late'::regclass")" "t"
# Captured writes on 0.5.0 left early's counters where 0.4.0 published them.
expect "early counters unchanged" "$(q downgraded "SELECT p.generation_vector=t.generation_vector FROM syntra.trusted_int4_projections AS p, syntra_source_tracking_status('early') AS t WHERE p.source_relation='early'::regclass")" "t"

# unslop-ignore NO_SQL_INTERPOLATION -- OLD is a literal; ALTER EXTENSION takes no bind parameters.
q downgraded "ALTER EXTENSION syntradb UPDATE TO '$OLD'"
expect "version" "$(q downgraded "SELECT extversion FROM pg_extension WHERE extname='syntradb'")" "$OLD"
expect "kept projections" "$(q downgraded "SELECT string_agg(source_relation::regclass::text,',' ORDER BY 1) FROM syntra.trusted_int4_projections")" "plain"
# The 0.5.0 library is still loaded; it must not publish a captured projection now.
# unslop-ignore NO_SQL_INTERPOLATION -- constant SQL; only the stderr path is a variable.
if q downgraded "SELECT syntra_publish_trusted_int4('late','v')" >/dev/null 2>"$WORK/publish.err"; then
    echo "downgrade check failed: the 0.5.0 library published a captured projection after the downgrade" >&2
    exit 1
fi
grep -q "trusted_int4_projections_downgraded_check" "$WORK/publish.err"
stop

install_old
start
for t in early late; do
    expect "$t on $OLD after the downgrade" "$(live "$t")" "heap:no_projection:true"
done
expect "plain on $OLD after the downgrade" "$(live plain)" "projection:eligible:true"
# 0.4.0 counts captured writes on the rows again, so new projections stay correct.
publish early
expect "early republished on $OLD" "$(live early)" "projection:eligible:true"
q downgraded "UPDATE early SET v=v+1 WHERE id=1"
expect "early after a write on $OLD" "$(live early)" "heap:version_changed:true"
q downgraded "UPDATE plain SET v=v+1 WHERE id=1"
expect "plain after a write on $OLD" "$(live plain)" "heap:version_changed:true"
# History captured on 0.5.0 and 0.4.0 applies on 0.4.0 and reads back exactly.
q downgraded "SELECT applied FROM syntra_apply_prune_capture('early')" >/dev/null
expect "early checkpoint on $OLD" "$(q downgraded "SELECT s.route||':'||(s.sum IS NOT DISTINCT FROM n.sum) FROM syntra_checkpoint_int4_summary('early','v') AS s,(SELECT sum(v) FROM early) AS n")" "checkpoint:true"
q downgraded "$FINGERPRINT" | grep -v -e '^col trusted_int4_projections.capture_position ' \
    -e '^con syntra.trusted_int4_projections trusted_int4_projections_capture_position_check ' \
    -e '^con syntra.trusted_int4_projections trusted_int4_projections_downgraded_check ' >"$WORK/downgraded.catalog"
q fresh_old "$FINGERPRINT" >"$WORK/fresh_old.catalog"
if ! diff -u "$WORK/fresh_old.catalog" "$WORK/downgraded.catalog" >"$WORK/old.diff"; then
    cat "$WORK/old.diff" >&2
    echo "downgrade check failed: downgraded catalog differs from a fresh $OLD install" >&2
    exit 1
fi
stop

install_current
start
# unslop-ignore NO_SQL_INTERPOLATION -- CURRENT is checked to be 0.5.0 above; ALTER EXTENSION takes no bind parameters.
q downgraded "ALTER EXTENSION syntradb UPDATE TO '$CURRENT'"
q fresh_current "CREATE EXTENSION syntradb"
q downgraded "$FINGERPRINT" >"$WORK/upgraded.catalog"
q fresh_current "$FINGERPRINT" >"$WORK/fresh_current.catalog"
if ! diff -u "$WORK/fresh_current.catalog" "$WORK/upgraded.catalog" >"$WORK/current.diff"; then
    cat "$WORK/current.diff" >&2
    echo "downgrade check failed: catalog after the second update differs from a fresh $CURRENT install" >&2
    exit 1
fi
expect "early after the second update" "$(live early)" "heap:version_changed:true"
publish early
expect "early republished on $CURRENT" "$(live early)" "projection:eligible:true"
q downgraded "UPDATE early SET v=v+1 WHERE id=2"
expect "early after a captured write on $CURRENT" "$(live early)" "heap:version_changed:true"
echo "downgrade check passed: $CURRENT -> $OLD -> $CURRENT; no captured projection reused after the downgrade, $(wc -l <"$WORK/fresh_old.catalog" | tr -d ' ') catalog entries identical to a fresh $OLD install apart from the kept column, $(wc -l <"$WORK/fresh_current.catalog" | tr -d ' ') to a fresh $CURRENT install after the second update; work dir $WORK"
