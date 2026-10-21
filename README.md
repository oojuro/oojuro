# Oojuro

The journal for AI agents: an embedded database that records everything an agent sees, thinks and does,
losslessly compressed, and lets it search its own history in milliseconds. No LLM inside the database.

**Status:** pre-release. Code lands here when the v1 architecture is approved.

## License

This license split is fixed and will not be changed.

| Part | License |
|---|---|
| Engine, daemon (`oojurod`), server: everything not marked otherwise | [FSL-1.1-ALv2](LICENSE.md). Free for any use except offering a competing commercial service. Each release becomes Apache-2.0 two years after it ships |
| Client SDKs, CLI and MCP client libraries (directories with their own `LICENSE`) | [Apache-2.0](LICENSE-APACHE-2.0) |

Embedding Oojuro in your own agents and products, commercial or not, is permitted. Offering Oojuro (or a
substitute built from it) as a competing hosted service is not, until each release converts to Apache-2.0.
This is the same model Sentry uses.

## Trademarks

"Oojuro" and its logos are trademarks of the Oojuro project. The Apache-2.0 license grants rights to the
code, not to the name (see section 6 of the license). Hosted services built on this code must not be
called "Oojuro" or imply endorsement.

## Contributing

Contributions are accepted under the license of the directory they change (inbound = outbound), with a
Developer Certificate of Origin sign-off (`git commit -s`).
