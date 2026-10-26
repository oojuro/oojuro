# Public source and release boundaries

## What is public

`oojuro/oojuro` contains the Apache-2.0 embedded database engine and its
storage/retrieval compatibility tests and research harnesses. Agent-specific
recorder, capture, application CLI/MCP and portable client SDK source belongs to
`oojuro/o-journal`. Both run locally without an Oojuro Cloud account.

Managed Oojuro hosting and the `oojuro.com` website belong to private
`oojuro/o-cloud`. Trusted release coordination belongs to private
`oojuro/o-release-operator`. Soft Axiom's personal-agent runtime is independently
owned in `oojuro/softaxiom`, with private hosting and release operations in its
own repositories. No customer journals, cloud credentials, provider accounts or
private agent conversations belong in public source. See
[repository boundaries](REPOSITORY_BOUNDARIES.md).

Original source code stays Apache-2.0. Third-party content is an exception to that code license,
not an exception to attribution: preserve root NOTICE, licenses/ and the dictionary README.
The starter dictionary is transformed material under its listed upstream grants, including
Nebius attribution and CC BY 4.0 terms. Its immutable bytes are needed to read existing journals.
Do not replace them as a licensing shortcut. Dataset manifests and split IDs are not a license
to redistribute the referenced datasets; research-only corpora are excluded from runtime builds.
Native binary notices cover the selected locked normal/build graph, not every benchmark feature
or a whole operating-system/container image. See [NATIVE_DISTRIBUTION.md](NATIVE_DISTRIBUTION.md).

## Publication review, 2026-10-23

The initial source review covers main `47dfcd0897f1d1a3bf298980b46fa7a05760f41a`, a full mirror
with 423 reachable commits, 84 branch heads and 119 PR refs, including deleted/merged-only blobs.
Gitleaks 8.30.1 scanned Git history and all 1,835 unique blobs (32,051,044 bytes); the release
tool archive was SHA-256 verified. Eight historical candidates and eleven blob candidates were
triaged as synthetic redaction tests or public benchmark instance IDs. The test PEM is assembled
from generated text; compatibility stores have deterministic synthetic generators and expected
checksums. No production credential or private transcript was found in the reviewed source.

Issue/PR descriptions and issue/review comments were included in the publication review.
Raw authenticated API transport metadata and local audit workspaces are not publication artifacts.
No history rewrite or removal of existing research branches was required by this review.
Refresh the full ref inventory and scan new objects before changing visibility; this receipt
does not cover future commits. Do not probe a suspected credential against a live service.

## CI/CD and releases

Ahikara owns native CI/CD and trusted publishing. The `.github/workflows/ci.yml` checks use only disposable GitHub-hosted Ubuntu runners, a read-only repository token and pinned checkout/toolchain versions. Checkout does not persist credentials. PR jobs receive no production secrets, private runner access, reusable caches, signing material or release artifacts. There is no `pull_request_target`, self-hosted runner, release trigger or publishing job. Treat all PR code as untrusted, including dependency build scripts. Do not move these jobs onto a private cloud runner or restore privileged artifacts/caches into them.

Repository Actions is enabled for these unprivileged checks. Inspect the actual run for a commit; adding workflow files alone does not establish a passing check. Maintainers may review their own changes; do not require a second reviewer if that would lock out the sole maintainer. Choose required status checks only after their exact job names have run successfully. This change does not configure administrative rules. Signing, package/native releases and deployment still require the owner's approval and reviewed release evidence.

This native source repository has no hosted app to deploy. `vercel.json` disables automatic
Git deployments for every branch; do not run native builds on Vercel CPU. If a separate hosted
site is added, build on its approved CI runner and deploy reviewed prebuilt artifacts.
Source publication does not publish native binaries, enable a hosted feature, promote an E2B
image or establish staging/real-account acceptance. Preserve the exact build/source/toolchain,
notice inventory and smoke/checkpoint receipts before Ahikara promotes a native release.
