#!/usr/bin/env bash
# Relative verdict for a hold-control log (issue #10). Reads the per-round lines
# "round table tps latency_ms failed dead_tuples horizon_age" plus the final
# "sums" line, compares captured with plain at the same round, and prints the
# result. The absolute offered-load limit is reported by the soak, not here.
#   1. zero failed transactions and equal sums;
#   2. throughput at least 90% of plain up to horizon age 64,000, 80% at any age;
#   3. latency at most 2x plain, once plain latency is above 100 ms.
set -euo pipefail
LOG="${1:?usage: hold-verdict.sh LOG}"
awk '
/^[0-9]+ (plain|captured) / {
    r = $1; t = $2
    tps[r, t] = $3; lat[r, t] = $4; fail[r, t] = $5; age[r, t] = $7
    if (r > n) n = r
    if ($5 + 0 > 0) failed += 1
}
/^sums / { sums_ok = ($2 == $3) ; sums_seen = 1 }
END {
    if (n == 0) { print "relative-verdict FAIL no rounds found"; exit 1 }
    worst = 9; worst_round = 0; bad_tp = 0; bad_lat = 0; worst_lat = 0; lat_round = 0
    for (r = 1; r <= n; r++) {
        if (!((r, "plain") in tps) || !((r, "captured") in tps)) continue
        ratio = tps[r, "captured"] / tps[r, "plain"]
        floor = (age[r, "captured"] <= 64000) ? 0.90 : 0.80
        if (ratio < worst) { worst = ratio; worst_round = r }
        if (ratio < floor) bad_tp += 1
        if (lat[r, "plain"] > 100) {
            lr = lat[r, "captured"] / lat[r, "plain"]
            if (lr > worst_lat) { worst_lat = lr; lat_round = r }
            if (lr > 2) bad_lat += 1
        }
    }
    ok_correct = (failed == 0 && sums_seen && sums_ok)
    printf "relative-verdict correctness %s (failed rounds %d, sums %s)\n", (ok_correct ? "PASS" : "FAIL"), failed, (sums_seen ? (sums_ok ? "equal" : "differ") : "missing")
    printf "relative-verdict throughput %s (worst captured/plain %.2f at round %d; %d rounds under the floor)\n", (bad_tp == 0 ? "PASS" : "FAIL"), worst, worst_round, bad_tp
    printf "relative-verdict latency %s (worst captured/plain %.1fx at round %d; %d rounds over 2x)\n", (bad_lat == 0 ? "PASS" : "FAIL"), worst_lat, lat_round, bad_lat
    printf "relative-verdict %s\n", ((ok_correct && bad_tp == 0 && bad_lat == 0) ? "PASS" : "FAIL")
}' "$LOG"
