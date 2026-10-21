// Real upstream state/lock/atomic-write code, scripted vault decisions.
import {mkdtemp,readFile,writeFile,rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {resolve,join} from 'node:path';
import {pathToFileURL} from 'node:url';
const root=resolve(process.env.RPI_REFERENCE_DIR!);
const {ClaustrumEnrollmentManager,disconnectClaustrumEnrollment}=await import(pathToFileURL(join(root,'packages/core/src/common-auth-embedded/claustrum/enrollment.js')).href);
const {ClaustrumCredentialError,writeEnrollmentTokenFile}=await import(pathToFileURL(join(root,'packages/core/node_modules/@cortexkit/claustrum-client/dist/index.js')).href);
const proposedName='antigravity-auth-pi',secret='b'.repeat(64),token={token:'a'.repeat(64),token_generation:1};
const pending={version:1,phase:'pending',proposedName,requestSecret:secret,createdAt:1000,updatedAt:1000};
const approved={version:1,phase:'approved',proposedName,approvedName:proposedName,tokenGeneration:1,updatedAt:1000};
const proposal={requestId:'request-1'},pendingReply={status:'pending'},approvedReply={status:'approved',name:proposedName,token:token.token,tokenGeneration:1};
const cases:any[]=[];
const directory=await mkdtemp(join(process.env.RPI_PRIVATE_TMP??tmpdir(),'vault-ceremony-'));
async function capture(name:string,state:any,credential:any,replies:any[],steps:string[]=['reconcile'],failTokenWrite=false,mintedSecret=secret){
 const parent=join(directory,String(cases.length));const paths={statePath:join(parent,'state.json'),tokenPath:join(parent,'token.json')};
 const {mkdir}=await import('node:fs/promises');await mkdir(parent,{mode:0o700});
 for(const [path,value] of [[paths.statePath,state],[paths.tokenPath,credential]])if(value!==undefined)await writeFile(path,JSON.stringify(value)+'\n',{mode:0o600});
 const read=async(path:string)=>{try{return JSON.parse(await readFile(path,'utf8'));}catch(e:any){if(e.code==='ENOENT')return null;throw e;}};
 const snapshot=async()=>({state:await read(paths.statePath),token:await read(paths.tokenPath)});
 const trace:any[]=[],initial={state,token:credential},script=structuredClone(replies);let now=42000;
 const invoke=async(operation:string,input:any)=>{trace.push({operation,input,before:await snapshot()});const reply=replies.shift();if(reply?.error){const {code,action}=reply.error;throw new ClaustrumCredentialError(code,'transient',action);}if(!reply)throw Error('script exhausted');return reply;};
 const client={enrollPropose:(input:any)=>invoke('propose',input),enrollPoll:(input:any)=>invoke('poll',input)};
 const manager=new ClaustrumEnrollmentManager({client,paths,proposedName,now:()=>now,mintSecret:()=>mintedSecret,writeTokenFile:async(path:string,value:any)=>{trace.push({operation:'write-token',input:value,before:await snapshot()});if(failTokenWrite)throw Error('fixture-write-failed');await writeEnrollmentTokenFile(path,value);}});
 const results=[];
 for(const step of steps){let result;try{let value;if(step==='disconnect')value=await disconnectClaustrumEnrollment(paths,proposedName);else if(step==='reset')value=await manager.resetTerminal();else value=await manager[step]();result={value};}catch(e:any){result={error:{name:e.name,kind:e.kind,message:e.message}};}results.push({result,after:await snapshot()});now+=1000;}
 cases.push({name,initial,replies:script,steps,failTokenWrite,mintedSecret,trace,results});
}
try {
 await capture('new pending',undefined,undefined,[proposal,pendingReply]);
 await capture('approve next tick',undefined,undefined,[proposal,pendingReply,approvedReply],['reconcile','reconcile','status']);
 await capture('new approval',undefined,undefined,[proposal,approvedReply]);
 await capture('denied then reset',undefined,undefined,[proposal,{status:'denied'},proposal,approvedReply],['reconcile','reset','reconcile']);
 await capture('token write fails',undefined,undefined,[proposal,approvedReply],['reconcile'],true);
 await capture('secret rejected before write',undefined,undefined,[],['reconcile'],false,'bad');
 for(const state of [undefined,pending,{...pending,requestId:'request-1'},approved,{version:1,phase:'denied',proposedName,updatedAt:1000},{version:1,phase:'blocked',proposedName,errorCode:'missing_token',updatedAt:1000},null,{}, {...approved,proposedName:'another-consumer'}])for(const credential of [undefined,token,null])for(const step of ['status','reset','disconnect'])await capture(`${step} existing`,state,credential,[],[step]);
 for(const state of [undefined,pending,approved,{version:1,phase:'denied',proposedName,updatedAt:1000},{...approved,proposedName:'another-consumer'}])await capture('existing token reconcile',state,token,[]);
 await capture('missing token refuses',approved,undefined,[]);
 for(const value of ['b'.repeat(64),'0'.repeat(63)+'1','0'.repeat(61)+'257'])await capture('legacy array secret coercion',{...pending,requestSecret:[value]},undefined,[proposal,pendingReply]);
 for(const operation of ['propose','poll'])for(const code of ['invalid_params','pending_exists','not_found','already_consumed','superseded','stale_generation','pending_queue_full','connection_dropped','other'])for(const action of ['retry','gone','reauth','reduce_and_retry']){
  const state=operation==='poll'?{...pending,requestId:'request-1'}:pending;
  await capture(`${operation}:${code}:${action}`,state,undefined,[{error:{code,action}}]);
 }
 await writeFile(resolve(process.env.RPI_REFERENCE_OUTPUT!),JSON.stringify({runtime:process.version,proposedName,cases},null,2)+'\n');
 console.log(`${cases.length} original durable ceremony scenarios captured`);
} finally {await rm(directory,{recursive:true,force:true});}
