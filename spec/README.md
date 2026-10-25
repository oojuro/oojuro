# The .ojo specification

Specification version: **0.1.0-draft.1**. Status: **experimental draft**.
Wire version: **0.1**. This version is independently maintained from engine,
CLI, ojo-journal and ojo-cloud releases.

[0.1/FORMAT.md](0.1/FORMAT.md) is the complete normative definition of this
wire version. A reader needs this document and the named standard codec and
cryptographic algorithms, without the reference engine, a cloud account,
agent-specific adapters, external dictionaries or an unpublished schema.
[vectors.json](0.1/vectors.json) contains synthetic interoperability vectors.

The core format stores opaque byte keys and byte values. A value may contain an
OTLP message, a JSON record, a transcript fragment or a binary attachment; its
application meaning does not alter the file grammar. A trace profile must name
its own schema and preserve trace/span/parent IDs and original content. This
draft does not standardize a universal agent event schema.

The additive [Event profile 1](events/1/FORMAT.md) defines general journal
records consisting of engine acceptance time and exact data. It identifies its
own marker and grammar inside wire 0.1, without reinterpreting existing KV files.
Its [version](events/1/VERSION), [independent vectors](events/1/vectors.json) and
[Python reader](../examples/read_events.py) are separate from implementation
releases. Agent/session/provider conventions remain application data.

## Versioning

- `spec/VERSION` identifies the specification document release. Its patch or
  draft revision may clarify wording or add vectors without changing accepted
  bytes. A byte/semantic compatibility change requires a new wire version.
- `0.1/FORMAT.md` and its vectors remain available when a newer wire version is
  added in a separate directory. Readers identify compatibility from the file
  header, never an engine version, release tag or filename.
- Readers and writers reject unknown versions and required features. There is
  no implicit upgrade, destructive conversion or plaintext downgrade.
- Specification releases use `ojo-spec-v<spec-version>` tags; implementation
  releases use their own package versions and release schedule. Creating files
  in this repository does not publish either kind of release.
- Before a stable format release, require at least two independently implemented
  readers to pass the valid, invalid, encryption and recovery vectors. This
  draft is not a promise that future 0.x wire versions will be compatible.

## Conformance

A reader validates the header, each complete committed transaction, codecs,
encryption, bounds and operation grammar before exposing that transaction. It
reports any incomplete trailing transaction separately. A writable
implementation also follows the locking, durability, exact retry and recovery
rules. Resource limits must produce an explicit refusal, not partial success.
Search ranking, indexes, retention policy and agent capture are not conformance
requirements. Indexes are disposable and not required to recover originals.

The specification and vectors are Apache-2.0 under the included
[LICENSE](LICENSE). Implementations of the named standard algorithms retain their own
licenses. No proprietary engine or service is required to implement this format.
