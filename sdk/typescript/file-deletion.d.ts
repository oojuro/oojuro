import { OojuroError, type OojuroClient, type OpaqueId, type WireInteger } from "./index.js";
/** Persist the exact original descriptor before the first delete attempt; contains private file metadata. */
export interface FileDeletionDescriptor {
  readonly format: "oojuro-user-file-delete-1";
  readonly deleteId: OpaqueId;
  readonly writer: OpaqueId;
  readonly name: string;
  readonly manifestRevision: OpaqueId;
  readonly manifestSha256: string;
  readonly manifest: string;
  readonly consent: "delete-selected-user-file";
}
export interface FileDeletionPlanOptions {
  deleteId: OpaqueId;
  writer: OpaqueId;
  name: string;
  /** Explicit selected original manifest revision from its upload receipt. */
  manifestRevision: OpaqueId;
  minGeneration?: WireInteger;
  signal?: AbortSignal;
  consent: "delete-selected-user-file";
}
export interface FileDeletionProgress {
  /** Historical validated ACK for removing the selected manifest revision; a new file may exist. */
  readonly logicalFileDeleted: boolean;
  /** Every selected original part delete has a matching ACK; not physical/cryptographic erasure. */
  readonly cleanupComplete: boolean;
  readonly partCount: number;
  readonly acknowledgedParts: number;
  readonly remainingParts: number;
  readonly remainingPartIndices: readonly number[];
  readonly generation: WireInteger;
}
export class OojuroFileDeletionError extends OojuroError {
  constructor(code: string, options: { status?: number; outcome?: "none" | "unknown"; progress: FileDeletionProgress });
  readonly progress: FileDeletionProgress;
}
/** Read only. Does not remove anything. Caller must durably retain the returned exact selection. */
export function planFileDeletion(client: OojuroClient, input: FileDeletionPlanOptions): Promise<FileDeletionDescriptor>;
export class PreparedFileDeletion {
  constructor(client: OojuroClient, descriptor: FileDeletionDescriptor);
  readonly progress: FileDeletionProgress;
  /** One original-ID attempt; no automatic cleanup or retry. */
  removeManifest(options?: { signal?: AbortSignal }): Promise<FileDeletionProgress>;
  /** Explicit selected-revision cleanup, only after matching original manifest-delete ACK.
   * Replay from part zero; unknown/denied/conflicting attempt stops immediately. */
  cleanupParts(options?: { signal?: AbortSignal }): Promise<FileDeletionProgress>;
}
export function prepareFileDeletion(client: OojuroClient, descriptor: FileDeletionDescriptor): PreparedFileDeletion;
