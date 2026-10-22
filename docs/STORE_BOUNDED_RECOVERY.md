# Store recovery and training workspace bounds

This is the first mandatory Store capacity slice after native Record disk directories. It preserves existing Store wire formats, options, original records/IDs/times/attrs/keys, source ACK semantics and reader APIs. It does not make all Store metadata bounded or implement the complete foundation SPEC scale engine.

## Changes

Hot replay previously read an entire hot file into a Vec. It now reads a fixed file header and one CRC-framed payload at a time through positional reads. The existing maximum is unchanged: payload <=64 MiB +64 bytes. AEAD, frame-head/sequence and decoded metadata verification still precede publishing a frame locator. A valid CRC but invalid AEAD/header is corruption, not a repairable tail.

On an invalid frame in the last hot file, recovery retains the existing exact byte-by-byte rule: if any later CRC-valid frame exists, refuse midstream corruption; otherwise truncate to the original last verified boundary and sync it. Later-frame detection uses 64 KiB header/CRC chunks instead of a whole-file allocation. A CRC candidate spanning chunks is still detected. Adversarial candidate lengths may cause repeated positional IO; this patch bounds workspace, not worst-case recovery CPU or IO. Non-last damaged hot files always refuse.

Initial and compaction dictionary training now feed the same deterministic content-independent priority reservoir directly from decoded frames. At most 2048 selected record prefixes of 8 KiB each are retained (16 MiB payload plus descriptors), independent of population size. Training constructs one <=16 MiB contiguous sample while consuming selected entries, so selected plus contiguous payload workspace is bounded by32 MiB, plus existing frame/decompression/zstd state, descriptors and output dictionary. Too little usable sample or an impossible requested training size yields no dictionary. The reservoir considers all candidate records, not only a chronological prefix. Public train_dict also delegates to it.

Dictionary selection/compression bytes can differ from older binaries; durable dictionary/frame decoding formats are unchanged and original data is lossless. This is not a compression ratio or training-quality improvement claim.

## Remaining assigned code

Store still keeps its FrameRef Vec, file handle HashMap, dictionary bytes/maps, manifest lists and compaction snapshots/chunk tables. Cold-index open still reads that entire index. Repeated failed initial training may re-read hot frames. The next opt-in disk frame/file/dictionary reader must remove these retained populations before any larger-limit or10M/100M acceptance. The old writer/default RecordLog still retains its own source population. No limit was raised here.

## Verification

Four focused synthetic regressions passed (5.17 s compile,4.78 s runtime): differential old/new later-frame criteria at chunk boundaries and malformed lengths/CRCs, bounded/deterministic distributed sampling, and actual sealed append/reopen/compaction preserving all original fields and disk confidentiality. Final source also corrects the marker assertion's length and removes a newly unused variable; broader Store and strict lint gates follow.

No provider, AWS, Notion, customer data, package installation, merge or deployment. Heavy verification uses an isolated target and shared flock outside actual bounded steps.
