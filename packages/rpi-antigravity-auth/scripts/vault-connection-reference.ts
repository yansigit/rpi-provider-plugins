// Original enrolled-client policy; only connection outcomes and time are scripted.
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {readFile, writeFile} from 'node:fs/promises';
import {join, resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
const root=resolve(process.env.RPI_REFERENCE_DIR!);
const dist=join(root,'packages/core/node_modules/@cortexkit/claustrum-client/dist');
const {ClaustrumClient,ClaustrumCredentialError}=await import(pathToFileURL(join(dist,'index.js')).href);
const {SubcCallError}=await import(pathToFileURL(join(root,'packages/core/node_modules/@cortexkit/subc-client/dist/index.js')).href);
const success={result:{view:'view',credentials:[]}};
const identity={project_root:'/fixture/project',harness:'opencode',session:'store-fixture'};
const cases:any[]=[];
const originalNow=Date.now;
const failure=(error:any)=>error.type==='subc'?new SubcCallError(error.kind,'private diagnostic',error.code):error.type==='credential'?new ClaustrumCredentialError('fixture_code','permanent','gone'):Error('private diagnostic');
async function capture(name:string,responses:any[][],connections:any[]=[],steps:any[]=['call']){
 let now=100000,connection=0;const trace:any[]=[],results:any[]=[];
 Date.now=()=>now;
 const transport=(id:number)=>({async call(moduleId:string,method:string,params:any,opts:any){trace.push(['call',id,moduleId,method,params,opts]);const reply=responses[id]?.shift();assert(reply!==undefined,'script exhausted');if(reply.error)throw failure(reply.error);return reply;},close(){trace.push(['close',id]);}});
 const connector=async(options:any)=>{trace.push(['connect',options]);const reply=connections.shift();if(reply?.error)throw failure(reply.error);return transport(++connection);};
 const client=new ClaustrumClient(transport(0),connector,'/fixture/connection',10000,identity,()=>{});
 const input={responses:structuredClone(responses),connections:structuredClone(connections),steps};
 for(const step of steps){if(step==='close'){client.close();continue;}if(typeof step==='number'){now=step;continue;}try{results.push({value:await client.listScoped('fixture-token')});}catch(e:any){results.push({error:{code:e.code,class:e.class,action:e.action,message:e.message}});}}
 client.close();cases.push({name,...input,trace,results});
}
const codes=[undefined,'','missing_identity','invalid_control_body','unknown_module','module_warming','route_closed','unknown_channel','connection_dropped','deadline_exceeded_no_drop_observed','other'];
for(const kind of ['terminal','not_sent','outcome_unknown'])for(const code of codes)await capture(`${kind}:${code}` ,[[{error:{type:'subc',kind,code}}],[success]]);
for(const type of ['other','credential'])await capture(type,[[{error:{type}}]]);
for(const kind of ['terminal','not_sent','outcome_unknown'])for(const code of codes)await capture(`retry:${kind}:${code}`,[[{error:{type:'subc',kind:'terminal'}}],[{error:{type:'subc',kind,code}}]]);
for(const type of ['other','credential','subc'])await capture(`connect:${type}`,[[{error:{type:'subc',kind:'terminal'}}]], [{error:{type,kind:'terminal',code:'connect_failed'}}]);
await capture('success',[[success]]);
await capture('invalid reply is not retried',[[{result:{}}]]);
await capture('backoff boundary',[[{error:{type:'subc',kind:'terminal'}}],[success,{error:{type:'subc',kind:'terminal'}},{error:{type:'subc',kind:'terminal'}}],[success]],[],['call',159999,'call',160000,'call']);
await capture('closed suppresses reconnect',[[{error:{type:'subc',kind:'terminal',code:'closed'}}]],[],['close','call']);
Date.now=originalNow;
await writeFile(resolve(process.env.RPI_REFERENCE_OUTPUT!),JSON.stringify({runtime:process.version,sourceSha256:createHash('sha256').update(await readFile(join(dist,'wire.js'))).digest('hex'),cases},null,2)+'\n');
console.log(`${cases.length} original connection-policy cases captured`);
