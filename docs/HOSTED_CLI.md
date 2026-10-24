> Historical agent-application document. Current ownership and maintained instructions
> are in [Oojuro Journal](https://github.com/oojuro/o-journal/blob/main/docs/HOSTED_CLI.md).
> The content below describes the pre-split monorepo at its recorded revision.

# Hosted reads in the existing Oojuro CLI

Private candidate, not a published release. Build the existing `oojuro` binary:

```sh
cargo build --locked --release -p oojuro --bin oojuro
```

`oojuro cloud records`, `oojuro cloud search` and `oojuro cloud get` use the same common project API as
`OojuroProjectClient`. Local recorder commands retain their existing behavior.
This read adapter does not register sources, append events or administer accounts.
Capture still uses its separate append-only credential and durable SDK spool.

## Connection

Create a protected absolute file, for example `/private/oojuro/connection.json`,
with mode 0600 in an owner-controlled directory. Save your one-time **project read
key**, issued under Projects → API keys, there. Never pass the secret as an argument.

```json
{
  "version": 1,
  "base_url": "https://oojuro.com",
  "project": "<64 lowercase hex project ID>",
  "read_key": "ojp_<one-time-project-read-secret>"
}
```

`oojuro.com` is the target product domain; live customer acceptance and cutover
remain pending. The isolated AWS preview hostname can be used while testing the
selected deployment. HTTPS is mandatory except explicit loopback HTTP. URL user
information, paths, queries and fragments are refused. Only `ojp_` credentials are
accepted, never capture, native-prefix or website-session tokens. Files with
unsafe permissions, symlink components or multiple hardlinks are refused.

## Metadata and exact content

```sh
oojuro cloud records --connection /private/oojuro/connection.json \
  --from 2026-10-22T00:00:00Z --through 2026-10-23T00:00:00Z \
  --agent <64-hex-agent-id> --limit 100

oojuro cloud get --connection /private/oojuro/connection.json \
  --journal <64-hex-journal-id> --id <64-hex-record-id> > record.txt
```

Metadata prints one JSON page: `snapshot`, `records` and `next`. Time endpoints
are inclusive. Omit either bound for no bound; agent selection is optional. Count
is 1–1,000, default 100. The existing CLI time forms work: RFC 3339, UTC date,
`@<decimal Unix seconds>`, `now` or a duration ago. Unix decimal times are parsed
as exact signed 64-bit microseconds; overflow, nonfinite values, exponent form
and nonzero sub-microsecond precision are refused rather than rounded/clamped.

Content is exact UTF-8 through 16 MiB: BOM/NUL and final-newline presence survive.
No extra newline or display header is added. Use global `--json` before `cloud`
for an escaped JSON string instead of raw text. Metadata has no text field; fetch
only content your application needs. Optional capture descriptors carry original
event/run identities. This is time/agent review, not common full-text search.

## Continue a snapshot

The cursor can be up to 2 MiB; store it in a protected mode-0600 file instead of
putting it into shell arguments. For example, save `next` from your first page:

```sh
umask 077
oojuro cloud records --connection /private/oojuro/connection.json --limit 100 > page.json
jq -er .next page.json > /private/oojuro/cursor.txt
# Only continue when next is a string. next:null means the snapshot is exhausted.
oojuro cloud records --connection /private/oojuro/connection.json --limit 100 \
  --cursor-file /private/oojuro/cursor.txt
```

Repeat the exact original absolute time/agent selection and read key. Only count
may change. Relative times such as `1h` or `now` resolve again on every command;
use absolute times for continuation. Cursors travel in form POST bodies, never
URLs. The snapshot is a vector of actual committed journal snapshots, not an
atomic cross-journal generation. Later appends appear on a fresh query. Cursors
expire after one hour; key/reference/deletion changes or unavailable source
history can refuse continuation. Do not silently restart the query when a
continuation fails. Separate content reads use current authority and may be
denied after a detach/delete/revoke.

## Metered full-text search

Save a UTF-8 query in a mode-0600 file under your private directory. The query
stays out of shell arguments/history and request URLs:

```sh
oojuro cloud search --connection /private/oojuro/connection.json \
  --request-id YOUR_RETAINED_64_HEX_REQUEST_ID --query-file /private/oojuro/query.txt \
  --from @0 --through @2000000000 --k 20
```

Time/agent selectors have the same meaning as `records`. `--k` is 1–100,
default 20. Output is complete JSON with an opaque vector snapshot and original
record descriptors, native scores/continuations and exact UTF-8 excerpt byte
offsets. There is no search cursor. The server uses current visible keyed versions
and refuses searches beyond its selected input budget with 413; narrow time or
agent selection when necessary. See the [shared search contract](PROJECT_READ_SDK.md#full-text-search).

This operation can consume search allowance. Retain the exact request ID and
query after an unknown outcome. Repeating them returns status only (409), never
another execution or saved results. A new request ID explicitly starts another
search. Errors after dispatch say `hosted_search_outcome_unknown`, retain a safe
HTTP status when available and produce no partial stdout. No automatic retry is
performed. Connection/query files must remain private.

## Bounds and failures

Requests are at most 2 MiB, metadata responses 4 MiB, content 16 MiB. Every command
collects and validates the entire bounded response before stdout: truncated,
invalid or oversized responses return failure without a partial page/content.
Exit is 0 for a complete read, 2 for a refusal/error. Request deadline is 60 seconds,
connection deadline 10 seconds. There is no redirect following, automatic retry,
cookie jar, referrer or automatic decompression. Server errors print a fixed code
and HTTP status; provider bodies/URLs and bearer material are not error messages.
Connection/cursor files are not copied to the local recorder, and hosted reads
never create or open a local journal. Protect redirected outputs according to
your data retention policy.

The HTTP dependency reuses locked reqwest 0.12.28 with blocking Rustls TLS. It
uses the documented [builder controls](https://docs.rs/reqwest/0.12.28/reqwest/blocking/struct.ClientBuilder.html)
and explicit [never-retry policy](https://docs.rs/reqwest/0.12.28/reqwest/retry/fn.never.html).
No package versions were changed by this addition.

## Checks and remaining scope

Actual CLI subprocess/HTTP tests cover scoped POST/cursor transport, private files,
strict protocol fields, selected time/agent, redirects, denial, truncated/chunked
failure, exact 16 MiB content and redacted errors. Existing local CLI round trips
and time-parser checks also pass: eight scoped Rust checks total. Ordinary scoped
Clippy succeeds with 31 inherited dependency diagnostics; strict `-D warnings`
stops on two pre-existing `oojuro-core` style diagnostics. No warning-free claim.
The cloud joined gate optionally uses
`OOJURO_HOSTED_CLI=/absolute/oojuro` to join this binary to its real
PostgreSQL/HTTP account/key service and installed SDK fixture. This is not an AWS
customer source, published install, production recovery or throughput benchmark.
Hosted capture CLI, automatic framework/harness capture, browser-assisted login,
growing-history search and full AWS qualification remain required launch work.
