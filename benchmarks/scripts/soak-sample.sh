#!/usr/bin/env bash
# Takes macOS `sample` stack profiles of named checkpoint soak backends while a
# soak runs. Usage: soak-sample.sh OUT_DIR "MINUTE ..." [SECONDS]
# Each listed minute after the soak's workers start, every backend whose
# application_name is pruner, writer_0 or applier_0 is sampled for SECONDS.
set -euo pipefail
out=$1
minutes=$2
seconds=${3:-10}
psql="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)/.tools/postgresql-production/18.6/install/bin/psql"
test ! -e "$out"
mkdir -p "$out"
socket=
while [ -z "$socket" ]; do
    for dir in /tmp/syntra-bench-*; do
        if [ -S "$dir/.s.PGSQL.28918" ]; then socket=$dir; fi
    done
    [ -n "$socket" ] || sleep 5
done
query() {
    "$psql" -h "$socket" -p 28918 -U syntra_bench -d syntra_enabled -AtqX -c "$1"
}
until [ "$(query "SELECT count(*) FROM pg_stat_activity WHERE application_name='pruner'" 2>/dev/null || echo 0)" = 1 ]; do
    sleep 5
done
start=$(date +%s)
echo "workers seen at $(date '+%H:%M:%S'), socket $socket" > "$out/index.txt"
for minute in $minutes; do
    while [ $(( $(date +%s) - start )) -lt $(( minute * 60 )) ]; do sleep 5; done
    query "SELECT application_name, pid FROM pg_stat_activity WHERE application_name IN ('pruner','writer_0','applier_0') ORDER BY 1" |
        while IFS='|' read -r name pid; do
            sample "$pid" "$seconds" -file "$out/m${minute}-${name}.txt" > /dev/null 2>&1 &
        done
    wait
    echo "minute $minute sampled at $(date '+%H:%M:%S')" >> "$out/index.txt"
done
