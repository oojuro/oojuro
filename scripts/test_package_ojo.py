#!/usr/bin/env python3
"""Check registry notice integrity independently of the live Cargo cache."""
import hashlib
import io
from pathlib import Path
import tarfile
import tempfile
import unittest

from package_ojo import registry_inputs


class RegistryNotices(unittest.TestCase):
    def archive(self, root, notice='LICENSE'):
        path = Path(root) / 'synthetic-1.0.0.crate'
        with tarfile.open(path, 'w:gz') as archive:
            for name, data in [('Cargo.toml', b'[package]\nname="synthetic"\nversion="1.0.0"\nlicense="MIT"\n'),
                               (notice, b'synthetic license text')]:
                member = tarfile.TarInfo('synthetic-1.0.0/' + name)
                member.size = len(data)
                archive.addfile(member, io.BytesIO(data))
        return path, hashlib.sha256(path.read_bytes()).hexdigest()

    def test_metadata_and_notices_come_from_verified_bytes(self):
        with tempfile.TemporaryDirectory() as root:
            path, checksum = self.archive(root)
            metadata, files = registry_inputs(path, checksum)
            self.assertEqual(metadata['name'], 'synthetic')
            self.assertEqual(files, {'LICENSE': b'synthetic license text'})
            path.write_bytes(b'changed registry archive')
            with self.assertRaisesRegex(ValueError, 'checksum'):
                registry_inputs(path, checksum)

    def test_notice_cannot_escape_its_package_directory(self):
        with tempfile.TemporaryDirectory() as root:
            path, checksum = self.archive(root, '../outside/LICENSE')
            with self.assertRaisesRegex(ValueError, 'unsafe registry notice'):
                registry_inputs(path, checksum)


if __name__ == '__main__':
    unittest.main()
