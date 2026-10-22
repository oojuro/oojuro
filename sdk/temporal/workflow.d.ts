import type { WorkflowEvent } from "./index.js";
/** Workflow-safe, best-effort diagnostic. No I/O or durability acknowledgement. */
export function recordWorkflowEvent(event:Omit<WorkflowEvent,"id"|"valid_us">&{id?:string;valid_us?:number}):boolean;
