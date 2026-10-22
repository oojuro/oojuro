import type {PayloadCodec,DataConverter,SerializationContext} from "@temporalio/common";
import {OojuroClaimCheckCodec,TemporalStorageError} from "../codec.js";
import {OojuroRecall,createRecallActivity,createRecallTool,type RecallResult} from "../recall.js";
const client={baseUrl:"https://synthetic.invalid",project:"1".repeat(64),token:"ojv2_"+"A".repeat(43)};
const codec:PayloadCodec=new OojuroClaimCheckCodec({client,writer:"2".repeat(64),namespace:"synthetic",
 bindingKey:new Uint8Array(32),consent:"intentional-user-file"});
const converter:DataConverter={payloadCodecs:[codec]};
const context:SerializationContext={type:"activity",namespace:"synthetic",workflowId:"workflow",activityId:"activity",isLocal:false};
void codec.encode([],context);void codec.decode([],context);void converter;
const recall=new OojuroRecall({client});const prepared=recall.prepare({requestId:"stable",query:"synthetic"});
const activity=createRecallActivity({client}),tool=createRecallTool({client});
const results:Promise<RecallResult>[]=[prepared.send(),activity({requestId:"stable",query:"synthetic"}),tool.execute({requestId:"another",query:"synthetic",k:1})];
void results;void TemporalStorageError;
