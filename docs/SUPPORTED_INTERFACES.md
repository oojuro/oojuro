# Supported interface decision — 24 October 2026

The owner chose the native journal and current hosted APIs without legacy compatibility requirements. This is a pre-release breaking change.

- `oojuro-native`: local encrypted journal, capture, records, original text, search and bounded export. See [quickstart](LOCAL_JOURNAL_QUICKSTART.md).
- `oojuro cloud`: scoped hosted capture, durable client state, project history and hosted stdio MCP. See [combined setup](COMBINED_AGENT_SETUP.md).
- `oojuro-engine`, `oojuro-rank`, `oojuro-store`: current embedded libraries. SDKs remain experimental source packages.

The `oojurod` daemon, socket fallback, old `Journal`/`Reader`/`Oojuro` facade, local MCP, hook/proxy/OTLP ingestion front doors and their flags are removed. Old commands fail explicitly. There is no automatic conversion of an old journal, daemon configuration or credentials. Use a new native journal and current Cloud enrollment. Preserve old data separately if needed; historical source remains in Git.

Trace/Nine and comparison tools remain research/test oracles outside the shipped client dependency graph. Native capture shares policy and metadata code directly; the old ingestion implementation is compiled only in unit tests for independent parity checks. Encryption, acknowledgement/retry semantics and recovery checks remain release requirements.

Historical design and benchmark documents describe their original snapshots. They do not extend the supported interface list above. `.ojo` remains the intended standalone extension; standalone-file support is not implemented.
