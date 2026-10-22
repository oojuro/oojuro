import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { isAbsolute } from "node:path";
const MAX_FRAME=1<<20, MAX_RESPONSE=4096;
const states=new WeakMap(), retries=new WeakMap(), diagnostics=new WeakMap();
const kinds=new Set(["input","output","thinking","system","tool_call","tool_result"]);
const name=s=>typeof s==="string"&&s.length>0&&Buffer.byteLength(s)<=256&&!/[\p{Cc}]/u.test(s);
const integer=n=>Number.isSafeInteger(n)&&n>=0;
export class TemporalCaptureError extends Error {
  constructor(code,outcome="none",prepared) {
    super("Temporal capture "+code);
    this.name="TemporalCaptureError";this.code=code;this.outcome=outcome;
    if(prepared) retries.set(this,prepared);
  }
  retry() {
    const prepared=retries.get(this);
    if(!prepared) return Promise.reject(new TemporalCaptureError("no_prepared_retry"));
    return prepared.send();
  }
}
function snapshot(value) {
  let nodes=0,bytes=0;const ancestors=new Set();
  const count=s=>{if(s.length>MAX_FRAME||(bytes+=Buffer.byteLength(s))>MAX_FRAME)throw new TemporalCaptureError("input_bound");};
  function check(v,depth) {
    if(++nodes>8192||depth>32) throw new TemporalCaptureError("input_bound");
    if(typeof v==="string"){count(v);return;}
    if(v===null||typeof v==="boolean") return;
    if(typeof v==="number"&&Number.isFinite(v)) return;
    if(typeof v!=="object"||ancestors.has(v)) throw new TemporalCaptureError("json_required");
    const proto=Object.getPrototypeOf(v);
    if(proto!==Object.prototype&&proto!==null&&!Array.isArray(v)) throw new TemporalCaptureError("json_required");
    ancestors.add(v);
    for(const key of Reflect.ownKeys(v)) {
      if(Array.isArray(v)&&key==="length")continue;
      if(typeof key!=="string")throw new TemporalCaptureError("json_required");
      count(key);
      const d=Object.getOwnPropertyDescriptor(v,key);
      if(!d||!("value" in d))throw new TemporalCaptureError("json_required");
      check(d.value,depth+1);
    }
    ancestors.delete(v);
  }
  check(value,0);
  const json=JSON.stringify(value);
  if(Buffer.byteLength(json)>MAX_FRAME)throw new TemporalCaptureError("input_bound");
  return JSON.parse(json);
}
function input(value,generation) {
  const v=snapshot(value);
  if(!v||!name(v.id)||!kinds.has(v.kind)||!Number.isSafeInteger(v.valid_us)||!v.stream)
    throw new TemporalCaptureError("event_identity");
  if(v.tool!=null&&!name(v.tool)||v.operation_id!=null&&!name(v.operation_id)||v.status!=null&&!["ok","error"].includes(v.status))
    throw new TemporalCaptureError("operation_identity");
  if(["tool_call","tool_result"].includes(v.kind)&&(!name(v.tool)||!name(v.operation_id)))
    throw new TemporalCaptureError("operation_required");
  if(v.kind==="tool_call"&&(!v.payload||Array.isArray(v.payload)||typeof v.payload!=="object"))
    throw new TemporalCaptureError("tool_arguments");
  if(v.kind==="tool_result"&&!["ok","error"].includes(v.status))throw new TemporalCaptureError("result_status");
  if(!Object.hasOwn(v,"payload"))throw new TemporalCaptureError("payload_required");
  const stream={namespace:v.stream.namespace,workflow_id:v.stream.workflow_id,run_id:v.stream.run_id,generation};
  if(!Object.values(stream).every(name))throw new TemporalCaptureError("stream_identity");
  const attributes=v.attributes??{};
  if(!attributes||Array.isArray(attributes)||typeof attributes!=="object"||Buffer.byteLength(JSON.stringify(attributes))>4096)
    throw new TemporalCaptureError("attributes_bound");
  return {stream,id:v.id,valid_us:v.valid_us,kind:v.kind,tool:v.tool??null,operation_id:v.operation_id??null,status:v.status??null,payload:v.payload,attributes};
}
function receipt(value,eventId,cursor,end,sequence) {
  const exact=(v,keys)=>v&&typeof v==="object"&&!Array.isArray(v)&&Object.keys(v).sort().join(",")===keys.split(",").sort().join(",");
  if(!exact(value,"event_id,sequence,cursor,end,outcome")||!exact(value.outcome,"raw_end,parsed_end,first_id,stored,next_id"))throw new TemporalCaptureError("invalid_ack","unknown");
  const o=value?.outcome;
  if(value?.event_id!==eventId||value.sequence!==sequence||value.cursor!==cursor||value.end!==end||!integer(value.sequence)||value.sequence<1||
    !o||o.raw_end!==end||o.parsed_end!==end||!integer(o.stored)||!integer(o.next_id)||
    !(o.first_id===null||integer(o.first_id)&&o.first_id>0)||o.next_id<1||(o.stored===0)!==(o.first_id===null)||(o.first_id!==null&&o.next_id<o.first_id+o.stored))throw new TemporalCaptureError("invalid_ack","unknown");
  return Object.freeze(value);
}
function command(config,which,body) {
  return new Promise((resolve,reject)=>{
    let started=false,done=false,size=0;const chunks=[];
    const child=spawn(config.binary,[which,"--root",config.root,...(which==="temporal-append"?["--allow-raw-source"]:[])],{stdio:["pipe","pipe","ignore"],windowsHide:true});
    const finish=(error,value)=>{if(done)return;done=true;clearTimeout(timer);error?reject(error):resolve(value);};
    const timer=setTimeout(()=>{child.kill("SIGKILL");finish(new TemporalCaptureError("native_timeout",which==="temporal-append"&&started?"unknown":"none"));},config.timeoutMs);
    child.on("spawn",()=>{started=true;});
    child.on("error",()=>finish(new TemporalCaptureError("native_unavailable",started&&which==="temporal-append"?"unknown":"none")));
    child.stdout.on("data",chunk=>{size+=chunk.length;if(size>MAX_RESPONSE){child.kill("SIGKILL");finish(new TemporalCaptureError("native_response_bound",which==="temporal-append"?"unknown":"none"));}else chunks.push(chunk);});
    child.stdin.on("error",()=>{});
    child.on("close",code=>{
      if(done)return;
      if(code!==0)return finish(new TemporalCaptureError("native_refused",which==="temporal-append"?"unknown":"none"));
      try {finish(null,JSON.parse(new TextDecoder("utf-8",{fatal:true}).decode(Buffer.concat(chunks))));} catch {finish(new TemporalCaptureError("native_invalid_response",which==="temporal-append"?"unknown":"none"));}
    });
    child.stdin.end(body);
  });
}
export class PreparedTemporalEvent {
  constructor(token,state) {if(token!==states)throw new TemporalCaptureError("prepared_constructor");states.set(this,state);}
  send() {const s=states.get(this);return s.owner.enqueue(()=>sendPrepared(this));}
}
async function sendPrepared(prepared) {
  const s=states.get(prepared);
  try {return receipt(await command(s.config,"temporal-append",s.body),s.id,s.cursor,s.end,s.sequence);}
  catch(error) {throw new TemporalCaptureError(error instanceof TemporalCaptureError?error.code:"native_failure","unknown",prepared);}
}
export class NativeTemporalRecorder {
  #config;#tail=Promise.resolve();#pending=0;#diagnostic;#counts={offered:0,dropped:0,failed:0,committed:0};
  constructor(options) {
    if(!options||![options.binary,options.root].every(v=>typeof v==="string"&&Buffer.byteLength(v)<=4096&&!/[\p{Cc}]/u.test(v)&&isAbsolute(v))||!name(options.generation))
      throw new TemporalCaptureError("configuration");
    const maxPending=options.maxPending??16, timeoutMs=options.timeoutMs??10000;
    if(!integer(maxPending)||maxPending<1||maxPending>64||!integer(timeoutMs)||timeoutMs<10||timeoutMs>60000)
      throw new TemporalCaptureError("configuration");
    this.#config=Object.freeze({binary:options.binary,root:options.root,generation:options.generation,maxPending,timeoutMs});
    this.#diagnostic=options.onDiagnostic;
    diagnostics.set(this,()=>{this.#counts.failed++;this.#report("adapter_diagnostic_failed");});
  }
  enqueue(work) {
    if(this.#pending>=this.#config.maxPending)return Promise.reject(new TemporalCaptureError("queue_full"));
    this.#pending++;
    const next=this.#tail.then(work).finally(()=>{this.#pending--;});
    this.#tail=next.catch(()=>{});
    return next;
  }
  async #prepare(v) {
    const source_id=createHash("sha256").update(JSON.stringify(v.stream)).digest("hex").slice(0,32);
    const p=await command(this.#config,"temporal-position",JSON.stringify({source_id,stream:v.stream,event_id:v.id}));
    if(!integer(p?.cursor)||!integer(p?.sequence)||p.sequence>0xffffffff)throw new TemporalCaptureError("invalid_position");
    let cursor=p.cursor, sequence=p.sequence+1;
    if(p.existing!==null) {
      const r=p.existing;
      if(!r||r.event_id!==v.id||!integer(r.cursor)||!integer(r.end)||r.cursor>r.end||r.end>p.cursor||!integer(r.sequence)||r.sequence<1||r.sequence>p.sequence)
        throw new TemporalCaptureError("invalid_position");
      cursor=r.cursor;sequence=r.sequence;
    }
    if(sequence>0xffffffff)throw new TemporalCaptureError("sequence_bound");
    const {stream,...event}=v;
    const frame=JSON.stringify({version:1,...event,sequence})+"\n";
    const bytes=Buffer.byteLength(frame);
    if(bytes>MAX_FRAME||!integer(cursor+bytes))throw new TemporalCaptureError("input_bound");
    const body=JSON.stringify({source_id,stream,cursor,frame});
    return new PreparedTemporalEvent(states,{owner:this,config:this.#config,body,id:v.id,sequence,cursor,end:cursor+bytes});
  }
  prepare(event) {
    let frozen;try{frozen=input(event,this.#config.generation);}catch(e){return Promise.reject(e);}
    return this.enqueue(()=>this.#prepare(frozen));
  }
  record(event) {
    let frozen;try{frozen=input(event,this.#config.generation);}catch(e){return Promise.reject(e);}
    return this.enqueue(()=>this.#record(frozen));
  }
  async #record(frozen) {
    const prepared=await this.#prepare(frozen);
    const ack=await sendPrepared(prepared);this.#counts.committed++;return ack;
  }
  /** Diagnostic path: accepted for queueing is not a storage acknowledgement. */
  offer(event) {
    this.#counts.offered++;
    if(this.#pending>=this.#config.maxPending){this.#counts.dropped++;this.#report("queue_full");return false;}
    let frozen;
    try {frozen=input(event,this.#config.generation);}catch(error){this.#counts.failed++;this.#report(error instanceof TemporalCaptureError?error.code:"recording_failed");return false;}
    this.enqueue(()=>this.#record(frozen)).catch(error=>{this.#counts.failed++;this.#report(error instanceof TemporalCaptureError?error.code:"recording_failed");});
    return true;
  }
  #report(code) {try{this.#diagnostic?.(code);}catch{}}
  stats(){return Object.freeze({...this.#counts,pending:this.#pending});}
  async drain(){await this.#tail;return this.stats();}
}

function diagnostic(recorder,work) {
  try {work();}catch {try{diagnostics.get(recorder)?.();}catch{}}
}
export function createWorkflowSinks(recorder) {
  return {oojuro:{record:{callDuringReplay:false,fn(info,event){
    diagnostic(recorder,()=>recorder.offer({...event,stream:{namespace:info.namespace,workflow_id:info.workflowId,run_id:info.runId}}));
  }}}};
}
export function createActivityInterceptor(recorder) {
  return ctx=>({inbound:{async execute(args,next){
    let base,op;
    diagnostic(recorder,()=>{
      const info=ctx.info,execution=info.workflowExecution;
      const supported=execution&&name(info.namespace)&&name(info.activityId)&&name(info.activityType)&&integer(info.attempt)&&
        info.attempt>0&&Number.isSafeInteger(info.currentAttemptScheduledTimestampMs*1000);
      if(!supported)return;
      op=createHash("sha256").update(JSON.stringify([info.namespace,execution.workflowId,execution.runId,info.activityId,info.attempt])).digest("hex");
      base={stream:{namespace:info.namespace,workflow_id:execution.workflowId,run_id:execution.runId},
        valid_us:info.currentAttemptScheduledTimestampMs*1000,tool:info.activityType,operation_id:op,
        attributes:{activity_id:info.activityId,attempt:info.attempt,timestamp_basis:"current_attempt_scheduled"}};
      recorder.offer({...base,id:op+":start",kind:"tool_call",payload:{args:args.args}});
    });
    // Only business execution is inside this try/catch; diagnostic failures cannot replace it.
    let result;
    try {result=await next(args);}catch(error){
      diagnostic(recorder,()=>{
        if(base)recorder.offer({...base,id:op+":result",kind:"tool_result",status:"error",
          payload:{name:typeof error?.name==="string"?error.name:"Error",message:typeof error?.message==="string"?error.message:"Activity failed"}});
      });
      throw error;
    }
    diagnostic(recorder,()=>{
      if(base)recorder.offer({...base,attributes:{...base.attributes,result_encoding:result===undefined?"undefined":"json"},
        id:op+":result",kind:"tool_result",status:"ok",payload:result===undefined?null:result});
    });
    return result;
  }}});
}
