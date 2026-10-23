#!/usr/bin/env python3
"""Package reviewed native binaries with complete dependency notices, never a bare executable."""
import argparse
import hashlib
import json
import io
import os
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import tomllib

ROOT = Path(__file__).resolve().parents[1]


def require(condition, message):
    if not condition:
        raise ValueError(message)


def run(*args):
    return subprocess.check_output(args, cwd=ROOT, text=True).strip()


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def legal_files(root):
    return sorted(p for p in root.rglob("*") if p.is_file() and
                  re.search(r"license|copying|copyright|notice|authors", p.name, re.I))


def verified_archive(archive, checksum):
    data = archive.read_bytes()
    require(hashlib.sha256(data).hexdigest() == checksum, "registry archive checksum mismatch")
    return tarfile.open(fileobj=io.BytesIO(data))


def crate_notices(archive, checksum):
    with verified_archive(archive, checksum) as contents:
        files = {str(Path(m.name).relative_to(archive.stem)): contents.extractfile(m).read()
                for m in contents.getmembers() if m.isfile() and
                re.search(r"license|copying|copyright|notice|authors", Path(m.name).name, re.I)}
    require(all(data.strip() for data in files.values()), "empty registry notice")
    return files


def selected_dependencies(target):
    tree = run("cargo", "tree", "--locked", "--offline", "-p", "oojuro", "--edges", "normal,build", "--target", target, "--prefix", "none", "--format", "{p}")
    selected = set(re.findall(r"^([\w-]+) v([^\s]+)", tree, re.M))
    require(selected, "empty production dependency graph")
    locked = tomllib.loads((ROOT / "Cargo.lock").read_text())["package"]
    registry = Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo")) / "registry/cache"
    dependencies = []
    for p in sorted(locked, key=lambda p: (p["name"], p["version"])):
        if (p["name"], p["version"]) not in selected or p.get("source") is None:
            continue
        require(p["source"] == "registry+https://github.com/rust-lang/crates.io-index", "unreviewed package source")
        archives = list(registry.glob("*/" + p["name"] + "-" + p["version"] + ".crate"))
        archive = next((a for a in archives if sha(a) == p.get("checksum")), None)
        require(archive is not None, "missing checksum-verified archive: " + p["name"])
        with verified_archive(archive, p["checksum"]) as contents:
            manifest = contents.getmember(archive.stem + "/Cargo.toml")
            require(manifest.isfile(), "missing crate manifest")
            metadata = tomllib.loads(contents.extractfile(manifest).read().decode())["package"]
        require(metadata["name"] == p["name"] and metadata["version"] == p["version"], "crate identity mismatch")
        dependencies.append({**p, "license": metadata.get("license"), "repository": metadata.get("repository"), "archive": archive})
    require(dependencies, "empty registry dependency inventory")
    return dependencies


def put(src, dest):
    require(src.is_file() and src.stat().st_size, "missing notice: " + str(src))
    dest.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(src, dest)


def verify(out):
    manifest = json.loads((out / "manifest.json").read_text())
    for name, expected in manifest["files"].items():
        path = out / name
        require(path.resolve().is_relative_to(out.resolve()) and not path.is_symlink(), "unsafe package path")
        require(path.stat().st_size > 0 and sha(path) == expected, "package changed: " + name)
    require({"LICENSE", "NOTICE", "Cargo.lock", "bin/oojuro", "bin/oojurod"} <= manifest["files"].keys(), "missing required package files")


def package(binary_dir, out, target):
    require(not out.exists(), "output must be new")
    require(not run("git", "status", "--porcelain"), "commit reviewed inputs before packaging")
    dependencies = []
    for p in selected_dependencies(target):
        archive, checksum = p["archive"], p["checksum"]
        files = crate_notices(archive, checksum)
        dest = out / "licenses" / (p["name"] + "-" + p["version"])
        if not files:
            # alloc-stdlib 0.2.4 omits the workspace BSD notice from its .crate.
            files = legal_files(ROOT / "licenses" / (p["name"] + "-" + p["version"]))
            require(files, "unreviewed package without notice: " + p["name"])
            for f in files:
                put(f, dest / f.name)
        else:
            for name, data in files.items():
                f = dest / name
                require(f.resolve().is_relative_to(dest.resolve()), "unsafe registry path")
                f.parent.mkdir(parents=True, exist_ok=True)
                f.write_bytes(data)
        allowed = {"0BSD", "MIT", "MIT-0", "Apache-2.0", "LLVM-exception", "CC0-1.0", "BSD-3-Clause", "ISC", "Unlicense", "Zlib", "Unicode-3.0", "CDLA-Permissive-2.0", "OR", "AND", "WITH"}
        require(p["license"] and set(re.findall(r"[\w.-]+", p["license"])) <= allowed, "unreviewed license: " + p["name"])
        dependencies.append({"name": p["name"], "version": p["version"], "license": p["license"], "repository": p["repository"], "registryChecksum": checksum})
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
