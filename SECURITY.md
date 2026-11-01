# Security policy

SyntraDB is a prototype and has no supported production release. See
[production readiness gates](docs/readiness.md). Security review of the
extension's unsafe boundaries is still open.

## Supported versions

| Version | Status |
| --- | --- |
| 0.12.x | Receives security fixes |
| Earlier | Not supported. Upgrade first. |

## Reporting a vulnerability

Do not open a public issue for a vulnerability. Use GitHub's private reporting:
<https://github.com/fikrifirmanf/syntradb/security/advisories/new>.

Include the affected version, PostgreSQL version, a minimal reproduction and the
impact you expect, such as privilege escalation, memory unsafety, a snapshot or
row-level security bypass, or data corruption.

## What to expect

The project has no service-level commitment. Reports are handled on a best-effort
basis. Confirmed issues are fixed on `main`, released as a patch version and
credited in the advisory unless you ask otherwise.
