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

The bundle contains `lib/syntradb.so` (`.dylib` on macOS), the generated install SQL,
every version transition script, the control file, license, repository source
archive, build manifest and verification result. The source archive records the
exact Git commit. Dependencies remain pinned in Cargo.lock; this is not an
offline vendor bundle.

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
The cluster stops on both success and failure. Build and SQL logs remain beside
the bundle. A failed build never produces a verified final archive.

Five offline tests cover platform library names, payload tampering, missing and
extra files, symlink rejection, stable archives and the source-fingerprint format. Disabling the
manifest comparison made the tampering test fail; the comparison was restored.
Native installation checks provide separate evidence beyond these offline tests.

Regular CI retains the pilot archive, manifest, verification result and logs
for seven days. It publishes no release tag and deploys no database. Full
recovery and lifecycle checks remain separate from this installation smoke.
