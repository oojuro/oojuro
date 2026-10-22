import test from "node:test";
import assert from "node:assert/strict";
import {inspect} from "node:util";
import {reportedTrace,OojuroError} from "../../typescript/index.js";
import {OojuroRecall,createRecallActivity,createRecallTool} from "../recall.js";
const P="1".repeat(64),W="2".repeat(64),R="3".repeat(64),D="4".repeat(64),TOKEN="ojv2_"+"A".repeat(43);
const mark={generation:7,epoch:2,transaction:{id:R,digest:D}};
const hit=(text="hello 🦉")=>({id:R,writer:W,writer_name:"SYNTHETIC_PRIVATE_NAME",memory_name:null,
  trace:reportedTrace(text,{source:"SYNTHETIC_PRIVATE_SOURCE",session:"SYNTHETIC_PRIVATE_SESSION"}),
  score_bits:2.75,continued:false,excerpt_start:0,excerpt_end:Buffer.byteLength(text),excerpt:text});
const options=(fetch,extra={})=>({client:{baseUrl:"https://synthetic.invalid",project:P,token:TOKEN,fetch},...extra});
const reply=value=>new Response(JSON.stringify({watermark:mark,value}),{headers:{"content-type":"application/json","x-oojuro-generation":"7"}});
test("recall uses actual SDK search and preserves authorized IDs, exact UTF8 excerpts and watermark",async()=>{
  const calls=[],recall=new OojuroRecall(options(async(url,init)=>{calls.push({url,init});return reply([hit()]);}));
  const input={requestId:"original-search",query:"hello",k:2,minGeneration:6},prepared=recall.prepare(input);
  input.query="mutated";input.requestId="changed";input.minGeneration=9;
  assert.doesNotMatch(inspect(prepared),/hello|original/);assert.doesNotMatch(JSON.stringify(prepared),/hello|original/);
  const result=await prepared.send();assert.equal(calls.length,1);
  assert.ok(calls[0].url.endsWith("/search"));assert.deepEqual(JSON.parse(calls[0].init.body),{request_id:"original-search",query:"hello",k:2});
  assert.equal(calls[0].init.headers["x-oojuro-min-generation"],"6");
  assert.deepEqual(result.watermark,mark);assert.equal(result.hits[0].excerpt,"hello 🦉");assert.equal(result.hits[0].id,R);
  assert.equal(result.truncated,false);assert.doesNotMatch(JSON.stringify(result),/SYNTHETIC_PRIVATE/);
});
test("lost metered search ACK never auto retries; explicit same prepared retry remains status-only",async()=>{
  const sent=[];let calls=0;
  const recall=new OojuroRecall(options(async(_url,init)=>{sent.push(init.body);calls++;
    if(calls===1)throw new Error("SYNTHETIC_PRIVATE"+TOKEN);
    return new Response(JSON.stringify({error:{code:"search_completed",message:"SYNTHETIC_PRIVATE"+TOKEN}}),{status:409,headers:{"content-type":"application/json"}});
  }));
  const prepared=recall.prepare({requestId:"one-logical-search",query:"hello"});
  for(const code of ["transport","search_completed"]){
    await assert.rejects(prepared.send(),error=>{assert.ok(error instanceof OojuroError);assert.equal(error.code,code);assert.equal(error.outcome,"unknown");assert.doesNotMatch(String(error)+inspect(error),/SYNTHETIC_PRIVATE/);return true;});
    assert.equal(calls,code==="transport"?1:2);
  }
  assert.equal(sent[0],sent[1]);
});
test("whole ranked prefix respects context budget and never clips a UTF8 receipt range",async()=>{
  const recall=new OojuroRecall(options(async()=>reply([hit("a".repeat(50)),hit("🦉".repeat(10))]),{maxTextBytes:64}));
  const result=await recall.prepare({requestId:"bounded",query:"hello"}).send();
  assert.equal(result.hits.length,1);assert.equal(result.hits[0].excerpt_end,50);assert.equal(result.truncated,true);assert.equal(result.returned_by_server,2);
});
test("recall activity/tool preserve current permission failures and expose no credentials",async()=>{
  let calls=0;const configuration=options(async()=>{calls++;return new Response(null,{status:403});},{maxResults:3});
  const activity=createRecallActivity(configuration),tool=createRecallTool(configuration);
  assert.equal(tool.inputSchema.properties.k.maximum,3);
  assert.doesNotMatch(JSON.stringify(tool),new RegExp(TOKEN));
  for(const execute of [activity,tool.execute])await assert.rejects(execute({requestId:"authorized-search",query:"hello"}),e=>e instanceof OojuroError);
  assert.equal(calls,2);
});
test("invalid request identity, getters, limits and extra fields refuse before any metered request",async()=>{
  let calls=0;const recall=new OojuroRecall(options(async()=>{calls++;return reply([]);}));
  for(const input of [{query:"hello"},{requestId:"one",query:" "},{requestId:"one",query:"hello",k:11},
    {requestId:"one",query:"hello",extra:true},{requestId:"one",query:"x".repeat(4097)},
    {requestId:"one",get query(){throw new Error("SYNTHETIC_PRIVATE");}}]){
    assert.throws(()=>recall.prepare(input),e=>e instanceof OojuroError&&e.code==="invalid_request"&&!String(e).includes("SYNTHETIC_PRIVATE"));
  }
  assert.equal(calls,0);
});
