import {OojuroClient,OojuroError} from "@oojuro/v2-client";
const inspect=Symbol.for("nodejs.util.inspect.custom"),states=new WeakMap();
function invalid(){throw new OojuroError("invalid_request");}
function own(object,key){const d=Object.getOwnPropertyDescriptor(object,key);if(d&&!("value" in d))invalid();return d?.value;}
function prepareInput(input,maxResults){
  try{
    if(!input||typeof input!=="object"||Array.isArray(input)||Reflect.ownKeys(input).some(k=>!["requestId","query","k","minGeneration"].includes(k)))invalid();
    const requestId=own(input,"requestId"),query=own(input,"query"),k=own(input,"k")??maxResults,minGeneration=own(input,"minGeneration")??0;
    if(typeof requestId!=="string"||!requestId||requestId.length>1024||Buffer.byteLength(requestId)>1024||
      typeof query!=="string"||!query.trim()||query.length>4096||Buffer.byteLength(query)>4096||
      !Number.isSafeInteger(k)||k<1||k>maxResults||!Number.isSafeInteger(minGeneration)||minGeneration<0)invalid();
    return {requestId,query,k,minGeneration};
  }catch{invalid();}
}
export class PreparedRecall {
  constructor(token,state){if(token!==states)invalid();states.set(this,state);}
  async send(options){
    const state=states.get(this);
    // Actual metered SDK search makes one attempt; replay is status-only and cannot reexecute.
    const response=await state.search.send(options),hits=[];let used=0;
    for(const hit of response.value){
      const bytes=Buffer.byteLength(hit.excerpt);
      if(used+bytes>state.maxTextBytes)break;
      used+=bytes;hits.push({id:hit.id,writer:hit.writer,excerpt:hit.excerpt,excerpt_start:hit.excerpt_start,
        excerpt_end:hit.excerpt_end,continued:hit.continued,score_bits:hit.score_bits,kind:hit.trace.kind,
        valid_us:hit.trace.valid_us});
    }
    return {watermark:response.watermark,hits,truncated:hits.length<response.value.length,returned_by_server:response.value.length};
  }
  toJSON(){return {type:"PreparedRecall"};}
  [inspect](){return "PreparedRecall {}";}
}
export class OojuroRecall {
  #client;#maxResults;#maxTextBytes;
  constructor(options){
    try{
      const maxResults=options.maxResults??10,maxTextBytes=options.maxTextBytes??65536;
      if(!Number.isSafeInteger(maxResults)||maxResults<1||maxResults>100||!Number.isSafeInteger(maxTextBytes)||maxTextBytes<64||maxTextBytes>65536)invalid();
      this.#client=new OojuroClient(options.client);this.#maxResults=maxResults;this.#maxTextBytes=maxTextBytes;
    }catch{invalid();}
  }
  get maxResults(){return this.#maxResults;}
  prepare(input){
    const frozen=prepareInput(input,this.#maxResults);
    return new PreparedRecall(states,{search:this.#client.prepareSearch(frozen),maxTextBytes:this.#maxTextBytes});
  }
}
export function createRecallActivity(options){
  const recall=new OojuroRecall(options);
  return async input=>recall.prepare(input).send();
}
export function createRecallTool(options){
  const recall=new OojuroRecall(options),execute=async input=>recall.prepare(input).send();
  return Object.freeze({name:"oojuro_recall",description:"Search authorized Oojuro traces. Supply a stable requestId for this logical search; retries return status only.",
    inputSchema:{type:"object",additionalProperties:false,required:["requestId","query"],properties:{
      requestId:{type:"string",minLength:1,maxLength:1024},query:{type:"string",minLength:1,maxLength:4096},
      k:{type:"integer",minimum:1,maximum:recall.maxResults},minGeneration:{type:"integer",minimum:0}
    }},execute});
}
