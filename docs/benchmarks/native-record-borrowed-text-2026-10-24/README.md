# Borrowed-text ownership; actual CLI peak-RSS result neutral

`summary.json` retains all measured arms. `manifest.json` pins each original local source file hash/size, each published derivative hash/size and transformation. `evidence.tar.gz` contains 121 flat regular UTF-8 files (86,585 compressed bytes). It preserves raw per-invocation metrics, failures where applicable, source/controllers, toolchain/artifact and actual test/cleanup proofs. It includes no ELF, target, key, fixture/config/store, corpus, original/search/context payload stdout or private hosted source.

Workspace/home and generated fixture paths use `<WORKSPACE>`/`<HOME>`/`<FIXTURE>` placeholders; these evidence copies require deliberately rebinding local paths before execution. Synthetic key-file inventory fingerprints are withheld. Cargo metadata JSON is removed from test stdout; terminal actual test/failure lines are retained. Long flat filenames are shortened with a source-relative-path hash; the manifest preserves their original relative identity.

Deterministic USTAR+gzip: flat relative regular entries, mode0644, uid/gid/mtime0, empty owner names. Every member and original source was rehashed; gzip decompression exactly round-trips to the recorded tar hash. Every individual derivative is under200,000B. Inspect without extraction:

```python
import gzip, hashlib, io, json, pathlib, tarfile
root = pathlib.Path(__file__).resolve().parent
m = json.loads((root / "manifest.json").read_text())
wire = (root / "evidence.tar.gz").read_bytes()
assert hashlib.sha256(wire).hexdigest() == m["archive"]["sha256"]
plain = gzip.decompress(wire)
assert hashlib.sha256(plain).hexdigest() == m["archive"]["decompressed_tar_sha256"]
with tarfile.open(fileobj=io.BytesIO(plain), mode="r:") as t:
    entries = t.getmembers()
    assert len(entries) == len(m["files"])
    for x, row in zip(entries, m["files"]):
        assert x.isfile() and "/" not in x.name and x.name == row["path"]
        assert x.mode == 0o644 and x.uid == x.gid == x.mtime == 0
        assert hashlib.sha256(t.extractfile(x).read()).hexdigest() == row["published_sha256"]
```

Results are synthetic warm opt0 allocation diagnostics, not release-speed, hosted/cost, cold-OS, larger-than-RAM or general performance claims. Failed/uncertain workers remain distinct. No additional workload or favorable rerun is authorized.

The pinned shared inventory controller is included for its imported inventory/save/identity helpers. Its unrelated PostgreSQL workload main is not invoked by either native allocation campaign.
