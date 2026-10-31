# Dependency advisory checks

Run `bash scripts/dependency-audit.sh` after installing the pinned scanner:

```sh
cargo install cargo-audit --version 0.22.2 --locked --root .tools/audit
bash scripts/dependency-audit.sh
```

Both lockfiles use the same RustSec data. The script scans each file.
Known security flaws fail the command. So do unsoundness advisories, yanked
crates and fetch errors. Warnings about unmaintained crates stay in the JSON
for review.

The `Dependency advisories` workflow runs on lockfile and scanner changes,
manually, and each Monday. Artifacts retain both reports, scanner version,
source commit and advisory database revision for seven days. This checks known
dependency advisories; it does not prove the code or deployed database secure.

## Current finding

The initial 2026-10-31 scan used cargo-audit 0.22.2 and RustSec database commit
`2295466c4e9df26647fd67d8fa1c705ac2fc7f81`. It found zero known vulnerabilities in
both lockfiles. The workspace report includes
[RUSTSEC-2021-0127](https://rustsec.org/advisories/RUSTSEC-2021-0127.html):
`serde_cbor` 0.11.2 is unmaintained. Its dependency path is
`syntra-pg -> pgrx 0.19.3 -> serde_cbor`.

pgrx includes that crate for its CBOR helpers. SyntraDB's
current SQL adapter does not use `PostgresType` or those encode/decode helpers.
That source inspection narrows the current exposure; it does not remove the
dependency or guarantee safety. Recheck this finding when changing pgrx or adding
serialized custom SQL types. No advisory is hidden with an ignore list.

## Scanner orchestration tests

Three local tests stub only the scanner command. They run offline. They check
both lockfiles, the shared database revision and failures from either scan.
Swallowing a scan error made two tests fail. The correct code then passed.

`python3 -m unittest discover -s scripts/tests -p 'test_*.py'` runs these cases
and the three acknowledgment-counter tests. The generic test connector does not
detect this standalone unittest folder, so its skipped result is not the evidence
for those six tests. The explicit command passed. The real scanner separately
checked both project lockfiles against the advisory revision recorded above.
