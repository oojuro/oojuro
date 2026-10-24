#!/usr/bin/env python3
"""Check the private SDK's local npm payload and exported entry points offline."""
import json
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
SDK = ROOT / "sdk/typescript"


def run(*args, cwd):
    return subprocess.check_output(args, cwd=cwd, text=True)


with tempfile.TemporaryDirectory(prefix="oojuro-sdk-package-") as directory:
    scratch = Path(directory)
    packed = json.loads(run(
        "npm", "pack", "--offline", "--ignore-scripts", "--json",
        "--pack-destination", str(scratch), cwd=SDK,
    ))
    if len(packed) != 1:
        raise RuntimeError("expected exactly one local package")
    package = packed[0]
    archive = scratch / package["filename"]
    if archive.parent != scratch or not archive.is_file():
        raise RuntimeError("invalid local package archive")
    files = {entry["path"] for entry in package["files"]}
    for name in ["index", "capture", "files", "file-deletion", "attachments"]:
        if not {name + ".js", name + ".d.ts"} <= files:
            raise RuntimeError("missing runtime or declaration entry: " + name)
    if not {"file-download.js", "file-format.js"} <= files:
        raise RuntimeError("missing shared file implementation")
    install = scratch / "consumer"
    install.mkdir()
    (install / "package.json").write_text(json.dumps({"private": True, "type": "module"}))
    subprocess.run([
        "npm", "install", "--offline", "--ignore-scripts", "--no-audit", "--no-fund",
        str(archive),
    ], cwd=install, check=True)
    (install / "smoke.mjs").write_text("""
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { OojuroClient, OojuroCaptureClient, OojuroProjectClient, captureSession } from '@oojuro/v2-client';
import { CaptureSpool } from '@oojuro/v2-client/capture';
import { prepareFileUpload, downloadFileTo } from '@oojuro/v2-client/files';
import { prepareFileDeletion } from '@oojuro/v2-client/file-deletion';
import { prepareJournalAttachment, downloadJournalAttachmentTo } from '@oojuro/v2-client/attachments';
for (const exported of [OojuroClient, OojuroCaptureClient, OojuroProjectClient, captureSession,
  CaptureSpool, prepareFileUpload, downloadFileTo, prepareFileDeletion,
  prepareJournalAttachment, downloadJournalAttachmentTo]) assert.equal(typeof exported, 'function');
const manifest = JSON.parse(await readFile(new URL('./node_modules/@oojuro/v2-client/package.json', import.meta.url)));
assert.equal(manifest.private, true);
assert.equal(Object.keys(manifest.dependencies ?? {}).length, 0);
console.log('Installed offline SDK exports passed; no publication or service calls.');
""")
    subprocess.run(["node", "smoke.mjs"], cwd=install, check=True)
