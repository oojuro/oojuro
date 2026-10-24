# Retained synthetic Store reader evidence

`evidence.tar.gz` contains 414 flat regular UTF-8 files. It stores metadata, raw synthetic test reports and public core measurement code only. All headers use uid/gid0, mode0644, mtime0 and empty owner names; gzip mtime is zero. The manifest outside the archive records original and published size/SHA-256 for every file, plus compressed and decompressed archive hashes. The archive is 252,700 B; original selected evidence totals 2,650,797 B.

Both campaigns, all 64 reports and all full-scan adverse pairs are retained. `first-*` refers to runtime fdb49ec; `successor-*` refers to d492438. Build source and measurement-only source are distinct: the latter adds identical ignored helper registration only. The baseline is retained PR88, not a rebuilt control.

The common helper is synthetic test code; its fixed `[91;32]` test-cipher seed is public fixture construction, not exported secret material. No fixture files or encryption keys from execution, binary, corpus/original payload, PGDATA or private hosted code are archived. Runtime originals are verified by hashes and full-field checks in the helper rather than published.

Workspace, home, private temporary and user-cgroup path prefixes are replaced with placeholders in published files. Original source SHA and published SHA distinguish that transformation; scalar measurements, source/artifact/compiler hashes and test outcomes are unchanged. Host-specific controller paths/cgroup settings require review and remapping before reproduction. The scripts are evidence, not a generic installer or instruction to execute them unreviewed.

To inspect without running anything, list the standard tar.gz and verify entries against `manifest.json`. For reproduction, apply the archived common helper and its cfg(test) registration to each pinned core source, build with the recorded Rust1.99.1/portable release/[] configuration, require exact source/freshness guards, and retain the fixed four alternating rounds, synthetic originals, resource caps and complete postverification. Reproduction is a new campaign; existing evidence is not silently replaced.

The root independently verified every raw report and all current inventory/cleanup proofs for each campaign. There is no independent live-sampling claim where the root observed only after the short unit ended. Full process peaks include verification; phase RSS and reserved capacity must not be substituted for one another.
