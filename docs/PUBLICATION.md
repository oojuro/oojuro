# Public source and release boundaries

## What is public

`oojuro/oojuro` contains the Apache-2.0 journal engine, recorder, CLI/MCP front doors, portable
SDKs and research harnesses. It runs locally and does not require an Oojuro Cloud account.
`oojuro/softaxiom` supplies the separate open-source personal-agent runtime that uses this journal.
Managed journal hosting/fleet operations and Soft Axiom's hosted account, billing, encryption,
Temporal composition and optional E2B/Kernel adapters belong to their private host repositories.
No customer journals, cloud credentials, provider accounts or private agent conversations belong here.

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

Ahikara owns native CI/CD and trusted publishing. Keep GitHub Actions disabled until public
contributions run on isolated, disposable runners without production secrets, credentials,
host sockets, trusted caches or access to the private cloud runner. Treat PR code as untrusted;
never execute it through `pull_request_target` or pass privileged artifacts/caches across trust
boundaries. Pin actions/dependencies, restrict token permissions and keep signing/publishing in
the reviewed main-branch release path. The maintainer may review and merge their own changes.

This native source repository has no hosted app to deploy. `vercel.json` disables automatic
Git deployments for every branch; do not run native builds on Vercel CPU. If a separate hosted
site is added, build on its approved CI runner and deploy reviewed prebuilt artifacts.
Source publication does not publish native binaries, enable a hosted feature, promote an E2B
image or establish staging/real-account acceptance. Preserve the exact build/source/toolchain,
notice inventory and smoke/checkpoint receipts before Ahikara promotes a native release.
