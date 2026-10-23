import type {Payload,PayloadCodec,SerializationContext} from "@temporalio/common";
import type {ClientOptions,OpaqueId} from "@oojuro/v2-client";
export class TemporalStorageError extends Error {
  readonly code: string;
  readonly outcome: "none" | "unknown";
}
export interface ClaimCheckOptions {
  /** Outside-workflow client credentials; never include them in Workflow arguments/history. */
  client: ClientOptions;
  writer: OpaqueId;
  namespace: string;
  /** Caller-protected durable 32-byte HMAC key; retain the same key across retry/restart/replay. */
  bindingKey: Uint8Array;
  /** Explicit original-payload file storage consent. Raw harness recording grants none. */
  consent: "intentional-user-file";
  /** Default 256KiB; 1KiB..1MiB, conservatively measured over the complete canonical payload frame. */
  thresholdBytes?: number;
  /** Complete payload frame cap, default/max32MiB. */
  maxPayloadBytes?: number;
  /** Whole batch cap, default/max64MiB; up to1024 payloads. */
  maxBatchBytes?: number;
}
export class OojuroClaimCheckCodec implements PayloadCodec {
  constructor(options: ClaimCheckOptions);
  encode(payloads: Payload[],context?: SerializationContext): Promise<Payload[]>;
  decode(payloads: Payload[],context?: SerializationContext): Promise<Payload[]>;
}
