import type { OojuroClient, OojuroCaptureClient, OojuroProjectClient, OpaqueId, WireInteger } from "./index.js";
import type { FileSink, FileReceipt } from "./files.js";
/** Initial linked-attachment bound; not a hosted-capacity promise. Generic files have a separate bound. */
export const ATTACHMENT_MAX_BYTES: 16777216;
export interface JournalAttachmentDescriptor {
  /** Persist the complete descriptor and exact selected bytes before the first request. */
  uploadId: OpaqueId;
  eventId: OpaqueId;
  /** Actual logical journal ID and its actual run identity; names never select authority. */
  journal: OpaqueId;
  runId: OpaqueId;
  writer: OpaqueId;
  agentId: OpaqueId;
  displayName: string;
  size: WireInteger;
  sha256: string;
  validAt: WireInteger;
  consent: "intentional-user-file";
}
export interface JournalAttachmentReference {
  readonly format: "oojuro-attachment-1";
  readonly journal: OpaqueId;
  readonly writer: OpaqueId;
  readonly upload_id: OpaqueId;
  readonly display_name: string;
  readonly manifest_name: string;
  readonly manifest_revision: OpaqueId;
  readonly total_bytes: WireInteger;
  readonly sha256: string;
}
export interface JournalAttachmentReceipt {
  readonly journal: OpaqueId;
  readonly eventId: OpaqueId;
  readonly recordId: OpaqueId;
  readonly file: JournalAttachmentReference;
  readonly generation: WireInteger;
}
export class PreparedJournalAttachment {
  /** Separate explicit file-write credential and metadata-only capture credential. */
  constructor(uploader: OojuroClient, capture: OojuroCaptureClient, input: JournalAttachmentDescriptor);
  /** No automatic retry or durable state. Unknown outcomes require the same descriptor and exact bytes.
   * File publication precedes the journal event; interrupted linking may leave an unlinked file. */
  send(source: AsyncIterable<Uint8Array>, options?: { signal?: AbortSignal }): Promise<JournalAttachmentReceipt>;
}
export function prepareJournalAttachment(uploader: OojuroClient, capture: OojuroCaptureClient,
  input: JournalAttachmentDescriptor): PreparedJournalAttachment;
/** Project read key; event visibility, pinned version and current permission are checked on every part.
 * Application-owned transactional sink must discard tentative bytes on abort. No parsing/preview. */
export function downloadJournalAttachmentTo(client: OojuroProjectClient,
  input: { journal: OpaqueId; recordId: OpaqueId; file: JournalAttachmentReference }, sink: FileSink,
  options?: { signal?: AbortSignal; minGeneration?: WireInteger }): Promise<FileReceipt>;
