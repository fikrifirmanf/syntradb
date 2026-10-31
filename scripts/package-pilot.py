#!/usr/bin/env python3
"""Build a source-backed pilot bundle and test it in a private PostgreSQL copy."""
import argparse
import gzip
import hashlib
import io
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import tarfile
import tempfile
import tomllib

ROOT = Path(__file__).resolve().parents[1]


def run(argv, *, timeout=120, **kwargs):
    return subprocess.run([str(x) for x in argv], check=kwargs.pop('check', True), timeout=timeout, **kwargs)


def output(argv):
    return run(argv, capture_output=True, text=True).stdout.strip()


def sha(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def fingerprint(root):
    # Independent implementation of build-support/source_fingerprint.rs v1.
    paths = {Path(x) for x in ['Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml', '.cargo/config.toml']}
    suffixes = {'.rs', '.toml', '.sql', '.sh', '.control', '.bin'}
    def walk_error(error):
        raise error
    for directory in ['crates', 'build-support', 'scripts', 'examples']:
        if (root / directory).is_symlink():
            raise ValueError('source fingerprint refuses symlinks')
        for base, dirs, files in os.walk(root / directory, followlinks=False, onerror=walk_error):
            for name in dirs + files:
                if (Path(base) / name).is_symlink():
                    raise ValueError('source fingerprint refuses symlinks')
            dirs[:] = [x for x in dirs if x != 'target']
            paths.update((Path(base) / x).relative_to(root) for x in files if Path(x).suffix in suffixes)
    digest = hashlib.sha256(b'syntra-source-v1\0')
    for relative in sorted(paths):
        path = root / relative
        if any((root / part).is_symlink() for part in [relative, *relative.parents]) or not path.is_file():
            raise ValueError('source fingerprint requires regular files')
        name, data = relative.as_posix().encode(), path.read_bytes()
        for part in [len(name).to_bytes(8, 'little'), name, len(data).to_bytes(8, 'little'), data]:
            digest.update(part)
    return digest.hexdigest()


def file_manifest(directory):
    result = {}
    for path in sorted(directory.rglob('*')):
        if path.is_symlink():
            raise ValueError('bundle refuses symlinks')
        if path.is_file():
            result[path.relative_to(directory).as_posix()] = {'sha256': sha(path), 'bytes': path.stat().st_size}
    return result


def validate_files(directory, expected):
    actual = file_manifest(directory)
    if actual != expected:
        raise ValueError('bundle payload differs from its file manifest')


def archive(directory, destination, epoch):
    # Stable archive metadata for the same staged bytes. This does not promise
    # bit-identical compiler output across different build paths or hosts.
    with destination.open('xb') as raw, gzip.GzipFile(fileobj=raw, mode='wb', filename='', mtime=0) as compressed:
        with tarfile.open(fileobj=compressed, mode='w') as tar:
            for path in sorted(directory.rglob('*')):
                if path.is_symlink() or not path.is_file():
                    if path.is_dir() and not path.is_symlink():
                        continue
                    raise ValueError('archive requires regular bundle files')
                info = tarfile.TarInfo(path.relative_to(directory).as_posix())
                data = path.read_bytes()
                info.size, info.mtime, info.mode = len(data), epoch, 0o644
                tar.addfile(info, io.BytesIO(data))


def config(pg_config, flag):
    return Path(output([pg_config, flag]))


def verify(bundle, pg_config, scratch, version, expected_fingerprint):
    """Never install into the supplied PostgreSQL prefix or start a system service."""
    prefix = config(pg_config, '--bindir').parent
    private = scratch / 'postgresql'
    shutil.copytree(prefix, private, symlinks=True)
    binary = private / 'bin'
    cfg = binary / 'pg_config'
    lib, extension = config(cfg, '--pkglibdir'), config(cfg, '--sharedir') / 'extension'
    for path in [lib, extension]:
        if not path.resolve().is_relative_to(private.resolve()):
            raise ValueError('relocated pg_config points outside the private install')
        # Remove old installed assets before copying any payload. CREATE
        # EXTENSION must load the packaged library and generated SQL.
        for old in path.glob('syntradb*'):
            if old.is_symlink() or old.is_file():
                old.unlink()
            else:
                raise ValueError('unexpected directory in prior extension assets')
    for source in (bundle / 'lib').iterdir():
        shutil.copy2(source, lib / source.name)
    for source in (bundle / 'extension').iterdir():
        shutil.copy2(source, extension / source.name)
    data = scratch / 'data'
    run([binary / 'initdb', '-D', data, '--username=syntra_package', '--auth=trust', '--no-locale', '--encoding=UTF8'], capture_output=True)
    environment = dict(os.environ, PGCONNECT_TIMEOUT='3', PGOPTIONS='-c statement_timeout=5000 -c lock_timeout=2000')
    with tempfile.TemporaryDirectory(prefix='syntra-pkg-', dir='/tmp') as socket:
        with (data / 'postgresql.conf').open('a') as conf:
            conf.write(f"\nlisten_addresses = ''\nunix_socket_directories = '{socket}'\nport = 28969\n")
        try:
            run([binary / 'pg_ctl', '-D', data, '-l', scratch / 'postgresql.log', '-w', '-t', '60', 'start'])
            sql = """CREATE EXTENSION syntradb;
CREATE TABLE package_source(id int4 PRIMARY KEY, v int4);
INSERT INTO package_source VALUES (1,10),(2,20);
SELECT syntra_seed_capture('package_source','id','v',64);
UPDATE package_source SET v=v+1 WHERE id=1;
SELECT applied,pruned FROM syntra_apply_prune_capture('package_source');
SELECT json_build_object('version',syntra_version(),'profile',syntra_build_profile(),
 'fingerprint',syntra_source_fingerprint(),'rows',s.rows,'sum',s.sum,'route',s.route,
 'pending',(SELECT pending_events FROM syntra_capture_lag('package_source')))
FROM syntra_checkpoint_int4_summary('package_source','v') AS s;
BEGIN ISOLATION LEVEL SERIALIZABLE READ ONLY;
SELECT rows,sum FROM syntra_int4_summary('package_source','v');
COMMIT;
"""
            result = run([binary / 'psql', '-XAtq', '-v', 'ON_ERROR_STOP=1', '-h', socket, '-p', '28969', '-U', 'syntra_package', '-d', 'postgres'], input=sql, capture_output=True, text=True, env=environment, timeout=30, check=False)
            (scratch / 'sql.stdout').write_text(result.stdout)
            (scratch / 'sql.stderr').write_text(result.stderr)
            result.check_returncode()
            rows = [json.loads(line) for line in result.stdout.splitlines() if line.startswith('{')]
            expected = {'version': version, 'profile': 'release', 'fingerprint': expected_fingerprint,
                        'rows': 2, 'sum': 31, 'route': 'checkpoint', 'pending': 0}
            if rows != [expected] or '\n2|31\n' not in '\n' + result.stdout:
                raise ValueError('packaged extension failed version, fingerprint, capture, checkpoint or SSI checks')
            return {'status': 'passed', 'checks': expected, 'fresh_heap_ssi': '2|31', 'private_install': True}
        finally:
            if (data / 'postmaster.pid').is_file():
                run([binary / 'pg_ctl', '-D', data, '-w', '-t', '30', 'stop', '-m', 'immediate'], capture_output=True)


def build(pg_config, destination):
    if output(['git', '-C', ROOT, 'status', '--porcelain']):
        raise ValueError('pilot packages require a clean committed working tree')
    if output([pg_config, '--version']) != 'PostgreSQL 18.6':
        raise ValueError('pilot packaging currently requires PostgreSQL 18.6')
    flags = output([pg_config, '--configure'])
    if not all(flag in flags for flag in ['--disable-cassert', '--disable-debug', 'CFLAGS=-O2']):
        raise ValueError('pilot packaging requires the pinned production PostgreSQL build flags')
    commit = output(['git', '-C', ROOT, 'rev-parse', 'HEAD'])
    epoch = int(output(['git', '-C', ROOT, 'show', '-s', '--format=%ct', commit]))
    version = tomllib.loads((ROOT / 'Cargo.toml').read_text())['workspace']['package']['version']
    expected_fingerprint = fingerprint(ROOT)
    destination.mkdir(parents=True, exist_ok=False)
    bundle = destination / 'bundle'
    bundle.mkdir()
    for name in ['lib', 'extension']:
        (bundle / name).mkdir()
    stage = destination / 'stage'
    env = dict(os.environ, CARGO_TARGET_DIR=str(ROOT / 'target/production'),
               PGRX_PG_CONFIG_PATH=str(pg_config))
    with (destination / 'build.log').open('w') as log:
        run(['bash', ROOT / 'scripts/pgrx.sh', 'package', '--cargo=--locked', '--pg-config', pg_config,
             '--out-dir', stage], timeout=1800, env=env, stdout=log, stderr=subprocess.STDOUT)
    library = stage / config(pg_config, '--pkglibdir').relative_to('/') / 'syntradb.so'
    extension = stage / config(pg_config, '--sharedir').relative_to('/') / 'extension'
    shutil.copy2(library, bundle / 'lib/syntradb.so')
    sql_files = sorted(extension.glob('syntradb*'))
    required = {'syntradb.control', f'syntradb--{version}.sql'} | {p.name for p in (ROOT / 'crates/syntra-pg/sql').glob('syntradb*.sql')}
    if {p.name for p in sql_files} != required:
        raise ValueError('package is missing SQL install or lifecycle assets, or contains unexpected assets')
    for path in sql_files:
        shutil.copy2(path, bundle / 'extension' / path.name)
    shutil.copy2(ROOT / 'LICENSE', bundle / 'LICENSE')
    source = run(['git', '-C', ROOT, 'archive', '--format=tar', commit], capture_output=True).stdout
    (bundle / 'source.tar.gz').write_bytes(gzip.compress(source, mtime=0))
    files = file_manifest(bundle)
    validate_files(bundle, files)
    verification = verify(bundle, pg_config, destination / 'verification', version, expected_fingerprint)
    (bundle / 'verification.json').write_text(json.dumps(verification, indent=2) + '\n')
    if output(['git', '-C', ROOT, 'status', '--porcelain']) or output(['git', '-C', ROOT, 'rev-parse', 'HEAD']) != commit:
        raise ValueError('source changed during the package build')
    rust = output(['rustc', '-vV'])
    manifest = {'schema_version': 1, 'status': 'pilot; not a production release', 'commit': commit,
                'version': version, 'source_fingerprint': expected_fingerprint,
                'postgresql': output([pg_config, '--version']), 'postgresql_configure': flags,
                'platform': platform.platform(), 'libc': platform.libc_ver(),
                'rustc': rust, 'files': file_manifest(bundle), 'source_date_epoch': epoch,
                'build_environment': {k: os.environ[k] for k in ['RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS', 'CFLAGS', 'CPPFLAGS'] if k in os.environ}}
    (bundle / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
    target = next(line.removeprefix('host: ') for line in rust.splitlines() if line.startswith('host: '))
    tar = destination / f'syntradb-{version}-pg18-{target}-pilot.tar.gz'
    archive(bundle, tar, epoch)
    (destination / 'SHA256SUMS').write_text(f'{sha(tar)}  {tar.name}\n')
    print(json.dumps({'archive': str(tar), 'sha256': sha(tar), 'verification': verification}))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--pg-config', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    build(args.pg_config.resolve(strict=True), args.output.resolve())
