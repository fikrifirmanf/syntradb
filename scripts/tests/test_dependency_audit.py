"""Exercise scanner exit propagation at the external CLI boundary, offline."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


class DependencyAuditTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory(prefix="syntra-audit-test-")
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name)
        (self.root / "scripts").mkdir()
        source = Path(__file__).resolve().parents[1] / "dependency-audit.sh"
        shutil.copyfile(source, self.root / "scripts/dependency-audit.sh")
        self.database = self.root / ".tools/advisory-db"
        self.database.mkdir(parents=True)
        for repo in [self.root, self.database]:
            subprocess.run(["git", "init", "--quiet", str(repo)], check=True, timeout=15)
            subprocess.run(
                ["git", "-C", str(repo), "-c", "user.name=Audit fixture",
                 "-c", "core.hooksPath=/dev/null", "-c", "commit.gpgsign=false",
                 "-c", "user.email=audit@localhost", "commit", "--quiet",
                 "--allow-empty", "-m", "Create isolated audit fixture"],
                check=True, timeout=15,
            )
        self.fake = self.root / "scanner.py"
        self.fake.write_text('''#!/usr/bin/env python3
import json, os, sys
from pathlib import Path
if sys.argv[1:] == ["--version"]:
    # unslop-ignore CONSOLE_IN_TEST -- Scanner CLI protocol output, asserted by the wrapper tests.
    print("cargo-audit 0.22.2")
    raise SystemExit(0)
with Path(os.environ["AUDIT_CALLS"]).open("a") as out:
    out.write(json.dumps(sys.argv[1:])+"\\n")
lock = sys.argv[sys.argv.index("--file")+1]
failed = lock == os.environ.get("FAIL_LOCK")
# unslop-ignore CONSOLE_IN_TEST -- Scanner JSON protocol output, asserted from each saved report.
print(json.dumps({"vulnerabilities":{"found":failed,"count":int(failed)}}))
raise SystemExit(1 if failed else 0)
''')
        self.fake.chmod(0o700)
        self.calls = self.root / "calls.jsonl"

    def run_audit(self, failed_lock=""):
        environment = dict(os.environ, SYNTRA_AUDIT_BIN=str(self.fake),
                           AUDIT_CALLS=str(self.calls), FAIL_LOCK=failed_lock)
        return subprocess.run(
            ["bash", str(self.root / "scripts/dependency-audit.sh"), "reports"],
            cwd=self.root, env=environment, capture_output=True, text=True,
            check=False, timeout=15,
        )

    def assert_two_scans(self):
        calls = [json.loads(line) for line in self.calls.read_text().splitlines()]
        self.assertEqual(len(calls), 2)
        self.assertEqual([call[call.index("--file")+1] for call in calls],
                         ["Cargo.lock", "fuzz/Cargo.lock"])
        self.assertNotIn("--no-fetch", calls[0])
        self.assertIn("--no-fetch", calls[1])
        for call in calls:
            self.assertEqual([call[i+1] for i, item in enumerate(call) if item == "--deny"],
                             ["unsound", "yanked"])
        self.assertEqual((self.root / "reports/advisory-commit.txt").read_text().strip(),
                         subprocess.check_output(["git", "-C", str(self.database),
                                                  "rev-parse", "HEAD"], text=True, timeout=15).strip())

    def test_first_scan_failure_does_not_skip_the_second_or_turn_green(self):
        result = self.run_audit("Cargo.lock")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assert_two_scans()
        for name, expected in [("root", True), ("fuzz", False)]:
            report = json.loads((self.root / f"reports/{name}.json").read_text())
            self.assertEqual(report["vulnerabilities"]["found"], expected)

    def test_second_scan_failure_fails_the_whole_command(self):
        result = self.run_audit("fuzz/Cargo.lock")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assert_two_scans()
        self.assertEqual(json.loads((self.root / "reports/fuzz.json").read_text())
                         ["vulnerabilities"]["count"], 1)

    def test_clean_scans_succeed_and_preserve_both_reports(self):
        result = self.run_audit()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assert_two_scans()
        for name in ["root", "fuzz"]:
            report = json.loads((self.root / f"reports/{name}.json").read_text())
            self.assertEqual(report["vulnerabilities"], {"found": False, "count": 0})


if __name__ == "__main__":
    unittest.main()
