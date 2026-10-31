"""Bundle integrity checks; a native installation smoke runs separately in CI."""
import importlib.util
import io
from pathlib import Path
import tarfile
import tempfile
import unittest

SOURCE = Path(__file__).resolve().parents[1] / 'package-pilot.py'
SPEC = importlib.util.spec_from_file_location('package_pilot', SOURCE)
package = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(package)


class PackagePilotTests(unittest.TestCase):
    def setUp(self):
        scratch = tempfile.TemporaryDirectory(prefix='syntra-bundle-test-')
        self.addCleanup(scratch.cleanup)
        self.root = Path(scratch.name)

    def test_manifest_rejects_changed_missing_and_extra_files(self):
        payload = self.root / 'payload'
        payload.mkdir()
        (payload / 'data').write_bytes(b'abc')
        expected = {'data': {'bytes': 3, 'sha256': 'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad'}}
        self.assertEqual(package.file_manifest(payload), expected)
        package.validate_files(payload, expected)
        (payload / 'data').write_bytes(b'abd')
        with self.assertRaisesRegex(ValueError, 'differs'):
            package.validate_files(payload, expected)
        (payload / 'data').unlink()
        with self.assertRaisesRegex(ValueError, 'differs'):
            package.validate_files(payload, expected)
        (payload / 'data').write_bytes(b'abc')
        (payload / 'extra').write_bytes(b'extra')
        with self.assertRaisesRegex(ValueError, 'differs'):
            package.validate_files(payload, expected)

    def test_manifest_refuses_symlinked_payloads(self):
        payload = self.root / 'payload'
        payload.mkdir()
        (payload / 'link').symlink_to('/outside')
        with self.assertRaisesRegex(ValueError, 'symlinks'):
            package.file_manifest(payload)

    def test_archive_is_stable_and_retains_the_exact_file_bytes(self):
        payload = self.root / 'payload'
        (payload / 'lib').mkdir(parents=True)
        (payload / 'lib/syntradb.so').write_bytes(b'fixture-library')
        (payload / 'LICENSE').write_bytes(b'fixture-license')
        first, second = self.root / 'first.tgz', self.root / 'second.tgz'
        package.archive(payload, first, 123)
        (payload / 'lib/syntradb.so').chmod(0o755)
        package.archive(payload, second, 123)
        self.assertEqual(first.read_bytes(), second.read_bytes())
        with tarfile.open(fileobj=io.BytesIO(first.read_bytes()), mode='r:gz') as tar:
            self.assertEqual(tar.getnames(), ['LICENSE', 'lib/syntradb.so'])
            self.assertEqual(tar.extractfile('lib/syntradb.so').read(), b'fixture-library')
            self.assertEqual([(m.uid, m.gid, m.mtime, m.mode) for m in tar.getmembers()], [(0, 0, 123, 0o644)] * 2)

    def test_source_fingerprint_matches_the_v1_wire_fixture_and_rejects_symlinks(self):
        (self.root / '.cargo').mkdir()
        for directory in ['crates', 'build-support', 'scripts', 'examples']:
            (self.root / directory).mkdir()
        for name, content in [('Cargo.lock', 'lock'), ('Cargo.toml', 'workspace'),
                              ('.cargo/config.toml', 'config'), ('rust-toolchain.toml', 'toolchain')]:
            (self.root / name).write_text(content)
        self.assertEqual(package.fingerprint(self.root), 'b7b3d46b4a4722431d7758195cd94557666f00bd9c3b14c914cdb43048c6a880')
        (self.root / 'crates/file.rs').write_text('changed-source')
        self.assertNotEqual(package.fingerprint(self.root), 'b7b3d46b4a4722431d7758195cd94557666f00bd9c3b14c914cdb43048c6a880')
        (self.root / 'crates/link').symlink_to('/outside')
        with self.assertRaisesRegex(ValueError, 'symlinks'):
            package.fingerprint(self.root)


if __name__ == '__main__':
    unittest.main()
