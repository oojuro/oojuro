# Experimental replay-cache release: not a default recommendation

Signed candidate `2723bc46429d76c15a10904e0e3b0b79a1ee28ff` adds a small
`Store::clear_read_cache(&mut self)` wrapper around the existing frame-cache
clear and calls it after successful typed RecordLog replay. Replay itself keeps
the original 4 MiB cache, including efficient reads of multi-record compacted
frames. Decoder contexts and dictionaries remain. Typed original rows are
already retained and RecordLog does not subsequently read those Store frames.
There is no storage mutation, authority shortcut, key/reply cache or format change.

The [frozen artifacts and source/DCO mapping](benchmarks/record-replay-cache-2026-10-23/provenance.json)
record this as **experimental only**: the repeated latency cost fails the
repository's default performance acceptance rule. Do not select or integrate
this change by default. Follow-up query work starts from the signed allocation
slack baseline `22a1767`, excluding this cache change.

## Matched copied-store measurements

Same frozen public ArguAna store (8,674 original records), 15 dev queries,
top100 plus exact original reads per query. Three fresh processes/copied stores
per arm and order, interleaved A/B and B/A with a separate full reversal; warm
OS cache, same workstation, generic x86-64 release, 3 GiB/no-swap child scope.
Baseline is compact postings plus build-slack trim, not the deployed source.
No known overlapping build/test during these timing runs. RSS is sampled after
operations and includes allocator-retained pages; this is not a total RAM cap.

| Order | Source | RSS after open MB | RSS after query sequence MB | Open ms | Search+original roundtrip p50 ms |
| --- | --- | ---: | ---: | ---: | ---: |
| First | Baseline | 48.087 | 80.937 | 89.156 | 64.865 |
| First | Cleared | 44.229 | 77.070 | 98.496 | 72.553 |
| Reverse | Baseline | 48.042 | 80.876 | 93.832 | 74.110 |
| Reverse | Cleared | 43.639 | 76.616 | 99.379 | 78.551 |

Values are medians of three per-process statistics. The roughly 4 MB RSS
reduction repeats, but search+original roundtrip p50 is approximately 12% and
6% slower in the two orders. Open is also slower. Samples overlap and are
small; neither a universal cost nor a benefit is claimed. The concern persisted
in the one bounded follow-up, so no additional timing repetition was used to
seek a more favorable sample. Caller-owned data and allocator/decoder overhead
outlive cache ownership release; plaintext erasure is not implied.

## Correctness and storage

- Encrypted Store tests: 14 passed. The new functional test clears a warmed
  compacted cache, checks exact reads and caller-owned originals, then changes
  sealed bytes with a valid outer checksum and requires authentication failure
  after clear. Existing fresh-open forgery coverage remains.
- Existing record/projection/recovery tests: 100 passed, five ignored.
- Portable release benchmark/profile build passed. Full 45-hit signatures
  match the baseline across IDs, exact score bits, continuation, clocks,
  metadata, UTF-8 excerpts and original checks.
- All seven harness gates pass over three runs, including nine SIGKILL trials.
- All 12 copied-store runs retain exactly identical file contents, SHA-256,
  apparent sizes and allocated sizes before/after open/query/close. Returned
  original bytes and ranked ID/score signatures match across sources.

No AWS, deployment, pricing, hosted capacity saving, public release or main
merge is part of this experiment. Releasing idle decoded frames remains an
explicit RAM/latency tradeoff for possible future design, not a shipped gain.
