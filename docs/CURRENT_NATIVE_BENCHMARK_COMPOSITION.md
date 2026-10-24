# Current native benchmark composition

This topic joins draft89 (`f4753638c255145e82c48d4f12c87cba3fe78703`)
with draft88 (`7dbd7b228a667779c560192a9e8388a709c927e0`). It is a local
qualification candidate, not a release or a change to the measured draft89 result.

- Engine, Store and ranking tracked trees are byte-identical to draft88.
- Benchmark sources and Cargo.lock are byte-identical to draft89.
- The merge is automatic, without conflict resolution or runtime edits.
- The Store prefix/cache/full-decode experiment is deliberately not composed here;
  its separately reported measurements remain mixed and unpromoted.

Draft89 measured Native84 against PostgreSQL frequency ranking. It does not
measure the later counted-result, borrowed-text and append-lifetime changes.
The next measurement must compare this current composition against the retained
Native84 artifact on the same complete public corpus and fixed query schedule,
including quality, exact full originals, copied reopen, append tails and separate
anonymous/file-cache/whole-cgroup memory. Existing PostgreSQL measurements need
not be repeated merely to measure this native source delta.

No build, test, timing, memory gain or compatibility qualification is established
by this source composition alone. Any later qualification must pin the actual
source, compiler/profile/features, artifacts and complete fixed workload results.
