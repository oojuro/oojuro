# Optional native record context packing

This pure helper is an explicit experimental native-record feature. It does not
install a hook, enable automatic injection, read a clock, authorize a record,
write authority state, train a ranker, or call a provider. Its coordinator owns
the separate module/CLI composition. Call it only with current authorized,
visible search hits; a returned packed context is not an authority witness.

`pack(candidates, options, current_turn, token_measure)` defaults to disabled.
When explicitly enabled, defaults follow the reference context policy: minimum
8 lexical text bits, at most 20 records and 2,500 caller-measured tokens. Output
is independently capped at 64 KiB of UTF-8 JSON. Inputs are capped at 1,000
candidates and 32 MiB aggregate original text. Rank receipts have at most eight
stages with bounded labels. Work is deterministic and bounded around the
trusted token callback; an in-process trait cannot preempt a callback or prove
its tokenizer implementation.

`TokenMeasure` supplies a bounded tokenizer/model/version label, its declared
Exact or Conservative guarantee, and a count of the **complete final JSON**,
including record metadata and trust labels. The helper supplies no characters
per token guess or universal BPE claim. The caller must use a suitable exact
counter or establish its conservative bound for the actual target model.
Token/byte-budget failure skips a whole candidate and tries the next; text is
never truncated, normalized or replaced. Callback failure returns an error,
never a partially accepted result.

Each selected item preserves the complete original Record and lexical bits,
row/continued fields, optional typed rank receipt/deltas and optional reported
session-turn metadata. Ranker boosts and recency priors cannot lift a record
below the original text-bit eligibility floor. Eligible items sort by packing
bits descending, valid time descending, writer then decimal ID text. This tie
policy is deterministic; it does not establish cross-writer score calibration.

The optional session-turn-decay-v1 prior is relative to session start: a known
earlier turn in the same session receives bits-per-turn times its turn number;
unknown/other sessions and turn zero receive none. Both record valid and engine
recorded times must be strictly before the current turn start. Supplied reported
turn start must precede the record's times and current turn; a same-session
current/future turn is invalid. All session/actor/tool/source/parent fields stay
reported metadata, never independently verified causal relationships.

Echo exclusion is conservative: known context markers, explicit Oojuro context/
call/result attributes, Oojuro-named tool calls or tool-call text mentioning
Oojuro, and an exact trimmed current input prompt are excluded. It is not a
complete shell-command classifier or a proof that all semantic echoes were
removed. The JSON envelope labels recalled content as untrusted data rather
than instructions. That label does not guarantee a reading model will obey it.

Tests are synthetic and cover disabled behavior, whole-original provenance,
Unicode, exact final token/byte boundaries, oversize skipping, deterministic
ordering, rank/recency eligibility, before-turn and echo exclusions, invalid
metadata, receipt/input bounds and callback failure. Module exports and a
composed focused test run are the native coordinator's next step; no benchmark,
quality-adoption or automatic-injection claim follows from authored tests.
