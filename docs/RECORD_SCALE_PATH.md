# Record lookup and the million-record scale gate

Oojuro is a generic embedded database. This work concerns exact records and
lexical retrieval; o-journal owns agent integrations. The proposed standalone
`.ojo` container is a separate storage-format project and is not implemented by
this lookup change.

The current Record authority and disposable lexical projection enforce
100,000-record and 128 MiB state limits. Keep those protections. Local warm
100k-record timings cannot establish the owner's million-record, ten-client
performance target, and increasing constants would not establish bounded memory.

The next scale path must make work proportional to selected pages/terms:

1. Reuse the existing persistent/paged index components where their contracts
   fit. Bind a bounded manifest and independently validated index pages to the
   exact committed record watermark. Derived index data remains disposable;
   it cannot become record authority or silently substitute a stale snapshot.
2. Append incrementally and publish derived index progress with explicit
   freshness. Acknowledged originals must survive a failure before an index
   update. Index catch-up/rebuild latency belongs in the measured operation
   where required; hiding it from a timing loop does not qualify freshness.
3. Seek term/time partitions and merge top-k results with the same filters,
   version winners, statistics/ranking and byte-exact original reads. Count
   complete project/partition fanout and avoid materializing every original
   before every query. Report any changed ranking contract explicitly.
4. Bound each decoded page, posting list, working set and cache admission.
   Report payload/index/WAL/manifest/dictionary/temporary-compaction bytes and
   peak memory. Per-page bounds are not a total process-memory guarantee.
5. Verify missing/corrupt/stale index behavior, cold reopen, interrupted
   publication, idempotent retry, original recovery and all acknowledged IDs.
   Run increasing 1k/10k/100k/1m stages, lexical selectivity/filter mixes and ten
   concurrent clients with offered load, queueing and timeouts retained.

This is a scale acceptance outline, not a completed index implementation,
throughput result, storage-saving claim or SLA. The checked ID lookup remains
small and independently reviewable while the larger index path is developed.
