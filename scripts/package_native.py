#!/usr/bin/env python3
"""Package reviewed native binaries with complete dependency notices, never a bare executable."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[1]


def run(*args):
    return subprocess.check_output(args, cwd=ROOT, text=True).strip()


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def legal_files(root):
    return sorted(p for p in root.rglob("*") if p.is_file() and
                  re.search(r"license|copying|copyright|notice|authors", p.name, re.I))


def put(src, dest):
    assert src.is_file() and src.stat().st_size, "missing notice: " + str(src)
    dest.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(src, dest)


def verify(out):
    manifest = json.loads((out / "manifest.json").read_text())
    for name, expected in manifest["files"].items():
        path = out / name
        assert path.resolve().is_relative_to(out.resolve()) and not path.is_symlink()
        assert sha(path) == expected, "package changed: " + name
    assert {"LICENSE", "NOTICE", "Cargo.lock", "bin/oojuro", "bin/oojurod"} <= manifest["files"].keys()


def package(binary_dir, out, target):
    assert not out.exists(), "output must be new"
    assert not run("git", "status", "--porcelain"), "commit reviewed inputs before packaging"
    metadata = json.loads(run("cargo", "metadata", "--locked", "--offline", "--format-version", "1", "--filter-platform", target))
    tree = run("cargo", "tree", "--locked", "--offline", "-p", "oojuro", "--edges", "normal,build", "--target", target, "--prefix", "none", "--format", "{p}")
    selected = set(re.findall(r"^([\w-]+) v([^\s]+)", tree, re.M))
    dependencies = []
    for p in sorted(metadata["packages"], key=lambda p: (p["name"], p["version"])):
        if (p["name"], p["version"]) not in selected or p["source"] is None:
            continue
        directory = Path(p["manifest_path"]).parent
        files = legal_files(directory)
        dest = out / "licenses" / (p["name"] + "-" + p["version"])
        if not files:
            # alloc-stdlib 0.2.4 omits the workspace BSD notice from its .crate.
            files = legal_files(ROOT / "licenses" / (p["name"] + "-" + p["version"]))
            assert files, "unreviewed package without notice: " + p["name"]
            for f in files:
                put(f, dest / f.name)
        else:
            for f in files:
                put(f, dest / f.relative_to(directory))
        allowed = {"0BSD", "MIT", "MIT-0", "Apache-2.0", "LLVM-exception", "CC0-1.0", "BSD-3-Clause", "ISC", "Unlicense", "Zlib", "Unicode-3.0", "CDLA-Permissive-2.0", "OR", "AND", "WITH"}
        assert p["license"] and set(re.findall(r"[\w.-]+", p["license"])) <= allowed, "unreviewed license: " + p["name"]
        dependencies.append({"name": p["name"], "version": p["version"], "license": p["license"], "repository": p["repository"]})
    for name in ["LICENSE", "NOTICE", "Cargo.lock"]:
        put(ROOT / name, out / name)
    shutil.copytree(ROOT / "licenses", out / "licenses" / "project-supplements")
    put(ROOT / "crates/oojuro-store/dict/README.md", out / "licenses/dictionary-provenance.md")
    rust = Path(run("rustc", "--print", "sysroot")) / "share/doc/rust"
    put(rust / "COPYRIGHT-library.html", out / "licenses/rust/COPYRIGHT-library.html")
    for f in sorted((rust / "licenses").iterdir()):
        put(f, out / "licenses/rust" / f.name)
    binaries = {}
    dictionary = (ROOT / "crates/oojuro-store/dict/starter-v1.zdict").read_bytes()
    for name in ["oojuro", "oojurod"]:
        src = binary_dir / name
        put(src, out / "bin" / name)
        (out / "bin" / name).chmod(0o755)
        binaries[name] = {"sha256": sha(src), "dictionaryIncluded": dictionary in src.read_bytes()}
    manifest = {
        "sourceCommit": run("git", "rev-parse", "HEAD"), "target": target,
        "rustc": run("rustc", "-vV"), "cargo": run("cargo", "--version"),
        "lockSha256": sha(ROOT / "Cargo.lock"), "binaries": binaries,
        "dependencies": dependencies,
        "files": {str(f.relative_to(out)): sha(f) for f in sorted(out.rglob("*")) if f.is_file()},
    }
    (out / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    verify(out)
    print(json.dumps({"dependencies": len(dependencies), "binaries": binaries}))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary_directory", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--target", default="x86_64-unknown-linux-gnu")
    args = parser.parse_args()
    package(args.binary_directory.resolve(), args.output.resolve(), args.target)
