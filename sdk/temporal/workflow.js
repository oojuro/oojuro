/** Workflow-safe diagnostic helper. No Node APIs, I/O or acknowledgement promise. */
import { proxySinks, workflowInfo } from "@temporalio/workflow";
const sinks=proxySinks();
let ordinal=0;
export function recordWorkflowEvent(event) {
  try {
    const info=workflowInfo();
    const id=event.id??"workflow-observation:"+(++ordinal);
    sinks.oojuro.record({...event,id,valid_us:event.valid_us??Date.now()*1000,
      attributes:{...event.attributes,workflow_type:info.workflowType}});
    return true;
  }catch{return false;}
}
