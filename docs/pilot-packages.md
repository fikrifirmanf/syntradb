# Pilot installation bundles

`scripts/package-pilot.py` builds a release library and SQL bundle, then tests
it in a private copy of PostgreSQL. It does not install into the supplied
PostgreSQL prefix or start a system service. These bundles are pilot artifacts,
not a production release or a general binary compatibility promise.

Build from a clean committed checkout after setting up the pinned toolchain:

```sh
bash scripts/setup.sh
bash scripts/setup-production.sh
python3 scripts/package-pilot.py \
  --pg-config "$PWD/.tools/postgresql-production/18.6/install/bin/pg_config" \
  --output "$PWD/benchmarks/results/pilot-package"
```

The output directory must be new. Python 3.11 or later is required. The current
target is PostgreSQL 18.6 built with the repository's production flags. The
bundle is specific to the build host's target and dependencies. Linux CI checks
Ubuntu 24.04 x86_64; local macOS builds need their own installation evidence.

The bundle contains `lib/syntradb.so` (`.dylib` on macOS), install SQL and every
upgrade script. It also includes the control file, license, source archive,
build manifest and test result. The source archive records the exact Git commit.
Dependencies remain pinned in Cargo.lock; this is not an offline vendor bundle.

The manifest records compiler, PostgreSQL flags, platform, source fingerprint
and file checksums. Archive metadata is stable for identical staged bytes.
Compiler output can differ across build paths and hosts, so bit-identical
binary builds have not been established.

## Installation check

The verifier copies PostgreSQL into a private directory, removes any previously
installed SyntraDB assets there, and installs only the bundle's library and SQL.
It starts a disposable cluster using a private Unix socket. It then checks:

- extension creation, version, release profile and source fingerprint;
- capture setup, a committed write, apply/prune and an exact checkpoint result;
- a fresh heap read in a serializable transaction.

Every check must pass before the final archive and SHA256SUMS file are written.
It also unpacks a test archive and checks each file's bytes. A second fresh
cluster loads those files before the tool names the final archive.
The cluster stops on both success and failure. Build and SQL logs remain beside
the bundle. A failed build never produces a verified final archive.

Six offline tests cover platform library names, payload tampering, missing and
extra files, unsafe archives, stable archives and the source-fingerprint format. Disabling the
manifest comparison made the tampering test fail; the comparison was restored.
Native installation checks provide separate evidence beyond these offline tests.
The generic test connector does not detect this standalone unittest layout;
`scripts/check.sh` runs explicit unittest discovery.

Regular CI retains the pilot archive, manifest, verification result and logs
for seven days. It publishes no release tag and deploys no database. Full
recovery and lifecycle checks remain separate from this installation smoke.

## Local verification

The macOS arm64 bundle passed both staged and extracted-archive installation
checks at `fdaa7a2`. Its PostgreSQL and extension profiles match the pinned
production configuration, and its runtime source fingerprint matches the source
snapshot. The [versioned evidence](../benchmarks/evidence/pilot-package-macos-2026-11-01.json)
records the archive checksum, file manifest, SQL output and negative controls.

A wrong expected fingerprint failed verification. Removing the bundle's library
failed extension creation despite an older library in the original PostgreSQL
prefix. Both failed test clusters stopped. This checks that verification loads
the packaged assets and propagates failures.

The explicit Python suite passed 18 tests, including six packaging tests.
The [Linux CI run](https://github.com/fikrifirmanf/syntradb/actions/runs/41231225170)
also passed at `fdaa7a2`: 218 PostgreSQL tests, 18 Python tests, the Rust checks
and both package installation checks. The Ubuntu 24.04 x86_64 archive checksum is
`d033070a16106e1134e7067a7dc352405fa8e3c2c8647a769a5653344239236f`.
The [Linux evidence](../benchmarks/evidence/pilot-package-linux-2026-11-01.json)
records the CI package summary, checksum and terminal job result. Downloading
the Linux binary artifact stalled locally, and a bounded retry timed out.
Its producer-side archive installation checks passed. The full manifest and
SQL logs remain in the seven-day artifact; a separate local checksum check is
pending. These results cover the two tested build environments, not a general
deployment support matrix.
