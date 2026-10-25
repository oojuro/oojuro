# Actual Event-file search composition

`ojo index` streams verified Event originals into an encrypted disposable index;
`ojo search` derives its binding from the actual file checkpoint and reads every
selected original through that file's `Journal::get`. The neutral index checks
returned timestamp/text against its authenticated metadata before excerpts leave
the command. This path does not regenerate originals from fixture IDs.

The read-only source handle and its shared inode lock are retained through build
or materialization. The wire lock excludes cooperating writers. Source identity,
sequence and full checkpoint digest form the index binding. Unrelated files,
appended prefixes and independently archived physical files refuse old reuse;
exact exports retain the binding. Commands release handles before output and
report the verified point-in-time source checkpoint with their bounded result.
They do not reinterpret source timestamps or require application fields.

Two complete synthetic integration tests exercise raw/encrypted source files,
raw/Zstd indexes, exact binary/empty/Unicode data, original cursor/time/byte
excerpts, timestamp bounds, exact export at a legacy .ojo pathname, archive
reindexing, stale and foreign sources, wrong keys, argument/byte-budget refusal,
no replacement and byte-exact preservation of torn sources. Tests pass with
normal and search-only features; existing CLI/Event tests also pass with minimal
raw features. Strict all-target Clippy passes for normal and minimal builds.

Defaults admit2million UTF-8 rows,64GiB indexed text,256MiB numeric metadata
policy,8GiB logical cache/build files,64MiB original/cache-text query work and
1MiB result data. These are explicit resource policies rather than filesystem
quotas or measured process RSS. Binary data remains exactly readable but is not
searched. The source is verified and numeric index metadata is loaded on each
CLI invocation; no warm-query latency claim follows from these tests.

The normal tool now enables optional search and archive support. Minimal raw
builds retain their existing graph. The packager records selected default
features and derives versions from the reviewed manifests; the search graph's
store dictionary attribution and project supplements are included with registry
and Rust notices. Registry notice integrity/traversal tests pass. A qualified
new binary release and public installation proof remain separate requirements.

[Exact commands and source hashes](juro-file-search-2026-10-25.json).
No wire/profile/version or frozen-vector bytes changed. This is local functional
Event-file-path evidence, not million-record or hosted performance acceptance.
