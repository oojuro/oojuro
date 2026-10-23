import importlib.util
import json
from pathlib import Path
import tempfile
import tarfile
import unittest

spec = importlib.util.spec_from_file_location("package_native", Path(__file__).with_name("package_native.py"))
package = importlib.util.module_from_spec(spec)
spec.loader.exec_module(package)


class Distribution(unittest.TestCase):
    def test_registry_archive_is_verified_against_lock_checksum(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            notice = root / "LICENSE"
            notice.write_text("synthetic upstream copyright")
            archive = root / "example-1.crate"
            with tarfile.open(archive, "w:gz") as target:
                target.add(notice, arcname="example-1/LICENSE")
            checksum = package.sha(archive)
            self.assertEqual(package.crate_notices(archive, checksum), {"LICENSE": notice.read_bytes()})
            archive.write_bytes(archive.read_bytes() + b"corrupt")
            with self.assertRaises(AssertionError):
                package.crate_notices(archive, checksum)

    def test_notice_verifier_rejects_missing_changed_and_escaping_paths(self):
        with tempfile.TemporaryDirectory() as directory:
            out = Path(directory)
            files = {}
            for name in ["LICENSE", "NOTICE", "Cargo.lock", "bin/oojuro", "bin/oojurod", "licenses/nested/NOTICE"]:
                dest = out / name
                dest.parent.mkdir(parents=True, exist_ok=True)
                dest.write_text("synthetic " + name)
                files[name] = package.sha(dest)
            (out / "manifest.json").write_text(json.dumps({"files": files}))
            package.verify(out)
            (out / "NOTICE").write_text("deleted attribution")
            with self.assertRaises(AssertionError):
                package.verify(out)
            (out / "NOTICE").unlink()
            with self.assertRaises(FileNotFoundError):
                package.verify(out)
            files = {"../escape": "fake"}
            (out / "manifest.json").write_text(json.dumps({"files": files}))
            with self.assertRaises(AssertionError):
                package.verify(out)

    def test_nested_legal_files_are_included(self):
        with tempfile.TemporaryDirectory() as directory:
            out = Path(directory)
            for name in ["LICENSE", "vendor/NOTICE", "vendor/COPYING", "vendor/code.c"]:
                dest = out / name
                dest.parent.mkdir(parents=True, exist_ok=True)
                dest.write_text("synthetic")
            self.assertEqual({str(p.relative_to(out)) for p in package.legal_files(out)}, {"LICENSE", "vendor/NOTICE", "vendor/COPYING"})


if __name__ == "__main__":
    unittest.main()
