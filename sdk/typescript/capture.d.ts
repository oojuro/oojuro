import type { CaptureEvent, OojuroCaptureClient } from "./index.js";
/** Node 24.4+ POSIX private local SQLite spool. Plaintext inputs are retained until ACK.
 * Enqueue is synchronous and durable on a correctly functioning fsync-capable filesystem.
 * A local enqueue is not a server acknowledgment. */
export class CaptureSpool {
  private constructor();
  static open(options: {
    directory: string;
    client: OojuroCaptureClient;
    maxBytes?: number;
    maxEvents?: number;
    signal?: AbortSignal;
  }): Promise<CaptureSpool>;
  readonly agentId: string;
  pending(): { events: number; bytes: number };
  enqueue(event: CaptureEvent): { eventId: string; queued: true };
  /** Keeps originals after every HTTP, timeout, invalid-ACK or unknown failure.
   * Flush again to retry; no hidden timer or process-global shutdown hook. */
  flush(options?: { timeoutMs?: number; signal?: AbortSignal }): Promise<{ delivered: number; remaining: number }>;
  /** Does not discard queued events or implicitly flush. Refuses while flushing. */
  close(): void;
}
