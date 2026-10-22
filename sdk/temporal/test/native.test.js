import test from "node:test";
import assert from "node:assert/strict";
import {mkdtempSync,mkdirSync,writeFileSync,readFileSync,chmodSync,rmSync} from "node:fs";
import {tmpdir} from "node:os";
import {join} from "node:path";
import {spawnSync} from "node:child_process";
import {NativeTemporalRecorder,TemporalCaptureError,createWorkflowSinks,createActivityInterceptor} from "../index.js";
const BIN=process.env.OOJURO_NATIVE_TEST_BIN;
if(!BIN)throw new Error("OOJURO_NATIVE_TEST_BIN must name the real built native binary");
const policy=()=>({version:1,record:true,paused:false,record_thinking:true,index_thinking:true,exclude:[],denied_tools:[]});
class Fixture {
 constructor() {
  this.parent=mkdtempSync(join(tmpdir(),"oojuro-temporal-sdk-"));chmodSync(this.parent,0o700);
  this.root=join(this.parent,"native");this.path=join(this.parent,"policy.json");
  writeFileSync(this.path,JSON.stringify(policy()),{mode:0o600});
  this.call("setup",["--harness","temporal","--writer","temporal:synthetic","--agent","7","--policy",this.path]);
 }
 call(command,args=[]) {
  const r=spawnSync(BIN,[command,"--root",this.root,...args],{encoding:"utf8",timeout:20000});
  assert.equal(r.status,0,"native command failed");
  return JSON.parse(r.stdout);
 }
 recorder(options={}) {return new NativeTemporalRecorder({binary:BIN,root:this.root,generation:"generation-1",...options});}
 raw(sourceId) {const p=join(this.parent,"raw-"+Math.random().toString(16).slice(2));this.call("restore-raw",["--source-id",sourceId,"--out",p,"--allow-raw-export"]);return readFileSync(p,"utf8");}
 close(){rmSync(this.parent,{recursive:true,force:true});}
}
const stream={namespace:"synthetic",workflow_id:"workflow",run_id:"run"};
const event=(id,payload="observable text")=>({id,stream,valid_us:123456,kind:"input",payload});
const source=async()=>{
 const {createHash}=await import("node:crypto");
 return createHash("sha256").update(JSON.stringify({...stream,generation:"generation-1"})).digest("hex").slice(0,32);
};
test("actual SDK to encrypted native authority: stable retry, reopen, read/search, byte-exact original raw",async()=>{
 const f=new Fixture();try {
  const r=f.recorder();const input=event("original",{message:"ORIGINALMARKER",nested:[1,true,null]});
  const pending=r.record(input);input.payload.message="MUTATEDAFTERPREPARE";
  const first=await pending;assert.equal(first.outcome.stored,1);
  const again=await f.recorder().record(event("original",{message:"ORIGINALMARKER",nested:[1,true,null]}));
  assert.deepEqual(again,first);
  await r.record(event("second","SEARCHMARKER"));
  const record=f.call("get",["--id","1"]);assert.match(record.output.event.text,/ORIGINALMARKER/);assert.doesNotMatch(record.output.event.text,/MUTATEDAFTERPREPARE/);
  assert.equal(f.call("search",["--query","SEARCHMARKER"]).hits.length,1);
  const raw=f.raw(await source());const frames=raw.trimEnd().split("\n").map(JSON.parse);
  assert.equal(frames.length,2);assert.equal(frames[0].id,"original");assert.equal(frames[1].sequence,2);
  assert.deepEqual(frames[0].payload,{message:"ORIGINALMARKER",nested:[1,true,null]});
  assert.equal(Buffer.byteLength(raw),(await r.record(event("second","SEARCHMARKER"))).end);
  const sealed=readFileSync(join(f.root,"native.json"),"utf8");assert.match(sealed,/"temporal"/);
 }finally{f.close();}
});
test("changed intent never duplicates an acknowledged event",async()=>{
 const f=new Fixture();try {
  const r=f.recorder();const first=await r.record(event("one","ORIGINAL"));
  await assert.rejects(r.record(event("one","CHANGED")),e=>e instanceof TemporalCaptureError&&e.outcome==="unknown"&&!String(e).includes("CHANGED"));
  assert.equal(f.call("stats").stored,1);assert.equal((await r.record(event("one","ORIGINAL"))).end,first.end);
 }finally{f.close();}
});
test("real native ACK loss retries exact original bytes even with unreadable current policy",async()=>{
 const f=new Fixture();try {
  const wrapper=join(f.parent,"native-ack-loss");const mark=join(f.parent,"lost-once");
  const quote=s=>"'"+s.replaceAll("'","'\\''")+"'";
  writeFileSync(wrapper,"#!/bin/sh\nif [ \"$1\" = temporal-append ] && [ ! -e "+quote(mark)+" ]; then\n  "+quote(BIN)+" \"$@\" >/dev/null\n  result=$?\n  if [ \"$result\" -eq 0 ]; then : >"+quote(mark)+"; exit 1; fi\n  exit \"$result\"\nfi\nexec "+quote(BIN)+" \"$@\"\n",{mode:0o700});
  const r=f.recorder({binary:wrapper});const prepared=await r.prepare(event("lost","ACKLOSSMARKER"));
  let failure;try{await prepared.send();assert.fail("ACK should be lost");}catch(e){failure=e;}
  assert.equal(failure.outcome,"unknown");assert.doesNotMatch(String(failure),/ACKLOSSMARKER/);
  writeFileSync(join(f.root,"policy.json"),"{unreadable policy",{mode:0o600});
  const receipt=await failure.retry();assert.equal(receipt.event_id,"lost");assert.equal(f.call("stats").stored,1);
  assert.equal(f.raw(await source()).trimEnd().split("\n").length,1);
 }finally{f.close();}
});
test("denied operation result remains withheld after policy relaxation and restart",async()=>{
 const f=new Fixture();try {
  const strict=policy();strict.denied_tools=["blocked_tool"];writeFileSync(join(f.root,"policy.json"),JSON.stringify(strict));
  const common={stream,valid_us:123456,tool:"blocked_tool",operation_id:"op"};
  assert.equal((await f.recorder().record({...common,id:"start",kind:"tool_call",payload:{command:"private"}})).outcome.stored,0);
  writeFileSync(join(f.root,"policy.json"),JSON.stringify(policy()));
  assert.equal((await f.recorder().record({...common,id:"end",kind:"tool_result",status:"ok",payload:"WITHHELDRESULT"})).outcome.stored,0);
  assert.equal(f.call("stats").stored,0);assert.equal(f.call("search",["--query","WITHHELDRESULT"]).hits.length,0);
  assert.match(f.raw(await source()),/WITHHELDRESULT/);
 }finally{f.close();}
});
test("bounded diagnostic queue exposes drops and settles before reuse",async()=>{
 const f=new Fixture();try {
  const diagnostics=[];const r=f.recorder({maxPending:1,onDiagnostic:code=>diagnostics.push(code)});
  assert.equal(r.offer(event("one")),true);assert.equal(r.offer(event("two")),false);
  assert.equal((await r.drain()).pending,0);assert.equal(r.stats().dropped,1);assert.deepEqual(diagnostics,["queue_full"]);
  assert.equal(r.offer(event("three")),true);await r.drain();assert.equal(r.stats().committed,2);
  assert.equal(r.offer({...event("invalid"),payload:undefined}),false);await r.drain();
  assert.equal(r.stats().failed,1);
 }finally{f.close();}
});
test("workflow sink is replay-disabled diagnostic, but stable task-retry IDs deduplicate",async()=>{
 const f=new Fixture();try {
  const r=f.recorder();const sinks=createWorkflowSinks(r);assert.equal(sinks.oojuro.record.callDuringReplay,false);
  const info={namespace:"synthetic",workflowId:"workflow",runId:"run",taskToken:"NEVERCOPYTOKEN"};
  const observed={id:"workflow-step-1",valid_us:123456,kind:"output",payload:"observed"};
  assert.equal(sinks.oojuro.record.fn(info,observed),undefined);await r.drain();
  sinks.oojuro.record.fn(info,observed);await r.drain();
  assert.equal(f.call("stats").stored,1);assert.doesNotMatch(f.raw(await source()),/NEVERCOPYTOKEN/);
 }finally{f.close();}
});
test("Activity observer preserves success/error identities and never captures task tokens or headers",async()=>{
 const f=new Fixture();try {
  const r=f.recorder();const factory=createActivityInterceptor(r);
  const ctx={info:{namespace:"synthetic",workflowExecution:{workflowId:"workflow",runId:"run"},activityId:"activity",activityType:"synthetic_task",attempt:1,currentAttemptScheduledTimestampMs:123,taskToken:"NEVERCOPYTOKEN"}};
  const interceptor=factory(ctx).inbound;
  const result={answer:"RESULT"};
  assert.equal(await interceptor.execute({args:["input"],headers:{secret:"NEVERCOPYHEADER"}},async()=>result),result);
  await r.drain();assert.equal(f.call("stats").stored,2);
  const raw=f.raw(await source());assert.doesNotMatch(raw,/NEVERCOPYTOKEN|NEVERCOPYHEADER/);assert.match(raw,/current_attempt_scheduled/);
  // Trace failure is observed separately, including an error after the business activity has run.
  const broken=f.recorder({root:join(f.parent,"absent"),onDiagnostic:()=>{throw new Error("callback");}});
  const observe=createActivityInterceptor(broken)(ctx).inbound;
  const failure=new Error("business failure");
  await assert.rejects(observe.execute({args:[],headers:{}},async()=>{throw failure;}),e=>e===failure);
  const returned={value:3};assert.equal(await observe.execute({args:[],headers:{}},async()=>returned),returned);
  await broken.drain();assert.ok(broken.stats().failed>0);
 }finally{f.close();}
});
test("unsupported values and accessor/cycle payloads refuse without invoking native commands",async()=>{
 const f=new Fixture();try {
  const r=f.recorder();let getterCalled=false;const payload={get dangerous(){getterCalled=true;return "private";}};
  await assert.rejects(r.record(event("getter",payload)),e=>e.code==="json_required");
  assert.equal(getterCalled,false);
  const cyclic={};cyclic.self=cyclic;
  await assert.rejects(r.record(event("cycle",cyclic)),e=>e.code==="json_required");
  await assert.rejects(r.record(event("big","x".repeat((1<<20)+1))),e=>e.code==="input_bound");
  assert.equal(f.call("stats").stored,0);
 }finally{f.close();}
});

test("diagnostic property getters and throwing offer never replace business success or error",async()=>{
 const f=new Fixture();try{
  const r=f.recorder();
  const ctx={info:{namespace:"synthetic",workflowExecution:{workflowId:"workflow",runId:"run"},activityId:"activity",activityType:"synthetic_task",attempt:1,currentAttemptScheduledTimestampMs:123}};
  for(const field of ["name","message"]) {
   const original=new Error("business");Object.defineProperty(original,field,{get(){throw new Error("diagnostic getter");}});
   let caught;try{await createActivityInterceptor(r)(ctx).inbound.execute({args:[],headers:{}},async()=>{throw original;});}catch(e){caught=e;}
   assert.equal(caught,original);
  }
  await r.drain();
  r.offer=()=>{throw new Error("diagnostic offer");};
  const observer=createActivityInterceptor(r)(ctx).inbound;
  const returned={result:"same"};assert.equal(await observer.execute({args:[],headers:{}},async()=>returned),returned);
  const original=new Error("original");let caught;
  try{await observer.execute({args:[],headers:{}},async()=>{throw original;});}catch(e){caught=e;}
  assert.equal(caught,original);
  assert.doesNotThrow(()=>createWorkflowSinks(r).oojuro.record.fn({},event("x")));
  assert.ok(r.stats().failed>=5);
 }finally{f.close();}
});
test("oversized strings and property keys refuse before serialization or native dispatch",async()=>{
 const f=new Fixture();try{
  const r=f.recorder(),original=JSON.stringify;let serializations=0;
  const huge="x".repeat((1<<20)+1),keyed={[huge]:true};
  JSON.stringify=(...args)=>{serializations++;return original(...args);};
  try{
   await assert.rejects(r.record(event("string",huge)),e=>e.code==="input_bound");
   await assert.rejects(r.record(event("key",keyed)),e=>e.code==="input_bound");
   assert.equal(r.offer(event("offer",huge)),false);
  }finally{JSON.stringify=original;}
  assert.equal(serializations,0);assert.equal(r.stats().pending,0);assert.equal(f.call("stats").stored,0);
 }finally{f.close();}
});
