# Contributing

SyntraDB is licensed under `GPL-3.0-only`. By contributing you agree your work is
released under that license. Follow the [code of conduct](CODE_OF_CONDUCT.md).

## Before you start

Open an issue for anything larger than a bug fix. Read
[production readiness gates](docs/readiness.md) first. The project keeps native
PostgreSQL execution as the default route, so changes need correctness evidence
before any speed claim.

## Set up and check

The tested baseline is PostgreSQL 18.6 and pgrx 0.19.3 on Rust 1.99.0.

```sh
bash scripts/setup.sh
bash scripts/check.sh
```

`scripts/check.sh` runs formatting, Clippy and the tests. Run builds as a regular
user, since PostgreSQL refuses to run as root.

## Pull requests

- Add tests that fail when the behavior is wrong. For a fix, show the test failing
  before the change.
- Changes to SQL objects need an upgrade script, a downgrade script and updates to
  the matching `scripts/upgrade-check.sh` and `scripts/downgrade-*-check.sh` runs.
- Record benchmark or soak results with raw samples, hardware and the commit.
  Document failed gates as failed.
- Update the relevant page under `docs/` and add a line to `CHANGELOG.md`.
- Keep commits focused. CI must pass on the latest commit.
