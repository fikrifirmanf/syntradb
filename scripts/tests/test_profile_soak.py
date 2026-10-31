"""Offline boundary checks; actual kernel sampling is verified by the CI probe."""
from collections import Counter
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock

SOURCE = Path(__file__).resolve().parents[2] / "benchmarks/scripts/profile_soak.py"
SPEC = importlib.util.spec_from_file_location("profile_soak", SOURCE)
profiler = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(profiler)
EXPECTED_ACTORS = ["applier_0", "applier_1", "keyed_monitor", "pruner", "read_committed",
                   "repeatable_read", "vacuum", "writer_0", "writer_1"]


class ProfileSoakTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory(prefix="syntra-profile-test-")
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name)

    def test_schedule_covers_retained_and_released_phases(self):
        self.assertEqual(profiler.schedule(240), [1, 3])
        self.assertEqual(profiler.schedule(7200), [8, 28, 58, 65])
        with self.assertRaises(ValueError):
            profiler.schedule(0)

    def test_requires_exact_worker_identity_and_positive_integer_pids(self):
        rows = [{"application_name": name, "pid": index+100}
                for index, name in enumerate(EXPECTED_ACTORS)]
        self.assertEqual(profiler.actors(rows), {name: index+100 for index, name in enumerate(EXPECTED_ACTORS)})
        for bad_pid in [True, 0, -1, "100"]:
            invalid = [dict(row) for row in rows]
            invalid[0]["pid"] = bad_pid
            with self.subTest(pid=bad_pid), self.assertRaises(ValueError):
                profiler.actors(invalid)
        with self.assertRaises(ValueError):
            profiler.actors(rows[:-1])
        invalid = [dict(row) for row in rows]
        invalid[0]["pid"] = invalid[1]["pid"]
        with self.assertRaises(ValueError):
            profiler.actors(invalid)

    def test_record_targets_only_named_pids_with_userspace_dwarf_samples(self):
        perf = profiler.Perf("/perf", sudo=True)
        argv = perf.record_command(self.root / "perf.data", [100, 200], 15)
        self.assertEqual(argv[:3], ["sudo", "-n", "/perf"])
        self.assertEqual(argv[argv.index("-p")+1], "100,200")
        self.assertEqual(argv[argv.index("-e")+1], "cpu-clock:u")
        self.assertEqual(argv[argv.index("--call-graph")+1], "dwarf,16384")
        self.assertNotIn("-a", argv)
        with self.assertRaises(ValueError):
            perf.record_command(self.root / "bad", [0], 15)

    def test_sample_counts_ignore_stack_frames_and_other_processes(self):
        def output(argv, **kwargs):
            kwargs["stdout"].write(" 100\n 100\n 200\n 999\n\t7ffff stack_frame (postgres)\n")
            return subprocess.CompletedProcess(argv, 0)
        with mock.patch.object(profiler.subprocess, "run", side_effect=output):
            result = profiler.Perf("/perf").samples(self.root / "data", self.root / "pids.txt", [100, 200])
        self.assertEqual(result, Counter({"100": 2, "200": 1}))

    def test_kernel_denial_fails_collection_and_cleans_up_probe(self):
        busy = mock.Mock(pid=123)
        busy.poll.return_value = None
        recorder = mock.Mock(returncode=1)
        recorder.poll.return_value = 1
        with mock.patch.object(profiler.subprocess, "Popen", side_effect=[busy, recorder]):
            with self.assertRaisesRegex(RuntimeError, "perf record failed"):
                profiler.collect(profiler.Perf("/perf"), self.root, self.root / "profiles", 240, 1)
        status = json.loads((self.root / "profiles/collection.json").read_text())
        self.assertEqual(status["status"], "failed")
        self.assertEqual(status["completed_windows"], [])
        busy.terminate.assert_called_once()
        busy.wait.assert_called_once_with(timeout=10)

    def test_missing_windows_cannot_be_reported_as_complete(self):
        write = {"status": "complete", "soak_seconds": 7200, "completed_windows": [8, 28]}
        (self.root / "collection.json").write_text(json.dumps(write))
        with self.assertRaisesRegex(RuntimeError, "missing or unexpected windows"):
            profiler.analyze(profiler.Perf("/perf"), self.root)


if __name__ == "__main__":
    unittest.main()
