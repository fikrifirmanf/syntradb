# Serializable heap verification for 0.9.0

Local PostgreSQL 18.6 checks pass for the
[fresh heap SSI contract](serializable-heap-reads.md). Native Linux CI and the
27-case recovery and lifecycle suite remain required before merging 0.9.0.
This feature does not resolve the earlier held-snapshot throughput failure.

## Fresh reads and transaction conflicts

The final production-harness check passed nine API and transaction-mode
combinations, 21 conflicting transaction pairs and three read-time aborts with
subsequent reuse of the same backend. It also confirmed that three capture and
cached-state APIs still reject SSI with `0A000`.

Conflict participants use custom reads without a native heap oracle in the same
transaction. Quiescent native checks run after the participants finish. This
avoids accidentally supplying a missing source dependency through the verifier.

Negative controls failed as intended:

- The old 0.8.0 implementation rejected acceptance with `0A000`.
- A constant query that bypassed the source failed the predicate-lock check.
- With that separate lock check disabled, the same bypass let both writers
  commit and failed the independent conflict check.
- An inverted expected read-time SQLSTATE failed the read-abort check.

All temporary controls were restored before the final run. The
[versioned local evidence](../benchmarks/evidence/ssi-local-090-2026-10-31.json)
records the completed checks, profiles and original report SHA-256. The runner
and extension fingerprints both equal
`1c4518ec479c2179c6dfea89bc60a2a9fbb8a76e1567519c83615be7b89d699f`.
The full local report is `benchmarks/results/ssi-090-final.json`.

`scripts/check.sh` passed six Python tests, Rust tests, formatting, workspace
Clippy and 218 PostgreSQL tests. These PostgreSQL tests retain the existing
permission, snapshot, cancellation and integer-layout checks. The new SSI
conflicts are separate real multi-session checks in the production harness.
The generic missing-test mapper does not recognize this test arrangement.

## Version lifecycle

The following local checks passed:

- Upgrade from 0.8.0 to 0.9.0 with pending captured history. All 532 catalog
  entries matched a fresh install.
- Thirty seconds of live upgrade writes from four writers. All 2,040 committed
  increments survived, no writes failed, and heap, event count and checkpoint agreed.
- A 0.9.0→0.8.0→0.9.0 round trip with both libraries. Fresh SSI worked on 0.9.0,
  the old library rejected both fresh APIs with `0A000`, and capture stayed exact.
- Round trips through 0.7.0 and 0.6.0. Dependent views still blocked removal of
  grouping. The older downgrade waited for an apply and moved eight deltas inline.

The 0.8.0 archive omits seven nested baseline archives. Retained source bytes
and modes were verified against commit `28145d0`. Repeated packing was identical.
The old library still reported its verified source fingerprint after building
from that archive. No stored format or function signature changed in 0.9.0.

Local logs use the prefixes `ssi-090-check`, `ssi-080-upgrade`,
`ssi-080-upgrade-load`, `ssi-090-downgrade`, `ssi-090-to-070` and `ssi-090-to-060`.
The live-write run is a correctness check; its duration is not a throughput claim.
