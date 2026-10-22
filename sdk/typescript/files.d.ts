import type { OojuroClient, OpaqueId, WireInteger } from "./index.js";
/** SDK bounds, not a capacity guarantee. Server/ledger caps may refuse earlier. */
export const FILE_PART_BYTES: 262144;
export const FILE_MAX_PARTS: 1024;
export const FILE_MAX_BYTES: 268435456;
export interface FileUploadDescriptor {
  /** Caller-generated unique identity, durably retained with exact original content before send. */
  uploadId: OpaqueId;
  writer: OpaqueId;
  name: string;
  /** Required: null means create only; otherwise the original manifest Blob revision. */
  expected: OpaqueId | null;
  size: WireInteger;
  /** Lowercase SHA-256 of the complete exact original content, known before upload. */
  sha256: string;
  /** Explicit user-file permission; trace capture/redaction is not this consent. */
  consent: "intentional-user-file";
}
export interface FileReceipt {
  readonly writer: OpaqueId;
  readonly name: string;
  readonly uploadId: OpaqueId;
  readonly size: WireInteger;
  readonly sha256: string;
  readonly generation: WireInteger;
  readonly manifestName: string;
}
export interface FileUploadReceipt extends FileReceipt {
  /** Supply as expected for an intentional later version. */
  readonly revision: OpaqueId;
}
export class PreparedFileUpload {
  constructor(client: OojuroClient, input: FileUploadDescriptor);
  /** One forward pass, no automatic retries. Each source chunk must be <= FILE_PART_BYTES.
   * Replay after unknown outcome with the same descriptor and exact original content.
   * This object is RAM state, not an automatically durable resume journal. */
  send(source: AsyncIterable<Uint8Array>, options?: { signal?: AbortSignal }): Promise<FileUploadReceipt>;
}
export function prepareFileUpload(client: OojuroClient, input: FileUploadDescriptor): PreparedFileUpload;
export function fileManifestName(name: string): string;
export interface FileReadOptions {
  writer: OpaqueId;
  name: string;
  minGeneration?: WireInteger;
  signal?: AbortSignal;
}
/** Transactional application-owned destination. Writes remain tentative until commit.
 * abort must discard tentative bytes; it never deletes server parts or changes retention. */
export interface FileSink {
  write(part: Uint8Array): void | Promise<void>;
  commit(receipt: FileReceipt): void | Promise<void>;
  abort(): void | Promise<void>;
}
/** Preferred safe path: commit is called only after all part/full digests, exact length,
 * final pointer and authorization checks succeed. Any failure invokes abort. */
export function downloadFileTo(client: OojuroClient, input: FileReadOptions, sink: FileSink): Promise<FileReceipt>;
/** Advanced: yielded part-verified bytes are tentative. Consume to EOF and await
 * completion before publishing/accepting any file. Early iterator return rejects completion. */
export function downloadFile(client: OojuroClient, input: FileReadOptions): Promise<{
  readonly chunks: AsyncIterable<Uint8Array>;
  readonly completion: Promise<FileReceipt>;
}>;
