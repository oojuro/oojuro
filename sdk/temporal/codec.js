import {OojuroClient} from "@oojuro/v2-client";
import {prepareFileUpload,downloadFileTo,FILE_PART_BYTES} from "@oojuro/v2-client/files";
import {TemporalStorageError,fail,opaque,integer,digest,mac,same,encoding,payloadSnapshot,serialize,deserialize,isReference,bytesView} from "./codec-format.js";
export {TemporalStorageError} from "./codec-format.js";
const MAX_PAYLOAD=32<<20,MAX_BATCH=64<<20,MAX_ITEMS=1024;
const text=new TextDecoder("utf-8",{fatal:true});
const label=s=>typeof s==="string"&&s.length>0&&Buffer.byteLength(s)<=256&&!/[\p{Cc}]/u.test(s);
async function* parts(bytes){for(let i=0;i<bytes.length;i+=FILE_PART_BYTES)yield bytes.subarray(i,i+FILE_PART_BYTES);}
function safe(error){
  if(error instanceof TemporalStorageError)return error;
  let outcome="none";try{if(Object.getOwnPropertyDescriptor(error,"outcome")?.value==="unknown")outcome="unknown";}catch{}
  return new TemporalStorageError("storage_unavailable",outcome);
}
export class OojuroClaimCheckCodec {
  #client;#key;#scope;#writer;#namespace;#threshold;#maxPayload;#maxBatch;
  constructor(options){
    try{
      const field=(object,key)=>{if(!object||typeof object!=="object")fail("configuration");const d=Object.getOwnPropertyDescriptor(object,key);if(d&&!("value" in d))fail("configuration");return d?.value;};
      const suppliedClient=field(options,"client"),client={};
      for(const key of ["baseUrl","project","token","fetch","maxRequestBytes","maxResponseBytes"]){
        const value=field(suppliedClient,key);if(value!==undefined)client[key]=value;
      }
      const writer=field(options,"writer"),namespace=field(options,"namespace"),bindingKey=bytesView(field(options,"bindingKey"));
      if(field(options,"consent")!=="intentional-user-file"||!opaque(client.project)||!opaque(writer)||
        !label(namespace)||bindingKey.byteLength!==32)fail("configuration");
      const threshold=field(options,"thresholdBytes")??262144,maxPayload=field(options,"maxPayloadBytes")??MAX_PAYLOAD,maxBatch=field(options,"maxBatchBytes")??MAX_BATCH;
      if(!integer(threshold)||threshold<1024||threshold>1048576||!integer(maxPayload)||maxPayload<threshold||maxPayload>MAX_PAYLOAD||
        !integer(maxBatch)||maxBatch<maxPayload||maxBatch>MAX_BATCH)fail("configuration");
      this.#client=new OojuroClient(client);this.#key=Buffer.from(bindingKey);
      this.#scope=JSON.stringify([client.project,writer,namespace]);
      this.#writer=writer;this.#namespace=namespace;
      this.#threshold=threshold;this.#maxPayload=maxPayload;this.#maxBatch=maxBatch;
    }catch{fail("configuration");}
  }
  #context(context){if(context!==undefined&&(!context||context.namespace!==this.#namespace))fail("namespace_mismatch");}
  #snapshots(payloads){
    if(!Array.isArray(payloads)||payloads.length>MAX_ITEMS)fail("batch_bound");
    const snapshots=[];let total=0;
    for(const payload of payloads){
      const snapshot=payloadSnapshot(payload,Math.min(this.#maxPayload,this.#maxBatch-total));
      const frame=serialize(snapshot,this.#maxPayload);
      total+=frame.length;if(total>this.#maxBatch)fail("batch_bound");
      snapshots.push({snapshot,frame});
    }
    return snapshots;
  }
  #identity(bytes){
    const handle=mac(this.#key,"handle",this.#scope,bytes);
    return {handle,uploadId:mac(this.#key,"upload",this.#scope,handle),name:"temporal-claim-v1/"+handle};
  }
  #reference(handle,revision,generation){
    const body={version:1,handle,revision,generation};
    return {...body,tag:mac(this.#key,"reference",this.#scope,JSON.stringify(body))};
  }
  #parse(payload){
    if(Object.keys(payload.metadata??{}).join(",")!=="encoding"||!(payload.data instanceof Uint8Array)||payload.data.length>2048)fail("invalid_reference");
    let ref;try{ref=JSON.parse(text.decode(payload.data));}catch{fail("invalid_reference");}
    if(!ref||Object.keys(ref).sort().join(",")!=="generation,handle,revision,tag,version"||ref.version!==1||
      !opaque(ref.handle)||!opaque(ref.revision)||!opaque(ref.tag)||!integer(ref.generation)||ref.generation<1)fail("invalid_reference");
    const expected=this.#reference(ref.handle,ref.revision,ref.generation);
    if(!same(ref.tag,expected.tag)||!Buffer.from(payload.data).equals(Buffer.from(JSON.stringify(expected))))fail("invalid_reference");
    return ref;
  }
  async encode(payloads,context){
    try{
      this.#context(context);
      const snapshots=this.#snapshots(payloads);const output=[];
      for(const {snapshot,frame} of snapshots){
        if(isReference(snapshot.payload))fail("reserved_encoding");
        if(frame.length<this.#threshold){output.push(snapshot.payload);continue;}
        const id=this.#identity(frame);
        const ack=await prepareFileUpload(this.#client,{uploadId:id.uploadId,writer:this.#writer,name:id.name,expected:null,
          size:frame.length,sha256:digest(frame),consent:"intentional-user-file"}).send(parts(frame));
        if(ack.uploadId!==id.uploadId||ack.name!==id.name||ack.writer!==this.#writer||ack.sha256!==digest(frame)||
          ack.size!==frame.length||!opaque(ack.revision)||!integer(ack.generation)||ack.generation<1)fail("invalid_storage_ack","unknown");
        const ref=this.#reference(id.handle,ack.revision,ack.generation);
        output.push({metadata:{encoding:encoding.slice()},data:Buffer.from(JSON.stringify(ref))});
      }
      return output;
    }catch(error){throw safe(error);}
  }
  async decode(payloads,context){
    try{
      this.#context(context);
      const snapshots=this.#snapshots(payloads);const output=[];let decodedBytes=0;
      // Authenticate every reference before starting any reads.
      const refs=snapshots.map(({snapshot})=>isReference(snapshot.payload)?this.#parse(snapshot.payload):null);
      for(let i=0;i<snapshots.length;i++){
        const ref=refs[i];if(!ref){decodedBytes+=snapshots[i].frame.length;if(decodedBytes>this.#maxBatch)fail("batch_bound");output.push(snapshots[i].snapshot.payload);continue;}
        const chunks=[];let total=0,joined,original;
        try{
          await downloadFileTo(this.#client,{writer:this.#writer,name:"temporal-claim-v1/"+ref.handle,
            minGeneration:ref.generation,expectedRevision:ref.revision},{
            write:part=>{total+=part.length;if(total>this.#maxPayload||decodedBytes+total>this.#maxBatch)fail("payload_bound");chunks.push(Buffer.from(part));},
            commit:receipt=>{
              joined=Buffer.concat(chunks,total);
              const id=this.#identity(joined);
              if(!same(id.handle,ref.handle)||receipt.uploadId!==id.uploadId||receipt.size!==total||receipt.sha256!==digest(joined))fail("payload_integrity");
              original=deserialize(joined,this.#maxPayload);
              if(isReference(original))fail("reserved_encoding");
            },
            abort:()=>{for(const part of chunks)part.fill(0);chunks.length=0;joined?.fill(0);original=undefined;}
          });
          if(!original)fail("payload_integrity");
          decodedBytes+=total;output.push(original);
        }finally{for(const part of chunks)part.fill(0);joined?.fill(0);}
      }
      return output;
    }catch(error){throw safe(error);}
  }
}
