# Starter dictionaries

`starter-v1.zdict` (114,688 bytes, BLAKE3
`5454cd0a5c4b56188da21c52b070f8e7ed2cd4c1b32889fda89b416781f839ff`): a zstd
dictionary for the flush runs of new and small stores. Run files name it by
the first 16 bytes of its BLAKE3 and never store it, so this file must never
change; a new dictionary is a new file with a new id, and every shipped
version stays readable.

Trained (`crates/oojuro-trace/examples/train_starter.rs`, 112 KiB target,
zstd's `ZDICT_trainFromBuffer` on ~11 MB of randomly sampled rows) on the
archive rows of a trace store holding TraceBench `base1m`: engine-encoded
records of these public datasets, **permissive licenses only** (project
policy):

| dataset | license | used |
|---|---|---|
| nebius/SWE-agent-trajectories, shards 0-1 | CC BY 4.0 (attribution) | yes |
| SWE-Gym/OpenHands-SFT-Trajectories | MIT | yes |
| NousResearch/hermes-function-calling-v1 (func-calling) | Apache-2.0 | yes |
| open-thoughts/OpenThoughts-114k, shard 0, first 4,000 | Apache-2.0 | yes |

Attribution (CC BY 4.0): contains substrings of "SWE-agent trajectories" by
Nebius (huggingface.co/datasets/nebius/SWE-agent-trajectories), licensed
CC BY 4.0. Not used for any shipped dictionary: the BEIR corpora in the
shared cache (CC BY-SA 4.0, GPL-3.0, CC BY-NC 2.0) and any private logs.
