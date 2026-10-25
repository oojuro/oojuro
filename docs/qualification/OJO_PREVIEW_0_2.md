# Ojo 0.2 public distribution qualification

The [experimental release](https://github.com/oojuro/oojuro/releases/tag/ojo-v0.2.0)
is an immutable annotated tag at source86ed2bd87a1296bb4e87cf1d402ffab12e214404,
contained in main after PR121. The independent wire/profile versions remain0.1
and Event profile1. The earlier0.1 tag and assets are retained unchanged.

The Linux x86-64 archive is1,372,516 bytes with SHA-256
`865c5411c95c46a892df14d32e07d8a7a8966ac8fea09537f68c471e2bdd3056`.
The installed binary SHA-256 is
`8728e8c664a095553c3af85b7d297be64efb2c2cb80514857913f1ab8ecb28ec`.
It requires glibc2.34 and includes all53 selected registry dependency notices,
Rust notices and dictionary attribution. Normal features are archive and search.

Release gates passed core default/archive tests, CLI normal/minimal integrations,
strict Clippy,20 actual fsync-EIO cases,5 installer tests, registry notice integrity
and normal dependency policy. The prepared real archive passed installation before
publication. Uploaded draft assets were independently downloaded and SHA-verified.

After publication, real unauthenticated HTTPS downloads verified the archive,
checksum list and versioned installer script. That public installer installed the
exact expected binary and notices. The installed program completed an encrypted
append with a deliberately broken acknowledgement pipe, scan/exact retained-intent
retry without duplicate or changed file bytes, raw/Zstd indexes and exact search
excerpts, exact cursor reads, byte-for-byte export at a legacy .ojo filename and
an encrypted compressed archive read. All synthetic source/index keys and datasets
were removed after terminal verification.

Homebrew's formula points to this exact public archive/checksum and tests a
non-agent Event append/index/search flow. Cargo installation uses the immutable
source tag. Their own installed-client results are recorded separately; neither
source checks nor this local distribution proof establish Cloud performance,
provisioning, restore, billing or a general storage advantage.

[Machine-readable public release evidence](ojo-preview-0.2-public-2026-10-25.json).
