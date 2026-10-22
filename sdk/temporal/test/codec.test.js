import test from "node:test";
import assert from "node:assert/strict";
import {createHash} from "node:crypto";
import {inspect} from "node:util";
import {runInNewContext} from "node:vm";
import {OojuroClaimCheckCodec,TemporalStorageError} from "../codec.js";
import {store,clientOptions,W} from "../../typescript/test/support/file-model.js";
const key=()=>new Uint8Array(32).fill(71);
const sha=bytes=>createHash("sha256").update(bytes).digest("hex");
const codec=(state,extra={})=>new OojuroClaimCheckCodec({client:clientOptions(state),writer:W,namespace:"synthetic-pc",
  bindingKey:key(),consent:"intentional-user-file",...extra});
const payload=(size=(2<<20)+123)=>({metadata:{encoding:Buffer.from("binary/plain"),note:Buffer.from("SYNTHETIC_PRIVATE_METADATA")},
  data:Uint8Array.from({length:size},(_,i)=>(i*37+19)%256)});
const clean=error=>{
  assert.ok(error instanceof TemporalStorageError);assert.equal(error.cause,undefined);
  for(const text of [String(error),error.stack,inspect(error),JSON.stringify(error)])assert.doesNotMatch(text,/SYNTHETIC_PRIVATE/);
  return true;
};
test("actual FileSDK claim-check codec >2MiB preserves exact metadata/data with a small opaque History reference",async()=>{
  const s=store(),original=payload(),c=codec(s),encoded=await c.encode([original],{type:"workflow",namespace:"synthetic-pc",workflowId:"workflow"});
  assert.equal(encoded.length,1);assert.ok(encoded[0].data.length<512);assert.equal(Buffer.from(encoded[0].metadata.encoding).toString(),"binary/oojuro-claim-check-v1");
  const history=JSON.stringify(encoded);assert.ok(!history.includes(sha(original.data)));assert.ok(!history.includes("SYNTHETIC_PRIVATE_METADATA"));
  const decoded=await codec(s).decode(encoded);assert.deepEqual(decoded[0],{metadata:{encoding:Buffer.from("binary/plain"),note:Buffer.from("SYNTHETIC_PRIVATE_METADATA")},data:Buffer.from(original.data)});
  assert.ok(s.calls.every(x=>x.path.includes("/v2/")));
  assert.equal(s.blobs.size,10);
});
test("recreated codec retries same original identity after discarded part and manifest ACKs",async()=>{
  for(const phase of ["part","manifest"]){
    const s=store(),original=payload();let lost=false;
    s.after=async input=>{if(!lost&&(phase==="part"||input.request_id.endsWith("/manifest"))){lost=true;throw new Error("SYNTHETIC_PRIVATE_FAILURE");}};
    await assert.rejects(codec(s).encode([original]),e=>{clean(e);assert.equal(e.outcome,"unknown");return true;});
    const originals=new Map(s.requests);s.after=null;
    const encoded=await codec(s).encode([original]);assert.equal(s.requests.size,10);
    for(const [id,item] of originals)assert.equal(s.requests.get(id).body,item.body);
    const generation=s.generation;assert.deepEqual(await codec(s).encode([original]),encoded);assert.equal(s.generation,generation);
    assert.deepEqual((await codec(s).decode(encoded))[0].data,Buffer.from(original.data));
  }
});
test("encode preflights complete batch and freezes original bytes before any asynchronous upload",async()=>{
  const s=store(),original=payload(),before=Buffer.from(original.data),c=codec(s);
  const pending=c.encode([original]);original.data.fill(0);original.metadata.note.fill(0);
  const decoded=await c.decode(await pending);assert.deepEqual(decoded[0].data,before);assert.equal(Buffer.from(decoded[0].metadata.note).toString(),"SYNTHETIC_PRIVATE_METADATA");
  const other=store();let calls=0;const bad={get data(){calls++;throw new Error("SYNTHETIC_PRIVATE_GETTER");}};
  await assert.rejects(codec(other).encode([payload(),bad]),clean);assert.equal(calls,0);assert.equal(other.calls.length,0);
  await assert.rejects(codec(other).encode([payload(33<<20)]),clean);assert.equal(other.calls.length,0);
  await assert.rejects(codec(other).encode([{data:new Uint8Array(new SharedArrayBuffer(4))}]),clean);
});
test("forged refs, wrong key/project/writer/namespace fail before reads",async()=>{
  const s=store(),c=codec(s),encoded=await c.encode([payload()]),ref=JSON.parse(Buffer.from(encoded[0].data).toString());
  for(const field of ["handle","revision","tag","generation","version"]){
    const changed={...ref,[field]:typeof ref[field]==="number"?ref[field]+1:"f".repeat(64)};
    const calls=s.calls.length;await assert.rejects(c.decode([{metadata:encoded[0].metadata,data:Buffer.from(JSON.stringify(changed))}]),clean);assert.equal(s.calls.length,calls);
  }
  for(const extra of [{bindingKey:new Uint8Array(32).fill(72)},{writer:"3".repeat(64)},{namespace:"another"},
    {client:{...clientOptions(s),project:"4".repeat(64)}}]){
    const calls=s.calls.length;await assert.rejects(codec(s,extra).decode(encoded),clean);assert.equal(s.calls.length,calls);
  }
  const calls=s.calls.length;await assert.rejects(c.decode(encoded,{type:"workflow",namespace:"other",workflowId:"workflow"}),clean);assert.equal(s.calls.length,calls);
});
test("strict current revision and auth checks prevent substituted or revoked claim-check decode",async()=>{
  for(const mode of ["part","manifest","missing","revoked","mid-read","tampered"]){
    const s=store(),c=codec(s),encoded=await c.encode([payload()]);
    const manifest=[...s.blobs.keys()].find(k=>k.includes("/manifests/"));
    if(mode==="part")s.blobs.values().next().value.revision="f".repeat(64);
    if(mode==="manifest")s.blobs.get(manifest).revision="f".repeat(64);
    if(mode==="missing")s.omitRevision=true;
    if(mode==="revoked")s.readable=false;
    if(mode==="mid-read")s.afterRead=(name,current)=>{if(name!==manifest)current.readable=false;};
    if(mode==="tampered")s.blobs.values().next().value.bytes[0]^=255;
    await assert.rejects(c.decode(encoded),clean);
  }
});
test("small protobuf payload states and empty batch pass without storage; malformed namespaces/consent refuse",async()=>{
  const s=store(),c=codec(s);
  const small=[{}, {metadata:null,data:null},{metadata:{},data:Buffer.alloc(0)},{data:Buffer.from("small")}];
  assert.deepEqual(await c.decode(await c.encode(small)),small);assert.deepEqual(await c.encode([]),[]);assert.equal(s.calls.length,0);
  for(const extra of [{namespace:"bad\nnamespace"},{consent:undefined},{bindingKey:new Uint8Array(31)}])assert.throws(()=>codec(s,extra),clean);
  const reserved={metadata:{encoding:Buffer.from("binary/oojuro-claim-check-v1")},data:Buffer.from("{}")};
  await assert.rejects(c.encode([reserved]),clean);assert.equal(s.calls.length,0);
});

test("escaped metadata keys roundtrip under the same bounded encode/decode header profile",async()=>{
  const s=store(),metadata={};
  for(let i=0;i<100;i++)metadata["\0".repeat(512)+i]=Uint8Array.of(i);
  const original={metadata,data:Buffer.from("metadata roundtrip")};
  const encoded=await codec(s).encode([original]);
  assert.equal(Buffer.from(encoded[0].metadata.encoding).toString(),"binary/oojuro-claim-check-v1");
  const [decoded]=await codec(s).decode(encoded);
  assert.equal(Object.keys(decoded.metadata).length,100);
  for(const key of Object.keys(metadata)){assert.ok(Object.hasOwn(decoded.metadata,key));assert.equal(Buffer.compare(Buffer.from(decoded.metadata[key]),Buffer.from(metadata[key])),0);}
  assert.deepEqual(decoded.data,original.data);
});
test("shadowed TypedArray accessors cannot execute or bypass pre-copy payload/key/metadata bounds",async()=>{
  const s=store(),c=codec(s),huge=new Uint8Array((32<<20)+1);let getters=0,copies=0;
  for(const field of ["buffer","byteOffset","byteLength"])Object.defineProperty(huge,field,{get(){getters++;return 0;}});
  const original=Buffer.from;Buffer.from=(...args)=>{copies++;return original(...args);};
  try{await assert.rejects(c.encode([{data:huge}]),clean);}finally{Buffer.from=original;}
  assert.equal(getters,0);assert.equal(copies,0);assert.equal(s.calls.length,0);
  const metadata=new Uint8Array(65537);Object.defineProperty(metadata,"byteLength",{get(){getters++;return 0;}});
  await assert.rejects(c.encode([{metadata:{oversized:metadata}}]),clean);assert.equal(getters,0);assert.equal(s.calls.length,0);
  const small=Uint8Array.of(1,2,3);Object.defineProperty(small,"buffer",{get(){getters++;throw new Error("SYNTHETIC_PRIVATE");}});
  const [decoded]=await c.decode(await c.encode([{data:small}]));assert.deepEqual(decoded.data,Buffer.from([1,2,3]));assert.equal(getters,0);
  const wrongKey=new Uint8Array(33);Object.defineProperty(wrongKey,"byteLength",{get(){getters++;return 32;}});
  assert.throws(()=>codec(s,{bindingKey:wrongKey}),clean);assert.equal(getters,0);
  let projectReads=0;const dangerous=clientOptions(s);Object.defineProperty(dangerous,"project",{get(){projectReads++;return "1".repeat(64);}});
  assert.throws(()=>codec(s,{client:dangerous}),clean);assert.equal(projectReads,0);
});

test("cross-realm shared backing stores refuse for key, data and metadata before storage",async()=>{
  const s=store(),foreign=runInNewContext("new SharedArrayBuffer(32)"),shared=Buffer.from(foreign);
  const c=codec(s);
  await assert.rejects(c.encode([{data:shared}]),clean);
  await assert.rejects(c.encode([{metadata:{value:shared}}]),clean);
  assert.throws(()=>codec(s,{bindingKey:shared}),clean);
  assert.equal(s.calls.length,0);
});
