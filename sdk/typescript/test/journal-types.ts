import {
  OojuroClient,
  type JournalInput,
  type JournalQuery,
  type JournalRecordInput,
  type JournalPage,
  type Read,
  type PreparedJournalWrite,
  type PreparedJournalPeriods,
} from "../index.js";
const c = new OojuroClient({
  baseUrl: "https://example.invalid",
  project: "1".repeat(64),
  token: "caller-token",
});
const input: JournalInput = {
  agent_id: "reported",
  parent_id: null,
  valid_at: 0,
  kind: "explicit",
  tool: null,
  status: null,
  exit: null,
  key: null,
  text: "caller unredacted text",
  attrs: { reported: true },
  indexed: false,
};
const query: JournalQuery = {
  as_of: { from: 0, valid_at: 100, known_at: 100 },
  generation: null,
  writer: null,
  mode: { mode: "Range", versions: "current", filter: { kind: null } },
};
const root = {
  schema: 1 as const,
  minimum_reader: 1 as const,
  project: "1".repeat(64),
  namespace: "2".repeat(64),
};
const write: JournalRecordInput = {
  requestId: "original",
  consent: "intentional-user-file",
  expectedRoot: root,
  writer: "3".repeat(64),
  adapter: "caller",
  input,
};
const prepared: PreparedJournalWrite = c.prepareJournalRecord(write);
const page: Promise<Read<JournalPage>> = c.journalPage({
  expectedRoot: root,
  query,
  cursor: null,
  maxBytes: 4096,
});
const billed: PreparedJournalPeriods = c.prepareJournalPeriods({
  expectedRoot: root,
  query,
  requestId: "original",
  widthUs: 10,
  maxPeriods: 100,
});
void prepared;
void page;
void billed;
// @ts-expect-error Consent is mandatory.
c.prepareJournalRecord({
  requestId: "original",
  expectedRoot: root,
  writer: "3".repeat(64),
  adapter: "caller",
  input,
});
// @ts-expect-error Case matches actual Rust serde.
const bad: JournalQuery["mode"] = { mode: "Range", versions: "Current", filter: {} };
// @ts-expect-error New Journal namespace uses typed native input, not legacy reported Trace.
c.prepareJournalRecord({ ...write, input: { format: 2, text: "legacy" } });
// @ts-expect-error No enrollment or raw-capture endpoint.
c.journalEnroll(root);
