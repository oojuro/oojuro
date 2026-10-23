/** Current experimental prefix-v2 API; not the proposed foundation launch SDK. */
export type OpaqueId = string;
/** All wire integer fields use the exact JS safe-integer subset; larger u64/i64 are refused. */
export type WireInteger = number;
export type Json =
  | null
  | boolean
  | number
  | string
  | readonly Json[]
  | { readonly [key: string]: Json };
export type TraceKind =
  | "system"
  | "user_input"
  | "assistant"
  | "thinking"
  | "tool_call"
  | "tool_result"
  | "explicit";
/** Caller-reported provenance. These fields never authorize access or verify a causal graph. */
export interface ReportedProvenance {
  agent_id: string;
  parent_id: string;
  source: string;
  session: string;
  parent_session: string;
  tool: string;
}
/** Reference metadata only. No source bytes or parser context are loaded or uploaded. */
export interface RawRef {
  source_generation: string;
  start: WireInteger;
  end: WireInteger;
  emission: WireInteger;
}
export interface Trace extends ReportedProvenance {
  format: 2;
  text: string;
  kind: TraceKind;
  turn: WireInteger;
  sequence: WireInteger;
  parent: WireInteger;
  revises: WireInteger;
  valid_us: WireInteger;
  recorded_us: WireInteger;
  duration_us: WireInteger;
  arguments: Json;
  status: string;
  exit_code: number | null;
  tokens_in: WireInteger;
  tokens_out: WireInteger;
  indexed: boolean;
  metadata: Readonly<Record<string, string>>;
  raw: RawRef | null;
}
export type Mutation =
  | { kind: "Record"; writer: OpaqueId; trace: Trace }
  | { kind: "Memory"; writer: OpaqueId; name: string; trace: Trace }
  | {
      kind: "Blob";
      writer: OpaqueId;
      name: string;
      expected: OpaqueId | null;
      bytes: readonly number[];
    }
  | { kind: "DeleteRecord"; id: OpaqueId }
  | { kind: "DeleteMemory"; writer: OpaqueId; name: string }
  | { kind: "DeleteBlob"; writer: OpaqueId; name: string; expected: OpaqueId };
export interface CommitInput {
  requestId: string;
  scope: OpaqueId;
  mutations: readonly Mutation[];
}
/** Server-assigned committed usage observation; never a client-supplied admission/grant.
 * Tags are required for every data mutation supported by this client. */
export interface UsageFact {
  request_tag: OpaqueId;
  intent_fingerprint: OpaqueId;
  scope_tag: OpaqueId;
  /** Logical server observation time, not provider CAS wall-clock evidence. */
  committed_at_us: WireInteger;
  /** Newly recorded bytes in this commit. */
  recorded_bytes: WireInteger;
  /** Authoritative total retained bytes after this commit; may exceed recorded_bytes. */
  kept_bytes: WireInteger;
}
export interface Outcome {
  generation: WireInteger;
  record_ids: Readonly<Record<string, OpaqueId>>;
  blob_revisions: Readonly<Record<string, OpaqueId>>;
  writer: null;
  key: null;
  destruction: null;
  usage: UsageFact;
}
export interface Watermark {
  generation: WireInteger;
  epoch: WireInteger;
  transaction: { id: OpaqueId; digest: OpaqueId };
}
export interface Read<T> {
  watermark: Watermark;
  value: T;
}
/** Independently server-authorized grant, separate from caller-reported Trace metadata. */
export interface AuthorizationGrant {
  admin: boolean;
  read: readonly OpaqueId[];
  write: readonly OpaqueId[];
  retry: readonly OpaqueId[];
}
export interface TraceRecord {
  id: OpaqueId;
  writer: OpaqueId;
  writer_name: string;
  memory_name: string | null;
  trace: Trace;
}
export interface SearchHit extends TraceRecord {
  score_bits: number;
  excerpt: string;
  continued: boolean;
  excerpt_start: WireInteger;
  excerpt_end: WireInteger;
}
export interface ReadOptions {
  minGeneration?: WireInteger;
  signal?: AbortSignal;
}
export interface ClientOptions {
  /** HTTPS origin, or explicit loopback HTTP for local development; no credentials/path/query/hash. */
  baseUrl: string;
  project: OpaqueId;
  token: string;
  fetch?: typeof globalThis.fetch;
  /** May lower defaults, never exceed 8 MiB request or 10 MiB response. */
  maxRequestBytes?: number;
  maxResponseBytes?: number;
  /** Opt-in framed methods use independent budgets; at most 16 MiB + 64 KiB + 4.
   * These do not increase JSON request/response limits. */
  maxFramedRequestBytes?: number;
  maxFramedResponseBytes?: number;
}
export class OojuroError extends Error {
  readonly code: string;
  readonly status?: number;
  /** Every commit attempt lacking a validated ACK remains unknown, including a rejected retry. */
  readonly outcome: "none" | "unknown";
}
export class PreparedCommit {
  private constructor();
  /** Exactly one attempt; no automatic retry and no new identity allocation. */
  send(options?: { signal?: AbortSignal }): Promise<Outcome>;
}
export function reportedTrace(
  text: string,
  fields?: Partial<Omit<Trace, "text" | "format">>,
): Trace;
export interface SearchInput {
  requestId: string;
  query: string;
  k: number;
  /** Frozen with the original intent; changing it is a different search. */
  minGeneration?: WireInteger;
}
export class PreparedSearch {
  private constructor();
  /** Exactly one attempt. Status-only retries never execute or return earlier result bytes. */
  send(options?: { signal?: AbortSignal }): Promise<Read<SearchHit[]>>;
}
/** Fresh hosted flat-record namespace; never enrolls or migrates legacy Trace data. */
export interface JournalRoot {
  schema: 1;
  minimum_reader: 1;
  project: OpaqueId;
  namespace: OpaqueId;
}
export type JournalKind =
  | "input"
  | "output"
  | "thinking"
  | "tool_call"
  | "tool_result"
  | "system"
  | "explicit"
  | "raw";
/** Explicit caller data. No redaction or independent provenance verification is performed. */
export interface JournalInput {
  agent_id: string;
  parent_id: string | null;
  valid_at: WireInteger;
  kind: JournalKind;
  tool: string | null;
  status: "ok" | "error" | null;
  exit: number | null;
  /** Required even when unkeyed. Key versions are scoped to writer. */
  key: string | null;
  text: string;
  attrs: Readonly<Record<string, Json>>;
  indexed: boolean;
}
export interface JournalRecord {
  id: OpaqueId;
  record: JournalInput & {
    schema: 1;
    id: WireInteger;
    writer: OpaqueId;
    adapter: string;
    recorded_at: WireInteger;
  };
}
export interface JournalQuery {
  as_of: { from: WireInteger; valid_at: WireInteger; known_at: WireInteger };
  generation: WireInteger | null;
  writer: OpaqueId | null;
  mode:
    | {
        mode: "Range";
        versions: "current" | "all";
        filter: {
          kind?: JournalKind | null;
          tool?: string | null;
          status?: "ok" | "error" | null;
          agent_id?: string | null;
        };
      }
    | { mode: "History"; key: string }
    | { mode: "Id"; id: OpaqueId };
}
export interface JournalRecordInput {
  requestId: string;
  consent: "intentional-user-file";
  expectedRoot: JournalRoot;
  writer: OpaqueId;
  adapter: string;
  input: JournalInput;
}
export interface JournalDeleteInput {
  requestId: string;
  expectedRoot: JournalRoot;
  writer: OpaqueId;
  id: OpaqueId;
}
export interface JournalForgetInput {
  requestId: string;
  expectedRoot: JournalRoot;
  writer: OpaqueId;
  key: string;
}
export interface JournalViewInput {
  expectedRoot: JournalRoot;
  query: JournalQuery;
}
export interface JournalPageInput extends JournalViewInput {
  /** Resume using the EXACT original root/query. Later appends are excluded. */
  cursor?: string | null;
  maxRecords?: number;
  maxBytes?: number;
}
export interface JournalPage {
  /** Snapshot watermark; envelope watermark is current and can be later. */
  watermark: Watermark;
  records: JournalRecord[];
  next: string | null;
}
export interface JournalDescriptor {
  id: OpaqueId;
  record: Omit<JournalRecord["record"], "text">;
  text_bytes: WireInteger;
  generation: WireInteger;
}
export interface JournalDescriptorsPage {
  watermark: Watermark;
  records: JournalDescriptor[];
  /** Bound to metadata projection; cannot resume a full page cursor. */
  next: string | null;
}
export interface JournalFramedReadInput {
  expectedRoot: JournalRoot;
  id: OpaqueId;
}
export interface JournalStats {
  records: WireInteger;
  text_bytes: WireInteger;
  kinds: Partial<Record<JournalKind, WireInteger>>;
  first_valid_at: WireInteger | null;
  last_valid_at: WireInteger | null;
}
export interface JournalPeriod {
  start_us: WireInteger;
  records: WireInteger;
  text_bytes: WireInteger;
}
export interface JournalPeriodsInput extends JournalViewInput {
  requestId: string;
  widthUs: WireInteger;
  maxPeriods: number;
  minGeneration?: WireInteger;
}
export class PreparedJournalWrite {
  private constructor();
  /** No automatic retry. A missing validated ACK always leaves the original outcome unknown. */
  send(options?: { signal?: AbortSignal }): Promise<Outcome>;
}
export class PreparedJournalPeriods {
  private constructor();
  /** Billed search. Original identity/body are frozen; retries return status, never recompute. */
  send(options?: { signal?: AbortSignal }): Promise<Read<JournalPeriod[]>>;
}
export class OojuroClient {
  constructor(options: ClientOptions);
  journalRoot(options?: ReadOptions): Promise<Read<JournalRoot>>;
  /** Encoded Journal POSTs are capped at 1 MiB, or the lower configured request limit.
   * Native 16 MiB text support does not imply equivalent HTTP capacity. */
  prepareJournalRecord(input: JournalRecordInput): PreparedJournalWrite;
  /** Explicit binary framing: bounded metadata plus exact UTF8 text, no JSON text expansion. */
  prepareJournalRecordFramed(input: JournalRecordInput): PreparedJournalWrite;
  /** No invented watermark: the frame has an actual generation header and one native record. */
  journalRecordFramed(
    input: JournalFramedReadInput,
    options?: ReadOptions,
  ): Promise<{ generation: WireInteger; value: JournalRecord }>;
  prepareJournalDelete(input: JournalDeleteInput): PreparedJournalWrite;
  /** Tombstones existing key versions; does not forbid future key reuse. */
  prepareJournalForget(input: JournalForgetInput): PreparedJournalWrite;
  journalPage(input: JournalPageInput, options?: ReadOptions): Promise<Read<JournalPage>>;
  journalDescriptorsPage(
    input: JournalPageInput,
    options?: ReadOptions,
  ): Promise<Read<JournalDescriptorsPage>>;
  journalStats(input: JournalViewInput, options?: ReadOptions): Promise<Read<JournalStats>>;
  prepareJournalPeriods(input: JournalPeriodsInput): PreparedJournalPeriods;
  prepareCommit(input: CommitInput): PreparedCommit;
  access(options?: ReadOptions): Promise<Read<AuthorizationGrant>>;
  records(options?: ReadOptions): Promise<Read<TraceRecord[]>>;
  record(id: OpaqueId, options?: ReadOptions): Promise<Read<TraceRecord>>;
  prepareSearch(input: SearchInput): PreparedSearch;
  search(input: SearchInput, options?: { signal?: AbortSignal }): Promise<Read<SearchHit[]>>;
  blob(
    writer: OpaqueId,
    name: string,
    options?: ReadOptions,
  ): Promise<{ generation: WireInteger; bytes: Uint8Array; revision?: OpaqueId }>;
  /** All-or-nothing bounded JSONL consumption: results exist only after clean, validated EOF. */
  exportRecords(
    options?: ReadOptions,
  ): Promise<{ generation: WireInteger; records: TraceRecord[] }>;
}

/** Immutable event/run IDs and all original input must survive retries and repacking. */
export interface CaptureEvent {
  eventId: OpaqueId;
  runId: OpaqueId;
  adapter: string;
  input: JournalInput;
}
export interface CaptureDiscovery {
  format: 1;
  journal: OpaqueId;
  agent_id: OpaqueId;
  key_id: OpaqueId;
  producer_id: OpaqueId;
  label: string;
  expires_us: WireInteger;
  limits: { events: number; metadata_bytes: number; text_bytes: number };
}
export interface CaptureReceipt {
  request_id: string;
  generation: WireInteger;
  recorded_bytes: WireInteger;
  records: { event_id: OpaqueId; record_id: OpaqueId; replayed: boolean }[];
}
export function captureEvent(input: CaptureEvent): CaptureEvent;
export class PreparedCapture {
  private constructor();
  /** Transport/invalid/truncated acknowledgments remain unknown. Retry this same prepared body. */
  send(options?: { signal?: AbortSignal }): Promise<CaptureReceipt>;
}
export class OojuroCaptureClient {
  constructor(options: { baseUrl: string; token: string; fetch?: typeof globalThis.fetch });
  readonly origin: string;
  discover(options?: { signal?: AbortSignal }): Promise<CaptureDiscovery>;
  prepareBatch(input: { requestId: string; events: readonly CaptureEvent[] }): PreparedCapture;
}
