import type { ActivityInterceptorsFactory, InjectedSinks } from "@temporalio/worker";
import type { Sinks } from "@temporalio/workflow";
export type Json=null|boolean|number|string|readonly Json[]|{readonly [key:string]:Json};
export interface Stream {namespace:string;workflow_id:string;run_id:string;}
export interface TemporalEvent {
  /** Stable logical event ID, retained with its exact original intent across retry. */
  id:string;stream:Stream;valid_us:number;
  kind:"input"|"output"|"thinking"|"system"|"tool_call"|"tool_result";
  tool?:string|null;operation_id?:string|null;status?:"ok"|"error"|null;
  payload:Json;attributes?:Readonly<Record<string,Json>>;
}
export interface Receipt {
  event_id:string;sequence:number;cursor:number;end:number;
  outcome:{raw_end:number;parsed_end:number;first_id:number|null;stored:number;next_id:number;};
}
export class TemporalCaptureError extends Error {
  readonly code:string;readonly outcome:"none"|"unknown";
  /** Explicit retry of the frozen original prepared append. No payload in the error. */
  retry():Promise<Receipt>;
}
export interface RecorderOptions {
  binary:string;root:string;generation:string;
  maxPending?:number;timeoutMs?:number;
  /** Static bounded code only, never an event, provider error or secret. */
  onDiagnostic?:(code:string)=>void;
}
export class PreparedTemporalEvent {
  private constructor();
  /** One attempt; retry this same object or an error's retry method after unknown outcome. */
  send():Promise<Receipt>;
}
export interface Stats {readonly offered:number;readonly dropped:number;readonly failed:number;readonly committed:number;readonly pending:number;}
export class NativeTemporalRecorder {
  constructor(options:RecorderOptions);
  prepare(event:TemporalEvent):Promise<PreparedTemporalEvent>;
  /** Resolves only after a fully validated native durable/readable ACK. */
  record(event:TemporalEvent):Promise<Receipt>;
  /** Best effort, bounded and nonblocking; true means queued, never acknowledged. */
  offer(event:TemporalEvent):boolean;
  stats():Stats;
  drain():Promise<Stats>;
}
export type WorkflowEvent=Omit<TemporalEvent,"stream">;
export interface OojuroSinks extends Sinks {oojuro:{record(event:WorkflowEvent):void;};}
/** Workflow-task retries may repeat these diagnostics despite callDuringReplay=false. */
export function createWorkflowSinks(recorder:NativeTemporalRecorder):InjectedSinks<OojuroSinks>;
/** Diagnostic observer. Trace failure never changes an Activity's business outcome. */
export function createActivityInterceptor(recorder:NativeTemporalRecorder):ActivityInterceptorsFactory;
