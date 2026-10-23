# CI and builds

Owner policy, October 22, 2026: Oojuro follows the same build and release rules as Soft Axiom
and the private cloud repositories. Ahikara owns CI/CD. The owner may review and merge their
own PRs; a second reviewer is optional. Build compute runs on our infrastructure, not Vercel.

## Compute and trust boundaries

| Work | Where and how |
| --- | --- |
| Trusted private builds and checks | Rocket; serialize heavy work under `/tmp/sa11-verification.lock` |
| Public contribution checks | Fresh disposable isolated VM per job, with read-only contents access and no service credentials |
| Releases and publishing | Separate trusted job for a reviewed exact commit, with narrowly scoped publishing credentials |
| Documentation | Build static artifacts in CI; publish to GitHub Pages or the chosen static host |
| A Vercel-hosted site | Build on our compute; upload prebuilt output only |

This repository currently has no CI workflows, repository runners or Vercel project. This policy
does not claim that an isolated runner service or release pipeline has been provisioned. Keep
the repository private until its publication review and contribution infrastructure are ready.

Before public checks are enabled, Ahikara must prove disposable VM isolation: no Rocket host
mounts, Docker socket, private network access, cloud metadata, service credentials or shared
writable caches. Destroy the VM and disks after each job; runner deregistration alone is
insufficient. Never attach persistent private Rocket runners to a public repository. If isolated
compute is unavailable, leave those jobs disabled until an approved ephemeral service is ready.

Use SHA-pinned actions, read-only contents permissions and checkout with
`persist-credentials: false`. Never execute untrusted PR code through `pull_request_target`,
credential-bearing release jobs or a workflow that reuses private caches. A fork condition in
a PR-editable workflow is not a security boundary. Paid provider checks and private datasets
belong in a separate trusted, capped lane. Ordinary checks use synthetic fixtures.

## Checks and release artifacts

Use the existing [contribution commands](../CONTRIBUTING.md#build-and-test) with the checked-in
lockfile: `cargo build --workspace --locked`, `cargo test --workspace --locked` and
`cargo clippy --workspace --all-targets --locked`. Benchmark harnesses that fetch datasets or
build competitor engines run separately; do not add those campaigns to every PR.

Release artifacts must identify the reviewed source commit, target and checksum. Test the
artifact before promotion. Build and publish in separate jobs; contribution jobs cannot access
release credentials. Never publish automatically from an untrusted PR. User journals, private
transcripts, credentials and internal benchmark data must not enter logs or artifacts.

## No Vercel source builds

- Keep `git.deploymentEnabled: false` in `vercel.json` at any Vercel application's configured root.
- Any future project must disable previews and use ignored build command `exit 0`; no nightly
  source-build exception. The existing `oojuro-com` website project already has these settings.
- Build for the intended environment with `vercel build`, then upload with
  `vercel deploy --prebuilt`. A failed or missing artifact fails deployment; no source-upload
  fallback or deploy hook. Preserve this rule in scheduled and manual jobs too.
- Supply intended build-time configuration before building; upload metadata cannot change
  embedded values. Verify source/artifact/environment identity before promotion. Never log
  configuration values. Only trusted publishing jobs receive deployment credentials.
- Git controls do not stop a credential holder from submitting an explicit source build.
  Release jobs and operator instructions must enforce the prebuilt-only path.

This controls build compute; it does not change application-serving infrastructure. Oojuro's
embedded engine must remain buildable without our Vercel account or private runners.

## Reference guidance

The [shared research and policy](https://github.com/oojuro/softaxiom/blob/main/docs/CI_AND_BUILDS.md)
records the reviewed OpenClaw and Hermes workflows. Follow
[GitHub's self-hosted runner security guidance](https://docs.github.com/en/actions/reference/security/secure-use#hardening-for-self-hosted-runners),
[Vercel's Git deployment controls](https://vercel.com/docs/project-configuration/git-configuration#turning-off-all-automatic-deployments)
and [prebuilt deployment documentation](https://vercel.com/docs/cli/deploy#prebuilt).
