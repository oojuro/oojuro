#!/usr/bin/env python3
"""Package the reviewed file tool and selected dependency notices; no publication."""
import argparse
import hashlib
import io
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import tomllib

ROOT = Path(__file__).resolve().parents[1]
ALLOWED = {'0BSD', 'MIT', 'MIT-0', 'Apache-2.0', 'LLVM-exception', 'CC0-1.0',
           'BSD-3-Clause', 'ISC', 'Unlicense', 'Zlib', 'Unicode-3.0', 'OR', 'AND', 'WITH'}


def run(*args):
    return subprocess.check_output(args, cwd=ROOT, text=True).strip()


def require(condition, message):
    if not condition:
        raise ValueError(message)


def sha(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def registry_inputs(archive, checksum):
    data = archive.read_bytes()
    require(hashlib.sha256(data).hexdigest() == checksum, 'registry archive checksum mismatch')
    result = {}
    with tarfile.open(fileobj=io.BytesIO(data)) as contents:
        manifest = contents.extractfile(archive.stem + '/Cargo.toml').read()
        for member in contents.getmembers():
            if not member.isfile() or not re.search(r'license|copying|copyright|notice|authors', Path(member.name).name, re.I):
                continue
            name = Path(member.name).relative_to(archive.stem)
            require(not name.is_absolute() and '..' not in name.parts, 'unsafe registry notice path')
            value = contents.extractfile(member).read()
            require(value.strip(), 'empty registry notice')
            result[str(name)] = value
    require(result, 'registry package omitted notices: ' + archive.name)
    return tomllib.loads(manifest.decode())['package'], result


def put(source, destination):
    require(source.is_file() and not source.is_symlink() and source.stat().st_size, 'missing regular input: ' + str(source))
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(source, destination)


def package(binary, output, target):
    require(not output.exists(), 'package output must be new')
    require(not run('git', 'status', '--porcelain'), 'commit reviewed source before packaging')
    tree = run('cargo', 'tree', '--locked', '--offline', '-p', 'ojo-tools', '--edges', 'normal,build',
               '--target', target, '--prefix', 'none', '--format', '{p}')
    selected = set(re.findall(r'^([\w-]+) v([^\s]+)', tree, re.M))
    require(('ojo-tools', '0.1.0') in selected and ('ojo', '0.1.0') in selected, 'missing file-tool graph')
    lock = tomllib.loads((ROOT / 'Cargo.lock').read_text())['package']
    locked = {(p['name'], p['version']): p for p in lock}
    cache = Path(os.environ.get('CARGO_HOME', Path.home() / '.cargo')) / 'registry/cache'
    dependencies = []
    for name, version in sorted(selected):
        item = locked[(name, version)]
        if item.get('source') is None:
            continue
        require(item['source'] == 'registry+https://github.com/rust-lang/crates.io-index', 'unreviewed non-registry tool dependency')
        archives = list(cache.glob(f'*/{name}-{version}.crate'))
        require(len(archives) == 1, 'missing or ambiguous registry archive: ' + name)
        archive = archives[0]
        checksum = item['checksum']
        p, files = registry_inputs(archive, checksum)
        # Read normalized registry metadata from the same checksum-verified archive;
        # do not fetch metadata for unrelated workspace benchmark dependencies.
        require(p['name'] == name and p['version'] == version, 'registry package identity mismatch')
        require(p['license'] and set(re.findall(r'[\w.-]+', p['license'])) <= ALLOWED, 'unreviewed dependency license: ' + p['name'])
        for relative, value in files.items():
            destination = output / 'licenses' / f'{name}-{version}' / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_bytes(value)
        dependencies.append({'name': p['name'], 'version': p['version'], 'license': p['license'],
                             'repository': p.get('repository'), 'registry_sha256': checksum})
    require(dependencies, 'empty dependency notice inventory')
    for name in ['LICENSE', 'NOTICE', 'Cargo.lock']:
        put(ROOT / name, output / name)
    rust = Path(run('rustc', '--print', 'sysroot')) / 'share/doc/rust'
    put(rust / 'COPYRIGHT-library.html', output / 'licenses/rust/COPYRIGHT-library.html')
    for path in sorted((rust / 'licenses').iterdir()):
        put(path, output / 'licenses/rust' / path.name)
    put(binary, output / 'bin/ojo')
    (output / 'bin/ojo').chmod(0o755)
    version = run(str(binary), '--version')
    require(version == 'ojo 0.1.0 (wire 0.1)', 'binary version disagrees with reviewed package')
    manifest = {'source_commit': run('git', 'rev-parse', 'HEAD'), 'target': target, 'version': version,
                'rustc': run('rustc', '-vV'), 'cargo': run('cargo', '--version'),
                'dependencies': dependencies, 'features': ['archive'],
                'files': {str(p.relative_to(output)): sha(p) for p in sorted(output.rglob('*')) if p.is_file()}}
    (output / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
    for name, digest in manifest['files'].items():
        require(sha(output / name) == digest, 'package checksum verification failed')
    print(json.dumps({'source_commit': manifest['source_commit'], 'dependencies': len(dependencies),
                      'binary_sha256': sha(output / 'bin/ojo')}))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target', default='x86_64-unknown-linux-gnu')
    args = parser.parse_args()
    package(args.binary.resolve(), args.output.resolve(), args.target)
