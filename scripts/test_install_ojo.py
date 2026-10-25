#!/usr/bin/env python3
"""Synthetic installer artifact/refusal checks; no network or user installation."""
import hashlib
import io
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
PACKAGE = 'ojo-v0.1.0-x86_64-unknown-linux-gnu'


class Installer(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.fixture = self.root / 'fixture'
        self.fixture.mkdir()
        self.tools = self.root / 'tools'
        self.tools.mkdir()
        curl = self.tools / 'curl'
        curl.write_text('''#!/usr/bin/env python3
import os,pathlib,shutil,sys
a=sys.argv[1:]
output=pathlib.Path(a[a.index('--output')+1])
name=a[-1].rsplit('/',1)[-1]
shutil.copyfile(pathlib.Path(os.environ['OJO_TEST_FIXTURE'])/name,output)
''')
        curl.chmod(0o755)
        self.env = os.environ | {'PATH': str(self.tools) + os.pathsep + os.environ['PATH'],
                                'OJO_TEST_FIXTURE': str(self.fixture),
                                'OJO_INSTALL_DIR': str(self.root / 'prefix/bin'),
                                'OJO_VERSION': '0.1.0', 'TMPDIR': str(self.root)}

    def archive(self, *, unsafe=False, version='0.1.0'):
        path = self.fixture / (PACKAGE + '.tar.gz')
        files = {'bin/ojo': f'#!/bin/sh\nprintf "ojo {version} (wire 0.1)\\n"\n'.encode(),
                 'LICENSE': b'synthetic license', 'NOTICE': b'synthetic notice',
                 'manifest.json': b'{"synthetic":true}', 'licenses/dep/LICENSE': b'synthetic third party'}
        with tarfile.open(path, 'w:gz') as archive:
            for name, content in files.items():
                member = tarfile.TarInfo(PACKAGE + '/' + name)
                member.size = len(content)
                member.mode = 0o755 if name == 'bin/ojo' else 0o644
                archive.addfile(member, io.BytesIO(content))
            if unsafe:
                member = tarfile.TarInfo(PACKAGE + '/outside')
                member.type = tarfile.SYMTYPE
                member.linkname = str(self.root / 'must-not-write')
                archive.addfile(member)
        digest = hashlib.sha256(path.read_bytes()).hexdigest()
        (self.fixture / 'SHA256SUMS').write_text(digest + '  ' + path.name + '\n')

    def run_installer(self):
        return subprocess.run(['sh', str(ROOT / 'scripts/install-ojo.sh')],
                              env=self.env, capture_output=True, text=True)

    def test_install_and_reinstall_preserve_notices(self):
        self.archive()
        first = self.run_installer()
        self.assertEqual(first.returncode, 0, first.stderr)
        binary = self.root / 'prefix/bin/ojo'
        self.assertEqual(subprocess.check_output([binary, '--version'], text=True).strip(),
                         'ojo 0.1.0 (wire 0.1)')
        self.assertTrue((self.root / 'prefix/share/ojo/0.1.0/licenses/dep/LICENSE').is_file())
        self.assertEqual(self.run_installer().returncode, 0)
        self.assertFalse(list(self.root.glob('ojo-install.*')))
        self.assertFalse(list(binary.parent.glob('.ojo-install-*')))

    def test_corrupt_download_cannot_replace_existing_binary(self):
        self.archive()
        binary = self.root / 'prefix/bin/ojo'
        binary.parent.mkdir(parents=True)
        binary.write_bytes(b'synthetic existing installation')
        (self.fixture / (PACKAGE + '.tar.gz')).write_bytes(b'corrupt')
        self.assertNotEqual(self.run_installer().returncode, 0)
        self.assertEqual(binary.read_bytes(), b'synthetic existing installation')
        self.assertFalse(list(self.root.glob('ojo-install.*')))

    def test_links_are_refused_before_extracting(self):
        self.archive(unsafe=True)
        self.assertNotEqual(self.run_installer().returncode, 0)
        self.assertFalse((self.root / 'prefix/bin/ojo').exists())
        self.assertFalse((self.root / 'must-not-write').exists())

    def test_wrong_version_is_not_installed(self):
        self.archive(version='9.9.9')
        self.assertNotEqual(self.run_installer().returncode, 0)
        self.assertFalse((self.root / 'prefix/bin/ojo').exists())


if __name__ == '__main__':
    unittest.main()
