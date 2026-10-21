# Oojuro

The journal for AI agents: an embedded database that records everything an agent sees, thinks and does,
losslessly compressed, and lets it search its own history in milliseconds. No LLM inside the database.

**Status:** pre-release. Code lands here when the v1 architecture is approved.

## License

Everything in this repository is licensed under the **Apache License 2.0** (see `LICENSE` and `NOTICE`).
That covers the engine, the recorder and daemon (`oojurod`), the file format, the SDKs, the CLI and the MCP
tools.

**Pledge:** the code in this repository will never be relicensed. What is open stays Apache-2.0.

How Oojuro, Inc. makes money: the hosted Oojuro Cloud (durable journals for sandboxed agents, team journals,
compliance archive) is a separate proprietary service and is not part of this repository. Embedding Oojuro
in your own agents and products, commercial or not, is and stays free.

## Trademarks

"Oojuro" and its logos are trademarks of the Oojuro project. The Apache-2.0 license grants rights to the
code, not to the name (see section 6 of the license). Hosted services built on this code must not be
called "Oojuro" or imply endorsement.

## Contributing

Contributions are accepted under the license of the directory they change (inbound = outbound), with a
Developer Certificate of Origin sign-off (`git commit -s`).
