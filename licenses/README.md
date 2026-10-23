# Native distribution supplements

These files supplement upstream notices; they do not replace Oojuro's LICENSE or NOTICE.

- `alloc-stdlib-0.2.4/LICENSE`: exact Dropbox workspace BSD-3-Clause notice from
  [revision ae42d22078b98549e987d2f03d12df7b984fde47](https://github.com/dropbox/rust-alloc-no-stdlib/blob/ae42d22078b98549e987d2f03d12df7b984fde47/LICENSE).
  That is the `.cargo_vcs_info.json` source revision of alloc-stdlib 0.2.4. Its published
  crate declares BSD-3-Clause but omits the notice. The upstream file is preserved unchanged.
- `dictionary/CC-BY-4.0.txt`: full [Creative Commons legal code](https://creativecommons.org/licenses/by/4.0/legalcode.txt),
  relevant to the Nebius-derived material in the embedded starter dictionary.
- `dictionary/MIT.txt`: standard MIT text from [SPDX license-list-data v3.27.0](https://github.com/spdx/license-list-data/blob/v3.27.0/text/MIT.txt).
  This is a license template, with its placeholders unchanged, not a newly asserted copyright.
  SWE-Gym's dataset card declares MIT but supplies no separate copyright/license file.
  Preserve the dataset's supplied attribution in NOTICE as well as these permission terms.

## Embedded dictionary attribution

The dictionary is transformed material, not newly Apache-licensed data. Its upstream provenance
and method are preserved in `crates/oojuro-store/dict/README.md` and root NOTICE. Reviewed cards:

| Source | Supplied license | Reviewed card revision |
| --- | --- | --- |
| [Nebius SWE-agent trajectories](https://huggingface.co/datasets/nebius/SWE-agent-trajectories/blob/68195a1/README.md) | CC BY 4.0 | 68195a1 |
| [SWE-Gym OpenHands trajectories](https://huggingface.co/datasets/SWE-Gym/OpenHands-SFT-Trajectories/blob/4aaa5a4/README.md) | MIT | 4aaa5a4 |
| [NousResearch Hermes function calling](https://huggingface.co/datasets/NousResearch/hermes-function-calling-v1/blob/1977bb5/README.md) | Apache-2.0 | 1977bb5 |
| [OpenThoughts-114k](https://huggingface.co/datasets/open-thoughts/OpenThoughts-114k/blob/bd093c3/README.md) | Apache-2.0 | bd093c3 |

These are evidence for the published grants, not claims about the exact training revisions.
Do not fetch or ship entire datasets as part of a native release. Preserve creator/source links,
license links and the existing statement that selected text trained a compression dictionary.
This review relies on upstream license declarations; it cannot establish additional rights in
unidentified underlying repository excerpts. Any future input with different or ambiguous rights
requires review before a new dictionary is shipped. Do not replace the existing dictionary bytes:
stored journal runs name that exact dictionary and require it for recovery.
