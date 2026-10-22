# Oojuro engine: sealed raw journal

This is the first working slice of the consolidated engine proposed in the foundation specification.
It retains original source bytes locally, independently of parsed/redacted records and lexical indexes.
It does not replace the current recorder or implement the rest of the proposed engine.

`raw::RawJournal` reuses the existing Store journal, group/framing recovery and compression/sealing code.
It writes source registration and byte chunks as encrypted records with `sync = true`.
A cursor is reconstructed from those exact committed records; there is no separately updated cursor file.
A replay after a lost response compares the already retained prefix byte for byte and appends only the new suffix.
Gaps, changed prefixes, source-identity reuse with another descriptor, wrong keys and plaintext downgrade fail.

## Caller contract

- Use an already existing parent and one dedicated journal directory and independently supplied key per writer/trust boundary. Store's owning-process lock prevents two owners.
- Apply recording policy and authorization BEFORE calling this library. It does not open source paths, find agent logs, infer permission, or forward bytes.
- Assign a stable opaque `SourceId` to a source generation. Rotation, replacement or truncation requires a new identity. An adapter must verify source generation and length before resuming; matching a prefix cannot prove the file was not truncated.
- Supply a bounded opaque descriptor (for example an encoded original relative path/mode/harness id). Descriptors and bytes are sealed; no plaintext path/cursor sidecars exist.
- Split inputs into blocks of at most 256 KiB. Each call acknowledges its own durable cursor, rather than claiming that several calls are atomic.
- If append or compaction fails, close and reopen. Recovery may discover a committed operation whose response was lost; retry using the original source offset and bytes.
- Restore streams bytes to a caller-owned sink. The caller chooses paths and must validate any decoded original path before creating a file. The library never materializes a path from an untrusted descriptor.

The descriptor is immutable. Empty sources are registered explicitly. Byte blocks preserve CRLF, invalid UTF-8, NUL, partial tails and final-newline absence without a text conversion.
No raw upload is enabled: cloud forwarding remains parsed/redacted according to the existing product policy.
Metadata such as compressed lengths, frame counts and public key identifiers remains visible. This is not a tamper-evident audit log:
AEAD rejects modified/downgraded frame bodies, but trusted external checkpoints are still needed to detect rollback or suffix truncation.
Keys are not persisted here and their secure lifecycle, backup/revocation and in-memory zeroization remain caller/integration requirements.

## Verification

```sh
cargo test -p oojuro-engine --locked
cargo test -p oojuro-store --test encrypted --locked
```

Synthetic fixtures cover arbitrary-byte round trips/reopen/compaction, resume across chunk boundaries, exact on-disk plaintext scans,
changed source/gap/identity/key rejection, torn append, commit-before-cursor recovery, sink failure, and real process SIGKILL after acknowledgement.
Store regressions exercise strict plaintext-upgrade/downgrade refusal. The ignored `raw_process_child` test is a subprocess fixture invoked by the SIGKILL test.

This proves local behavior on the development filesystem. Host power, volume/AZ loss, replicated acknowledgement and deployment acceptance
are separate hosted-service gates. No user transcripts are included. Storage/latency overhead and source-count scaling remain unmeasured.

## Next implementation gates

1. Connect capture policy and source-generation detection to this raw sink; prove raw/parsed batch visibility and cursor recovery together.
2. Port the flat record, store/index and lexical query path, retaining reference ids/ties for differential checks.
3. Integrate optional ranker after access/time filters, with receipts and erasure-safe table ownership.
4. Complete writer erasure, key lifecycle and externally witnessed audit.
5. Finish adapters, CLI/MCP and owner/authorized-agent/human review/export surfaces.
6. Add hosted replicated commit authority and fault tests; a local fsync cannot provide the hosted host/AZ durability target.
