"""Acknowledgement counters must stay complete while a writer publishes."""

from pathlib import Path
import subprocess
import tempfile
import unittest


HELPER = Path(__file__).resolve().parents[1] / "lib/ack-counter.sh"


class AcknowledgementCounterTests(unittest.TestCase):
    def run_bash(self, script, directory):
        return subprocess.run(
            ["bash", "-euo", "pipefail", "-c", script, "test", str(HELPER), str(directory)],
            capture_output=True,
            text=True,
            timeout=10,
            check=False,
        )

    def test_old_count_remains_complete_until_the_atomic_rename(self):
        with tempfile.TemporaryDirectory(prefix="syntra ack test ") as directory:
            counter = Path(directory) / "acked-1"
            counter.write_text("0\n")
            result = self.run_bash(
                r'''
                source "$1"
                counter="$2/acked-1"
                renames=0
                mv() {
                    # Observe the real files at the rename boundary, then
                    # delegate to the real filesystem rename operation.
                    [[ "$(cat "$counter")" == "0" ]] || return 61
                    [[ "$(cat "$counter.next")" == "123" ]] || return 62
                    renames=$((renames + 1))
                    command mv "$@"
                }
                syntra_publish_ack "$counter" 123
                [[ "$renames" == "1" ]]
                ''',
                directory,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(counter.read_text(), "123\n")
            self.assertFalse(Path(str(counter) + ".next").exists())

    def test_invalid_count_does_not_replace_the_previous_acknowledgements(self):
        with tempfile.TemporaryDirectory() as directory:
            counter = Path(directory) / "acked-1"
            counter.write_text("42\n")
            result = self.run_bash(
                r'''
                source "$1"
                if syntra_publish_ack "$2/acked-1" not-a-count; then
                    exit 1
                else
                    [[ "$?" == "2" ]]
                fi
                ''',
                directory,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(counter.read_text(), "42\n")
            self.assertFalse(Path(str(counter) + ".next").exists())

    def test_failed_partial_write_is_not_published_and_preserves_its_error(self):
        with tempfile.TemporaryDirectory() as directory:
            counter = Path(directory) / "acked-1"
            counter.write_text("42\n")
            result = self.run_bash(
                r'''
                source "$1"
                printf() { builtin printf partial; return 17; }
                if syntra_publish_ack "$2/acked-1" 43; then
                    exit 1
                else
                    [[ "$?" == "17" ]]
                fi
                ''',
                directory,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(counter.read_text(), "42\n")
            self.assertEqual(Path(str(counter) + ".next").read_text(), "partial")


if __name__ == "__main__":
    unittest.main()
