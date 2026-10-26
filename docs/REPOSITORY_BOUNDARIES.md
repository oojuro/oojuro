# Repository boundaries

The engine has one purpose: durable, compressed, searchable storage. The
agent-journal application and managed service are separate consumers.
Managed hosting and Cloud development are suspended; open source local usage
does not require a hosted account. The company's focus is Soft Axiom.

The canonical new database filename extension is `.juro`; existing `.ojo`
files retain their paths and remain readable. This is a filename convention,
not a repository/product/CLI rename or a wire/profile change.

| Owner | Source responsibility |
| --- | --- |
| `oojuro/oojuro` | Engine, rank, store, legacy compatibility libraries and storage/retrieval benchmarks |
| `oojuro/o-journal` | `oojuro`, `oojuro-capture`, `oojuro-proxy` application crates; agent setup scripts, application packaging and client SDKs |
| `oojuro/o-cloud` | Hosted account/project policy, sharing, billing, infrastructure and the `oojuro.com` website |
| `oojuro/o-release-operator` | Trusted release handoff, signing/publishing/promotion coordination and rollback |

Soft Axiom is a separate personal-agent runtime (`oojuro/softaxiom`) with private
hosting (`oojuro/sa-cloud`) and its own trusted operator (`oojuro/sa-release-operator`).

## Migration contract

Journal's initial extraction comes from public source
`d1b24f7ed020365e09431aeaa8f4a7ff05e27dc3`. Its source provenance is recorded
[in Journal](https://github.com/oojuro/o-journal/blob/main/PROVENANCE.md), and its
engine dependencies are pinned to that immutable revision. Rust application and
SDK behavior is preserved; workspace paths, dependency locations, packaging
notice discovery and documentation links change.

Existing engine revisions continue to contain their original application
crates. Consumers pinned to those commits still build from the same source.
Sibling-checkout consumers must adopt the separate Journal checkout before
using an engine revision that removes the application. Existing installer and
cloud pins are not silently advanced by this reorganization.

The native engine/rank/store implementations, legacy storage implementations
and on-disk field names are preserved. The independent `crates/ojo` library adds
experimental standalone wire 0.1 files; it does not convert old stores or claim
a stable-format guarantee. Session finish/idle/archive scheduling remains in
Journal/cloud; the engine exposes the generic archive operation only.

Build/test logic stays with its source component. Operator coordination is
separate from application code and does not grant publication or deployment
authority to a source PR. Public source never contains private service code or
customer/agent history.
