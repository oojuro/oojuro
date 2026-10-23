import importlib.util
import json
from pathlib import Path
import tempfile
import tarfile
import unittest
from unittest.mock import patch
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

    def test_selected_graph_packages_without_unused_workspace_archives(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            cache = root / "cargo/registry/cache/synthetic"
            cache.mkdir(parents=True)
            archive = cache / "example-1.0.0.crate"
            with tarfile.open(archive, "w:gz") as target:
                for name, data in {
                    "LICENSE": b"Synthetic upstream license",
                    "Cargo.toml": b'[package]\nname="example"\nversion="1.0.0"\nlicense="MIT"\n',
                }.items():
                    member = tarfile.TarInfo("example-1.0.0/" + name)
                    member.size = len(data)
                    target.addfile(member, io.BytesIO(data))
            for name in ["LICENSE", "NOTICE", "licenses/project/LICENSE", "crates/oojuro-store/dict/README.md", "rust/share/doc/rust/COPYRIGHT-library.html", "rust/share/doc/rust/licenses/MIT", "binaries/oojuro", "binaries/oojurod"]:
                file = root / name
                file.parent.mkdir(parents=True, exist_ok=True)
                file.write_text("Synthetic legal or binary content")
            (root / "crates/oojuro-store/dict/starter-v1.zdict").write_bytes(b"synthetic dictionary")
            (root / "Cargo.lock").write_text(f'''[[package]]
name = "oojuro"
version = "0.1.0"
[[package]]
name = "example"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "{package.sha(archive)}"
[[package]]
name = "unused-benchmark"
version = "9.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "{'a' * 64}"
''')
            def run(*args):
                if args[:2] == ("cargo", "metadata"):
                    raise FileNotFoundError("unused benchmark archive was never downloaded")
                if args[:2] == ("cargo", "tree"):
                    return "oojuro v0.1.0\nexample v1.0.0"
                if args == ("git", "status", "--porcelain"):
                    return ""
                if args == ("git", "rev-parse", "HEAD"):
                    return "b" * 40
                if args == ("rustc", "--print", "sysroot"):
                    return str(root / "rust")
                if args in [("rustc", "-vV"), ("cargo", "--version")]:
                    return "synthetic toolchain"
                raise ValueError("unexpected command")
            with patch.object(package, "ROOT", root), patch.object(package, "run", run), patch.dict("os.environ", {"CARGO_HOME": str(root / "cargo")}):
                package.package(root / "binaries", root / "bundle", "x86_64-unknown-linux-gnu")
            manifest = json.loads((root / "bundle/manifest.json").read_text())
            self.assertEqual([d["name"] for d in manifest["dependencies"]], ["example"])

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
