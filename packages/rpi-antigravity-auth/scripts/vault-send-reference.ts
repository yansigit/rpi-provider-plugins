import {mkdtemp,writeFile,readFile,rm} from 'node:fs/promises';
import {join,resolve} from 'node:path';import {pathToFileURL} from 'node:url';import {setImmediate as immediate} from 'node:timers/promises';
const root=resolve(process.env.RPI_REFERENCE_DIR!),base=join(root,'packages/core/src');
const {createAntigravityVaultAccountSource}=await import(pathToFileURL(join(base,'vault-account-source.ts')).href);
const claustrum=await import(pathToFileURL(join(base,'common-auth-embedded/claustrum/index.js')).href);
const consumer=JSON.parse(await readFile(new URL('../tests/fixtures/vault-consumer.json',import.meta.url),'utf8'));
const {inventory,receipt,token}=consumer;
const reference={routeId:consumer.route,credentialId:'id',accountIdentity:'account',label:'id'};
const row={...reference,credentialType:'oauth',state:'active',enabled:true,addedAt:1000};
const roster={version:1,view:'v1',complete:true,rows:[row],declined:[]};
const metadata={credentialId:'id',credentialType:'oauth',accountIdentity:'account',accountIdentitySource:'asserted',expectedAccountIdentity:'account',assertedCredentialId:'id',assertedAccountIdentity:'account',projectId:'project',recordVersion:3,expiresAtMs:901000};
const error=(e:any)=>({name:e.name,message:e.message,...(e.kind!==undefined&&{kind:e.kind}),...(e.code!==undefined&&{code:e.code}),...(e.class!==undefined&&{class:e.class}),...(e.action!==undefined&&{action:e.action})});
const fail=(v:any)=>new claustrum.ClaustrumCredentialError(v.code??'not_found',v.class??'permanent',(v.class??'permanent')==='permanent'?'gone':'retry');
const cases:any[]=[],validations:any[]=[],routeCases:any[]=[];
async function run(name:string,steps:any[]){
 const dir=await mkdtemp('/home/user/provider-test-tmp/vault-send-'),rosterPath=join(dir,'roster.json'),tokenPath=join(dir,'token.json');await writeFile(tokenPath,JSON.stringify(token),{mode:0o600});
 const calls:any[]=[],responses:any={list:[],get:[],report:[]};let active=true,clientId=0;const signal=new AbortController();const admissions=new Map<string,any>();
 const reply=(op:string,fallback:any)=>{const v=responses[op].shift()??{};if(v.error)throw fail(v.error);return v.value??fallback;};
 const options={claustrum,host:'pi',rosterPath,tokenPath,reporterSource:'direct',now:()=>1000,isCustodyActive:async()=>{calls.push(['active']);return active;},
  connect:async()=>{const id=++clientId;calls.push(['connect',id]);return{listScoped:async(t:string)=>{calls.push(['list',id,t]);return reply('list',inventory);},getScoped:async(v:any)=>{calls.push(['get',id,v]);return reply('get',receipt);},reportAuthFailureScoped:async(v:any)=>{calls.push(['report',id,v]);reply('report',null);},close(){calls.push(['close',id]);}};},
  onRoster:(v:any)=>calls.push(['roster',v]),onError:(e:any)=>calls.push(['error',error(e)]),logger:Object.fromEntries(['debug','info','warn'].map(level=>[level,(message:string,fields:any)=>calls.push(['log',level,message,fields])]))};
 const source=createAntigravityVaultAccountSource(options),foreign=createAntigravityVaultAccountSource(options);const results:any[]=[],states:any[]=[];
 for(const step of steps){
  try{let value:any;
   switch(step.op){
    case'set':if('active'in step)active=step.active;for(const op of ['list','get','report'])if(op in step)responses[op].push(...step[op]);break;
    case'refresh':value=await source.refresh();break;
    case'admit':{const admission=await source.admit(step.reference??reference,signal.signal);admissions.set(step.id??'admission',admission);value={metadata:JSON.parse(JSON.stringify(admission)),accessToken:admission.accessToken};break;}
    case'report':value=await(step.foreign?foreign:source).reportServedStatus(admissions.get(step.id??'admission'),step.status??401);break;
    case'accept':await source.accept(step.reference??reference);break;
    case'decline':await source.decline(step.reference??reference);break;
    case'cancel':signal.abort();break;
    case'close':source.close();break;
    case'send':{
     let index=0;const response=await source.send(step.reference??reference,async(admission:any)=>{
      const spec=step.responses[index++]??{status:200};calls.push(['dispatch',JSON.parse(JSON.stringify(admission)),admission.accessToken]);
      if(spec.abort)signal.abort();if(spec.close)source.close();if(spec.error)throw Error(spec.error);
      return {status:spec.status,id:index,body:{async cancel(){calls.push(['cancel-body',index]);if(spec.cancelAbort)signal.abort();if(spec.cancelError)throw Error(spec.cancelError);}}};
     },{site:'model',signal:signal.signal});value={status:response.status,id:response.id};break;
    }
    default:throw Error('unknown step');
   }
   if(!['set','cancel','close'].includes(step.op))results.push({value:value??null});
  }catch(e){results.push({error:error(e)});}
  await immediate();states.push({routes:source.routes(),reach:source.grantReach()??null,cause:source.emptyRosterCause()});
 }
 source.close();foreign.close();await immediate();const after=await readFile(rosterPath,'utf8').catch(()=>null);cases.push({name,steps,results,states,calls,after});await rm(dir,{recursive:true,force:true});
}
const specs:any[]=[];const add=(n:string,s:any[])=>specs.push([n,s]);const refresh={op:'refresh'},admit={op:'admit'},send=(...responses:any[])=>({op:'send',responses});
add('no-roster',[admit]);add('fresh-send',[refresh,send({status:200}),send({status:200})]);
for(const status of [200,400,401,402,403,429,500])add(`status-${status}`,[refresh,send({status})]);
for(const version of [0,2,4])add(`rotated-${version}`,[refresh,{op:'set',get:[{}, {value:{...receipt,recordVersion:version}}]},send({status:401},{status:200})]);
add('second-401-reports-retried-version',[refresh,{op:'set',get:[{}, {value:{...receipt,recordVersion:4}}]},send({status:401},{status:401})]);
add('same-version-new-material',[refresh,{op:'set',get:[{},{value:{...receipt,material:'other-token'}}]},send({status:401})]);
for(const [name,value]of Object.entries({account:{...receipt,accountId:'other'},credential:{...receipt,credentialId:'other'},project:{...receipt,projectId:''},expiry:{...receipt,expiresAtMs:1}}))add('failed-readmission-'+name,[refresh,{op:'set',get:[{},{value}]},send({status:401})]);
add('failed-readmission-refused',[refresh,{op:'set',get:[{},{error:{code:'not_found'}}]},send({status:401})]);
add('report-failure-swallowed',[refresh,{op:'set',report:[{error:{code:'not_found'}}]},send({status:401})]);
add('cancel-before-send',[refresh,{op:'cancel'},send({status:200})]);
add('cancel-at-401',[refresh,send({status:401,abort:true})]);
add('close-at-401',[refresh,send({status:401,close:true})]);
add('dispatch-failure',[refresh,send({error:'fixture dispatch failed'})]);
for(const [name,extra]of Object.entries({failure:{cancelError:'fixture cancellation failed'},abort:{cancelAbort:true}}))add('retry-body-'+name,[refresh,{op:'set',get:[{},{value:{...receipt,recordVersion:4}}]},send({status:401,...extra},{status:200})]);
add('kept-401',[refresh,admit,{op:'set',get:[{error:{code:'ECONNREFUSED',class:'transient'}}]},send({status:401})]);
add('report-once',[refresh,admit,{op:'report'},{op:'report'},{op:'close'},{op:'report'}]);
add('report-failure-stays-reported',[refresh,admit,{op:'set',report:[{error:{code:'not_found'}}]},{op:'report'},{op:'report'}]);
for(const status of [200,401])add('foreign-report-'+status,[refresh,admit,{op:'report',foreign:true,status}]);
add('non-401-after-close',[refresh,admit,{op:'close'},{op:'report',status:200}]);
add('inactive-send',[refresh,{op:'set',active:false},send({status:200})]);
add('decline-accept',[refresh,{op:'decline'},send({status:200}),{op:'accept'},send({status:200})]);
for(const grants of [undefined,0,1])add(`empty-roster-${grants}`,[{op:'set',list:[{value:{view:'empty',rows:[],...(grants===undefined?{}:{grants,grantTuples:[]})}}]},refresh]);
for(const args of specs)await run(...args as [string,any[]]);
// A fake consumer exposes only the source's extra admission boundary; no transport decoding is replaced above.
const raw=(receipt:any,rosterValue:any=roster)=>createAntigravityVaultAccountSource({claustrum:{...claustrum,ClaustrumConsumer:class{snapshot(){return rosterValue;}async authorize(){return receipt;}close(){}grantReach(){return undefined;}}},host:'pi',rosterPath:'unused',tokenPath:'unused',connect:async()=>{},isCustodyActive:()=>true,reporterSource:'direct'});
const samples:any[]=[{...metadata,accessToken:'fixture'}];
for(const key of Object.keys({...metadata,accessToken:'fixture',fromKeptReceipt:true}))for(const value of [undefined,null,false,true,0,-1,1.5,9007199254740992,'',' ','x','account','id','oauth','asserted']){const sample={...metadata,accessToken:'fixture',[key]:value};samples.push(sample);}
for(const c of ['\t','\n','\r','\u0085','\u00a0','\u200b','\u2028','\ufeff'])samples.push({...metadata,accessToken:'fixture',projectId:'a'+c+'b'});
for(const receipt of samples){const source=raw(receipt);let result;try{const admission=await source.admit(reference);result={value:JSON.parse(JSON.stringify(admission))};}catch(e){result={error:error(e)};}source.close();validations.push({receipt,result});}
const rows:any[]=[row];for(const key of ['enabled','state','credentialType','stale','unclaimed','accountIdentity'])for(const value of [undefined,null,false,true,'',' ','account','active','oauth','api_key'])rows.push({...row,[key]:value});
for(const candidate of rows)for(const declined of [[],[{credentialId:'id'}],[{credentialId:'id',accountIdentity:'other'}],[{credentialId:'id',accountIdentity:'account'}]]){const value={...roster,rows:[candidate],declined};const source=raw(metadata,value);routeCases.push({roster:value,result:source.routes()});source.close();}
await writeFile(process.env.RPI_REFERENCE_OUTPUT!,JSON.stringify({reference:'a97f9055017f3c1546afbfe61a19d50b428be3d8',runtime:process.version,inventory,receipt,token,referenceRoute:reference,cases,validations,routeCases})+'\n');console.log(`${cases.length} send traces; ${validations.length} admission checks; ${routeCases.length} route projections`);
