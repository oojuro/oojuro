import importlib.util
import json
from pathlib import Path
import tempfile
import tarfile
import unittest
import os
import subprocess
import sys
import io

spec = importlib.util.spec_from_file_location("package_native", Path(__file__).with_name("package_native.py"))
package = importlib.util.module_from_spec(spec)
spec.loader.exec_module(package)


class Distribution(unittest.TestCase):
    def test_empty_archive_notices_are_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            archive = Path(directory) / "example-1.crate"
            with tarfile.open(archive, "w:gz") as target:
                member = tarfile.TarInfo("example-1/LICENSE")
                member.size = 3
                target.addfile(member, io.BytesIO(b" \n\t"))
            with self.assertRaises(ValueError):
                package.crate_notices(archive, package.sha(archive))

    def test_release_build_prepares_cache_before_building_or_packaging(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            tools, log = root / "tools", root / "calls.jsonl"
            tools.mkdir()
            helper = "#!" + sys.executable + "\n" + r"""
import json, os, sys
from pathlib import Path
name = Path(sys.argv[0]).name
if name == "git":
    sys.exit(0)
with open(os.environ['NATIVE_TEST_LOG'], 'a') as log:
    log.write(json.dumps([name, *sys.argv[1:]]) + '\n')
if name == 'cargo' and sys.argv[1] == 'fetch' and os.environ.get('NATIVE_TEST_FETCH_FAIL'):
    sys.exit(23)
"""
            for name in ["git", "cargo", "python3"]:
                file = tools / name
                file.write_text(helper)
                file.chmod(0o700)
            target = root / "target with spaces"
            env = {**os.environ, "PATH": str(tools) + os.pathsep + os.environ['PATH'],
                   "NATIVE_TEST_LOG": str(log), "CARGO_TARGET_DIR": str(target)}
            script = Path(__file__).with_name("build_native.sh")
            result = subprocess.run(["bash", str(script), str(root / "fresh output")], env=env, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            calls = [json.loads(line) for line in log.read_text().splitlines()]
            self.assertEqual([call[:2] for call in calls], [["cargo", "fetch"], ["cargo", "build"], ["python3", "scripts/package_native.py"]])
            self.assertIn("--locked", calls[0])
            self.assertEqual(calls[2][2], str(target / "x86_64-unknown-linux-gnu/release"))
            log.unlink()
            result = subprocess.run(["bash", str(script), str(root / "fresh output")], env={**env, "NATIVE_TEST_FETCH_FAIL": "1"}, capture_output=True, text=True)
            self.assertEqual(result.returncode, 23)
            self.assertEqual(len(log.read_text().splitlines()), 1)

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
            with self.assertRaises(ValueError):
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
            with self.assertRaises(ValueError):
                package.verify(out)
            (out / "NOTICE").unlink()
            with self.assertRaises(FileNotFoundError):
                package.verify(out)
            files = {"../escape": "fake"}
            (out / "manifest.json").write_text(json.dumps({"files": files}))
            with self.assertRaises(ValueError):
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
