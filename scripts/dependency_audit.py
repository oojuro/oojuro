#!/usr/bin/env python3
"""Audit selected Cargo build graphs; Cargo.lock alone includes unused experiments.

Uses only Python's standard library and the installed Cargo. Counts distinct
name/version pairs, includes build dependencies, excludes tests and dev deps.
"""
import argparse
import json
import re
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
TARGET = "x86_64-unknown-linux-gnu"
WORKSPACE_NAMES = set()
SCOPES = {
    "default": [],
    "rank": ["-p", "oojuro-rank"],
    "store": ["-p", "oojuro-store"],
    "engine": ["-p", "oojuro-engine"],
    "legacy-core": ["-p", "oojuro-core"],
    "legacy-nine": ["-p", "oojuro-nine"],
    "legacy-trace": ["-p", "oojuro-trace"],
    "capture": ["-p", "oojuro-capture"],
    "proxy": ["-p", "oojuro-proxy"],
    "cli": ["-p", "oojuro"],
    "bench": ["-p", "oojuro-bench"],
    "tracebench": ["-p", "oojuro-tracebench"],
    "store-tools": ["-p", "oojuro-store", "--features", "baselines,codecs"],
}


def cargo(*args):
    return subprocess.check_output(
        ["cargo", *args, "--manifest-path", str(ROOT / "Cargo.toml")],
        text=True, cwd=ROOT,
    )


def graph(selection, target, edge_kinds="normal,build"):
    lines = cargo("tree", "--locked", "--edges", edge_kinds,
                  "--target", target, "--prefix", "depth", "--no-dedupe",
                  "--format", "{p}|{f}", *selection)
    packages, edges, stack, roots = {}, set(), [], set()
    for line in lines.splitlines():
        if not line:
            continue
        match = re.fullmatch(r"(\d+)(\S+) v([^ |]+)(?: \(([^|]+)\))?\|(.*)", line)
        if not match:
            raise ValueError(f"unexpected Cargo tree line: {line!r}")
        depth, name, version, source_detail, features = match.groups()
        depth, key = int(depth), f"{name}@{version}"
        # Cargo can compile two equal-version packages from distinct Git sources.
        git_source = source_detail if source_detail and "://" in source_detail else None
        if git_source:
            key += "#" + git_source
        packages.setdefault(key, {"name": name, "version": version,
                                  "git_source": git_source,
                                  "workspace": name in WORKSPACE_NAMES and not git_source,
                                  "features": set(), "used_by": set()})
        packages[key]["features"].update(filter(None, features.split(",")))
        if depth:
            parent = stack[depth - 1]
            edges.add((parent, key))
            packages[key]["used_by"].add(parent)
        else:
            roots.add(key)
        stack[depth:] = [key]
    for entry in packages.values():
        entry["features"] = sorted(entry["features"])
        entry["used_by"] = sorted(entry["used_by"])
    external = [key for key, entry in packages.items()
                if not entry["workspace"]]
    return {"selection": selection, "target": target,
            "edges": edge_kinds, "dev_dependencies": "dev" in edge_kinds.split(","),
            "roots": sorted(roots), "external_count": len(external),
            "package_count": len(packages), "workspace_count": len(packages) - len(external), "edge_count": len(edges),
            "packages": dict(sorted(packages.items()))}


def check(report):
    policy = json.loads((ROOT / "dependency-policy.json").read_text())
    allow = set(policy["engine_allowlist"])
    direct = set(policy["direct_foundations"]) | set(policy["algorithm_exceptions"])
    internal = {"oojuro-engine", "oojuro-rank", "oojuro-store"}
    for scope in ("default", "rank", "store", "engine", "engine-all-platforms"):
        for package in report["graphs"][scope]["packages"].values():
            name = package["name"]
            if name not in internal and name not in allow:
                raise ValueError(f"{scope}: dependency outside engine allowlist: {name}")
            if name not in internal and any(parent.partition("@")[0] in internal
                                            for parent in package["used_by"]):
                if name not in direct:
                    raise ValueError(f"{scope}: unreviewed direct engine dependency: {name}")
        for name in ("lz4_flex",):
            entries = [p for p in report["graphs"][scope]["packages"].values()
                       if p["name"] == name]
            required = {"std", "safe-encode", "safe-decode", "checked-decode"}
            if any("frame" in p["features"] or not required <= set(p["features"])
                   for p in entries):
                raise ValueError(f"{scope}: require safe, checked, block-only LZ4 features")
    store = report["graphs"]["store"]["packages"]
    if any(p["name"] == "serde_json" for p in store.values()):
        raise ValueError("store library includes benchmark JSON dependency")
    expected = {"oojuro-engine", "oojuro-rank", "oojuro-store"}
    roots = {key.partition("@")[0] for key in report["graphs"]["default"]["roots"]}
    if roots != expected:
        raise ValueError("default build must select only engine, rank and store")


def main():
    global ROOT, WORKSPACE_NAMES
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument("--scope", action="append", choices=SCOPES)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--edge-breakdown", action="store_true", help="Record normal-only and dev-inclusive comparison closures separately")
    parser.add_argument("--engine-only", action="store_true", help="Audit only engine/rank/store and all-platform engine; suitable for the policy CI gate")
    args = parser.parse_args()
    if args.scope and (args.check or args.engine_only):
        parser.error("--check/--engine-only require the standard policy scopes; omit --scope")
    scopes = ["default", "rank", "store", "engine"] if args.engine_only else (args.scope or SCOPES)
    ROOT = args.root.resolve()
    metadata_args = ["metadata", "--locked", "--format-version", "1"]
    if args.engine_only:
        metadata_args.append("--no-deps")
    elif "store-tools" in scopes:
        metadata_args += ["--features", "oojuro-store/baselines,oojuro-store/codecs"]
    # Inventory the actual selected tool features and all-platform engine graph;
    # --all-features would incorrectly include unrelated optional benchmark arms.
    metadata = json.loads(cargo(*metadata_args))
    workspace_ids = set(metadata["workspace_members"])
    WORKSPACE_NAMES = {p["name"] for p in metadata["packages"] if p["id"] in workspace_ids}
    report = {"format": 1, "target": TARGET,
              "rustc": subprocess.check_output(["rustc", "--version"], text=True).strip(),
              "cargo": subprocess.check_output(["cargo", "--version"], text=True).strip(),
              "revision": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT,
                                                   text=True).strip(),
              "dirty": bool(subprocess.check_output(["git", "status", "--porcelain"],
                                                     cwd=ROOT, text=True)),
              "graphs": {name: graph(SCOPES[name], TARGET)
                         for name in scopes}}
    if not args.scope or "engine" in args.scope:
        report["graphs"]["engine-all-platforms"] = graph(SCOPES["engine"], "all")
    if args.edge_breakdown:
        report["comparison_graphs"] = {
            name: {"normal-only": graph(SCOPES[name], TARGET, "normal"),
                   "dev-inclusive": graph(SCOPES[name], TARGET, "normal,build,dev")}
            for name in scopes
        }
    used = {key for graph_ in report["graphs"].values() for key in graph_["packages"]}
    report["inventory"] = {}
    engine_keys = report["graphs"].get("engine-all-platforms", {}).get("packages", {})
    application_keys = {
        key for scope, graph_ in report["graphs"].items()
        if scope in {"legacy-core", "legacy-nine", "legacy-trace", "capture", "proxy", "cli"}
        for key in graph_["packages"]
    }
    for package in metadata["packages"]:
        key = f"{package['name']}@{package['version']}"
        if key not in used or package["id"] in workspace_ids:
            continue
        report["inventory"][key] = {
            "license": package["license"], "repository": package["repository"],
            "description": package["description"],
            "source": package["source"], "native_links": package["links"],
            "build_script": any("custom-build" in t["kind"] for t in package["targets"]),
            "scopes": sorted(name for name, graph_ in report["graphs"].items()
                             if key in graph_["packages"]),
            "decision": ("retain: foundational implementation or its transitive support"
                         if key in engine_keys else
                         "retain outside minimal engine: CLI, legacy API or integration"
                         if key in application_keys else
                         "retain only in explicitly selected benchmark/experimental tools"
                         if not args.scope else "caller-selected graph; see scope-specific audit"),
        }
    external_keys = {key for graph_ in report["graphs"].values()
                     for key, package in graph_["packages"].items() if not package["workspace"]}
    missing_inventory = sorted(external_keys - set(report["inventory"]))
    report["inventory_coverage"] = {
        "mode": "policy-only; --no-deps metadata intentionally omits external inventory" if args.engine_only else "selected external package metadata",
        "selected_external_packages": len(external_keys),
        "inventoried_packages": len(report["inventory"]),
        "omitted_packages": missing_inventory,
    }
    if missing_inventory and not args.engine_only:
        raise ValueError(f"selected dependencies missing metadata inventory: {missing_inventory}")
    if args.check:
        check(report)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    for name, graph_ in report["graphs"].items():
        print(f"{name}: {graph_['external_count']} external packages, {graph_['edge_count']} edges")


if __name__ == "__main__":
    main()
