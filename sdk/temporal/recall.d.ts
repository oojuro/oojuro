import type {ClientOptions,SearchInput,Watermark,OpaqueId,TraceKind} from "@oojuro/v2-client";
export interface RecallOptions {
  /** Outside-workflow credentials; no credentials belong in Workflow arguments/sinks. */
  client: ClientOptions;
  maxResults?: number;
  maxTextBytes?: number;
}
export interface RecallHit {
  id: OpaqueId; writer: OpaqueId; excerpt: string; excerpt_start: number; excerpt_end: number;
  continued: boolean; score_bits: number; kind: TraceKind; valid_us: number;
}
export interface RecallResult {
  watermark: Watermark; hits: RecallHit[]; truncated: boolean; returned_by_server: number;
}
export type RecallInput = Omit<SearchInput,"k"> & {k?: number};
export class PreparedRecall {
  private constructor();
  /** One actual metered request; no automatic reexecution/retry. Preserve original requestId. */
  send(options?: {signal?: AbortSignal}): Promise<RecallResult>;
}
export class OojuroRecall {
  constructor(options: RecallOptions);
  readonly maxResults: number;
  prepare(input: RecallInput): PreparedRecall;
}
export function createRecallActivity(options: RecallOptions): (input: RecallInput)=>Promise<RecallResult>;
export function createRecallTool(options: RecallOptions): {
  name: "oojuro_recall"; description: string;
  inputSchema: Readonly<Record<string,unknown>>;
  execute(input: RecallInput): Promise<RecallResult>;
};
