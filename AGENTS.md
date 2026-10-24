# Working on Oojuro

Read README.md, docs/DESIGN.md and docs/REPOSITORY_BOUNDARIES.md first.
Oojuro owns embedded storage and retrieval. Agent capture, transcript adapters,
session policy, recorder, application CLI/MCP and SDK changes belong to
`oojuro/o-journal`; managed hosting and the website belong to `oojuro/o-cloud`.
Trusted release coordination belongs to `oojuro/o-release-operator`.

Preserve supported storage formats, exact original reads, encryption, recovery,
retry behavior and public library compatibility. The minimal standalone file
engine is crates/ojo; legacy crates remain for existing consumers. Wire 0.1 is
experimental and defined independently in spec/0.1/FORMAT.md. Preserve frozen
vectors and never apply historical directory-store benchmarks to the file engine.
Keep private service code, credentials and real agent/customer history out of
public source. Preserve Apache-2.0 and third-party notices.

Use topic branches, signed commits and the relevant checks in CONTRIBUTING.md.
A source change or repository split does not publish a package or deploy a service.
