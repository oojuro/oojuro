# Store encryption

The optional `oojuro_store::Options::cipher` seals newly written frame bodies and
compression dictionaries with XChaCha20-Poly1305. Compression happens before
encryption. Each seal uses a fresh random 24-byte nonce and authenticates the
frame header or dictionary id and length. Frames and dictionaries have separate
authentication domains.

Callers supply a 32-byte key through `crypto::FrameCipher` and retain that key
outside the store. The `ENCRYPTED` marker stores a public key fingerprint, never
the key. An intact marker rejects missing or mismatched keys when opening.

## Scope and limits

- Text, record keys, attributes, and trained dictionary samples are encrypted.
- File names, sizes, frame headers, sequence ranges, and cold index timestamps
  remain visible. Manifest, index, and file checksums detect accidental corruption;
  they are not authenticated. This does not protect against directory rollback,
  file deletion, or an attacker rewriting the unencrypted index.
- Adding a key to an existing plaintext store seals future writes. Existing
  cold segments and dictionaries remain plaintext until explicitly rewritten.
  Hot data rewritten by compaction is sealed, but this is not secure erasure.
- This option applies to the low-level mutable `Store`. Immutable run builders
  and the higher-level capture/trace APIs do not yet expose encryption.
- This is not a completed raw session storage layer, per-writer key lifecycle,
  writer erasure, KMS integration, access control, or audit implementation.
- Plaintext exists in process memory and decoded caches. Deleting a key from one
  caller does not erase other copies, caches, backups, or previous plaintext files.

A valid outer CRC is not proof of authenticity: sealed bodies must also pass AEAD
verification. Authentication failures are errors; a forged hot body with a valid
CRC must not be treated as a recoverable torn tail or silently truncated.

## Verification

`cargo test -p oojuro-store` uses synthetic records to cover plaintext-compatible
upgrades, hot replay, cold compaction, deduplication, dictionary training and
reopening. Targeted regressions check side fields and range/time APIs, forged hot
and cold frames, forged dictionaries with repaired CRCs, malformed markers,
truncated seals, and cross-domain authentication failures. These tests do not
constitute an independent cryptographic audit or cloud acceptance test.
