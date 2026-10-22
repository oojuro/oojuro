// Opt-in real SDK bundling check. Reuses installed peers; never opens a Temporal connection.
import {createRequire} from "node:module";
import {mkdtempSync,writeFileSync,rmSync} from "node:fs";
import {tmpdir} from "node:os";
import {join,dirname,resolve} from "node:path";
import {fileURLToPath} from "node:url";
const require=createRequire(import.meta.url),packageRoot=resolve(dirname(fileURLToPath(import.meta.url)),"..");
const worker=process.env.OOJURO_TEMPORAL_WORKER_ENTRY;
const {bundleWorkflowCode}=worker?require(worker):require("@temporalio/worker");
const scratch=mkdtempSync(join(tmpdir(),"oojuro-workflow-bundle-"));
try{
 const path=join(scratch,"workflows.js");
 writeFileSync(path,"import {recordWorkflowEvent} from "+JSON.stringify(join(packageRoot,"workflow.js"))+";\nexport async function captureProbe(){recordWorkflowEvent({kind:'system',payload:'synthetic'});return true;}\n");
 const bundled=await bundleWorkflowCode({workflowsPath:path,webpackConfigHook(config){
   if(process.env.OOJURO_TEMPORAL_PEER_MODULES)config.resolve.modules.push(process.env.OOJURO_TEMPORAL_PEER_MODULES);
   return config;
 }});
 if(!bundled.code.includes("captureProbe"))throw new Error("workflow export missing");
 console.log("Actual Temporal workflow bundle passed");
}finally{rmSync(scratch,{recursive:true,force:true});}
