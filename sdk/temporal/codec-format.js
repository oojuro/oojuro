import {createHash,createHmac,timingSafeEqual} from "node:crypto";
import {types} from "node:util";
export const ENCODING="binary/oojuro-claim-check-v1";
export const encoding=new TextEncoder().encode(ENCODING);
const HEADER_BOUND=512<<10;
const magic=Buffer.from("oojuro-temporal-payload-1\n");
const codes=new Set(["configuration","namespace_mismatch","batch_bound","payload_bound","metadata_bound","invalid_payload",
  "payload_integrity","reserved_encoding","invalid_reference","invalid_storage_ack","storage_unavailable"]);
export class TemporalStorageError extends Error {
  constructor(code,outcome="none"){
    const safeCode=codes.has(code)?code:"invalid_payload";
    super("Temporal storage "+safeCode);this.name="TemporalStorageError";this.code=safeCode;
    this.outcome=outcome==="unknown"?"unknown":"none";Object.freeze(this);
  }
}
export const fail=(code="invalid_payload",outcome="none")=>{throw new TemporalStorageError(code,outcome);};
export const opaque=s=>typeof s==="string"&&/^[0-9a-f]{64}$/.test(s);
export const integer=n=>Number.isSafeInteger(n)&&n>=0;
export const digest=bytes=>createHash("sha256").update(bytes).digest("hex");
export function mac(key,domain,...parts){
  const h=createHmac("sha256",key).update("oojuro/temporal/"+domain+"/v1\0");
  for(const part of parts){const bytes=typeof part==="string"?Buffer.from(part):part;h.update(String(bytes.length)+":");h.update(bytes);}
  return h.digest("hex");
}
export function same(a,b){return opaque(a)&&opaque(b)&&timingSafeEqual(Buffer.from(a,"hex"),Buffer.from(b,"hex"));}
function own(object,key){const d=Object.getOwnPropertyDescriptor(object,key);if(d&&!("value" in d))fail();return d?.value;}
function state(value){return value===undefined?0:value===null?1:2;}
const typedArray=Object.getPrototypeOf(Uint8Array.prototype);
const bufferGet=Object.getOwnPropertyDescriptor(typedArray,"buffer").get;
const offsetGet=Object.getOwnPropertyDescriptor(typedArray,"byteOffset").get;
const lengthGet=Object.getOwnPropertyDescriptor(typedArray,"byteLength").get;
/** Intrinsic access ignores caller-shadowed TypedArray getters before bounds/copy. */
export function bytesView(value){
  if(!(value instanceof Uint8Array))fail();
  const buffer=Reflect.apply(bufferGet,value,[]),offset=Reflect.apply(offsetGet,value,[]),length=Reflect.apply(lengthGet,value,[]);
  if(types.isSharedArrayBuffer(buffer))fail();
  return new Uint8Array(buffer,offset,length);
}
export function payloadSnapshot(payload,maxBytes){
  if(!payload||typeof payload!=="object"||Array.isArray(payload)||Reflect.ownKeys(payload).some(k=>!["metadata","data"].includes(k)))fail();
  const metadata=own(payload,"metadata"),suppliedData=own(payload,"data");
  const data=suppliedData==null?suppliedData:bytesView(suppliedData);
  if((data?.byteLength??0)>maxBytes)fail("payload_bound");
  const entries=[];let bytes=data?.byteLength??0,metadataBytes=0;
  if(metadata!=null){
    if(typeof metadata!=="object"||Array.isArray(metadata))fail();
    const keys=Reflect.ownKeys(metadata);
    if(keys.length>128)fail("metadata_bound");
    for(const key of keys){
      if(typeof key!=="string"||key.length>1024||Buffer.byteLength(key)>4096)fail("metadata_bound");
      const value=bytesView(own(metadata,key));
      if(value.byteLength>65536)fail("metadata_bound");
      metadataBytes+=Buffer.byteLength(key)+value.byteLength;
      if(metadataBytes>65536)fail("metadata_bound");
      bytes+=Buffer.byteLength(key)+value.byteLength;
      if(bytes>maxBytes)fail("payload_bound");
      entries.push([key,Buffer.from(value)]);
    }
    entries.sort((a,b)=>a[0]<b[0]?-1:a[0]>b[0]?1:0);
    if(entries.reduce((n,[k,v])=>n+Buffer.byteLength(k)+v.length,0)>65536)fail("metadata_bound");
  }
  const out={};
  if(metadata!==undefined)out.metadata=metadata===null?null:Object.fromEntries(entries);
  if(data!==undefined)out.data=data===null?null:Buffer.from(data);
  return {payload:out,entries,metadataState:state(metadata),dataState:state(data),bytes};
}
export function serialize(snapshot,maxBytes){
  const header=Buffer.from(JSON.stringify({metadata_state:snapshot.metadataState,data_state:snapshot.dataState,
    metadata:snapshot.entries.map(([k,v])=>[k,v.toString("base64")])}));
  if(header.length>HEADER_BOUND)fail("metadata_bound");
  const data=snapshot.payload.data??Buffer.alloc(0),size=magic.length+4+header.length+data.length;
  if(size>maxBytes)fail("payload_bound");
  const prefix=Buffer.allocUnsafe(4);prefix.writeUInt32BE(header.length);
  return Buffer.concat([magic,prefix,header,data],size);
}
export function deserialize(bytes,maxBytes){
  if(bytes.length>maxBytes||bytes.length<magic.length+4||!bytes.subarray(0,magic.length).equals(magic))fail("payload_integrity");
  const length=bytes.readUInt32BE(magic.length),start=magic.length+4,end=start+length;
  if(length>HEADER_BOUND||end>bytes.length)fail("payload_integrity");
  let h;try{h=JSON.parse(new TextDecoder("utf-8",{fatal:true}).decode(bytes.subarray(start,end)));}catch{fail("payload_integrity");}
  if(!h||Object.keys(h).sort().join(",")!=="data_state,metadata,metadata_state"||
    ![0,1,2].includes(h.metadata_state)||![0,1,2].includes(h.data_state)||!Array.isArray(h.metadata)||h.metadata.length>128)fail("payload_integrity");
  const metadata=Object.create(null);let previous;
  for(const pair of h.metadata){
    if(!Array.isArray(pair)||pair.length!==2||typeof pair[0]!=="string"||typeof pair[1]!=="string"||
      previous!==undefined&&previous>=pair[0]||pair[1].length>90000)fail("payload_integrity");
    const value=Buffer.from(pair[1],"base64");if(value.toString("base64")!==pair[1])fail("payload_integrity");
    metadata[pair[0]]=value;previous=pair[0];
  }
  if(h.metadata_state!==2&&h.metadata.length||h.data_state!==2&&end!==bytes.length)fail("payload_integrity");
  const payload={};
  if(h.metadata_state!==0)payload.metadata=h.metadata_state===1?null:metadata;
  if(h.data_state!==0)payload.data=h.data_state===1?null:bytes.subarray(end);
  const checked=payloadSnapshot(payload,maxBytes);
  if(!serialize(checked,maxBytes).equals(bytes))fail("payload_integrity");
  return checked.payload;
}
export function isReference(payload){
  const value=payload.metadata?.encoding;
  return value instanceof Uint8Array&&Buffer.from(value).equals(encoding);
}
