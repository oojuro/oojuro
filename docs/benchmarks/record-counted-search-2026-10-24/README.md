# Native Record counted-output qualification evidence

The runtime change and source tests are in this topic. Human-readable results and limits: [RECORD_COUNTED_SEARCH.md](../../RECORD_COUNTED_SEARCH.md). `summary.json` records every measured arm. `manifest.json` records original local evidence hashes/sizes, published derivative hashes/sizes and transformations.

`evidence.tar.gz` contains72 flat regular UTF-8 files: build/source/toolchain/artifact receipts and controllers; six distinct focused checks across two libtests; original failed rank assertion; all four context controller sources/receipts; three earlier stopped/failed attempts and independent cleanup; eight-arm result/inventory/independent proof. No ELF, target, fixture store, key bytes, config, original/search payload, dataset, PGDATA or private hosted source is included. Synthetic key fingerprints in inventory metadata are withheld. The separate future borrowed-text proposal is excluded.

## Redaction and verification

Workspace/home paths become `<WORKSPACE>`/`<HOME>`; synthetic fixture paths become `<FIXTURE>`. These are evidence copies, not directly executable with those placeholders: rebind task paths and canonical compiler wrappers deliberately. Original source hashes in the manifest identify reviewed executable controllers. Cargo JSON metadata is omitted from test stdout because the receipts already pin artifacts; terminal actual test/failure output is retained.

The archive is deterministic USTAR+gzip: relative flat regular paths, mode0644, uid/gid/mtime0, empty owner names/gzip filename. The archive and every decompressed member were rehashed; decompression round-trip matched exact tar bytes. Maximum individual payload size is below200,000B. No extraction is needed to inspect the small JSON/README files. Verify safely before extracting:

```python
import gzip, hashlib, io, json, pathlib, tarfile
root = pathlib.Path(__file__).resolve().parent
m = json.loads((root / "manifest.json").read_text())
wire = (root / m["archive"]["path"]).read_bytes()
assert hashlib.sha256(wire).hexdigest() == m["archive"]["sha256"]
plain = gzip.decompress(wire)
assert hashlib.sha256(plain).hexdigest() == m["archive"]["decompressed_tar_sha256"]
with tarfile.open(fileobj=io.BytesIO(plain), mode="r:") as t:
    members = t.getmembers()
    assert len(members) == len(m["files"])
    for x, row in zip(members, m["files"]):
        assert x.isfile() and x.name == row["path"] and "/" not in x.name
        assert x.uid == x.gid == x.mtime == 0 and x.mode == 0o644
        assert hashlib.sha256(t.extractfile(x).read()).hexdigest() == row["published_sha256"]
```

This is an opt0 allocation diagnostic on synthetic escaped originals and a small context. It is not a release benchmark, hosted/cost qualification, cold-OS result or general search memory claim. Earlier unsuccessful attempts remain distinct; no failed worker is retroactively marked passed.
