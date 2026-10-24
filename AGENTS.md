# Working on Oojuro

Read README.md, docs/DESIGN.md and docs/REPOSITORY_BOUNDARIES.md first.
Oojuro owns embedded storage and retrieval. Agent capture, transcript adapters,
session policy, recorder, application CLI/MCP and SDK changes belong to
`oojuro/o-journal`; managed hosting and the website belong to `oojuro/o-cloud`.
Trusted release coordination belongs to `oojuro/o-release-operator`.

Preserve supported storage formats, exact original reads, encryption, recovery,
retry behavior and public library compatibility. `.ojo` is the chosen database
extension; do not claim standalone container support before it is implemented.
Keep private service code, credentials and real agent/customer history out of
public source. Preserve Apache-2.0 and third-party notices.

Use topic branches, signed commits and the relevant checks in CONTRIBUTING.md.
A source change or repository split does not publish a package or deploy a service.
