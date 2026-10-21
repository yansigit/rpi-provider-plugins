// Pi-reachable consumer methods, unchanged common-auth and real private files.
import {mkdtemp,writeFile,readFile,rm,chmod} from 'node:fs/promises';
import {join,resolve} from 'node:path';import {pathToFileURL} from 'node:url';import {setImmediate as immediate} from 'node:timers/promises';
const root=resolve(process.env.RPI_REFERENCE_DIR!);const base=join(root,'packages/core/src/common-auth-embedded/claustrum');
const {ClaustrumConsumer}=await import(pathToFileURL(join(base,'consumer.js')).href);
const {ClaustrumConsumerError}=await import(pathToFileURL(join(base,'errors.js')).href);
const {ClaustrumCredentialError}=await import(pathToFileURL(join(root,'packages/core/node_modules/@cortexkit/claustrum-client/dist/errors.js')).href);
const inventory={view:'v1',rows:[{id:'id',credentialType:'oauth',categories:['antigravity-native'],operations:['read'],refreshAdapter:'antigravity',state:'active',accountId:'account'}],grants:1,grantTuples:[{selectorKind:'category',selector:'antigravity-native',operation:'read'}]};
const receipt={credentialId:'id',accountId:'account',material:'fixture-access',recordVersion:3,expiresAtMs:901000,projectId:'project'};
const route='vault:id~9af211329b';const token={token:'a'.repeat(64),token_generation:1};
const failure=(value:any)=>value?.code?new ClaustrumCredentialError(value.code,value.class??'permanent',(value.class??'permanent')==='permanent'?'gone':'retry'):new ClaustrumConsumerError('fixture','fixture failure');
const err=(e:any)=>({name:e.name,message:e.message,...(e.kind!==undefined&&{kind:e.kind}),...(e.code!==undefined&&{code:e.code}),...(e.class!==undefined&&{class:e.class}),...(e.action!==undefined&&{action:e.action})});
const cases:any[]=[];
async function run(name:string,steps:any[]){
 const dir=await mkdtemp('/home/user/provider-test-tmp/consumer-reference-'),rosterPath=join(dir,'roster.json'),tokenPath=join(dir,'token.json');await writeFile(tokenPath,JSON.stringify(token),{mode:0o600});
 let active:any=true,now=1000;const responses:any={connect:[],list:[],get:[],report:[]},calls:any[]=[],held=new Map<string,()=>void>(),attempts=new Map<string,any>(),pending=new Map<string,Promise<any>>();let clientId=0;
 async function response(op:string,fallback:any){const spec=responses[op].shift()??{};if(spec.hold)await new Promise<void>(r=>held.set(spec.hold,r));if(spec.error)throw failure(spec.error);return spec.value??fallback;}
 const consumer=new ClaustrumConsumer({rosterPath,tokenPath,family:{refreshAdapter:'antigravity',category:'antigravity-native',apiKeys:false},requireAssertion:true,now:()=>now,
  isCustodyActive:async()=>{calls.push(['active']);return Array.isArray(active)?(active.length>1?active.shift():active[0]):active;},
  connect:async()=>{const id=++clientId;calls.push(['connect',id]);await response('connect',null);return {
   listScoped:async(t:string)=>{calls.push(['list',id,t]);return response('list',inventory);},
   getScoped:async(input:any)=>{calls.push(['get',id,input]);return response('get',receipt);},
   reportAuthFailureScoped:async(input:any)=>{calls.push(['report',id,input]);await response('report',null);},
   close(){calls.push(['close',id]);}
  };},onRoster:(roster:any)=>calls.push(['roster',roster]),logger:Object.fromEntries(['warn','info','debug'].map(level=>[level,(message:string,fields:any)=>calls.push(['log',level,message,fields])]))});
 const signals=new Map<string,AbortController>();
 async function invoke(step:any){try{let value:any;switch(step.method??step.op){case'refresh':value=await consumer.refresh();break;case'authorize':{const signal=signals.get(step.signal??'signal')??new AbortController();signals.set(step.signal??'signal',signal);const attempt=await consumer.authorize(step.route??route,signal.signal);attempts.set(step.attempt??'attempt',attempt);value={metadata:JSON.parse(JSON.stringify(attempt)),accessToken:attempt.accessToken};break;}case'accept':await consumer.accept(step.route??route);break;case'decline':await consumer.decline(step.route??route);break;case'report':await consumer.reportFailure(attempts.get(step.attempt??'attempt'),step.status??401,'direct');break;default:throw Error('unknown method');}return {value:value??null};}catch(e){return{error:err(e)};}}
 const results:any[]=[],states:any[]=[];
 for(const step of steps){
  switch(step.op){
   case'set':if('active'in step)active=structuredClone(step.active);if('now'in step)now=step.now;for(const op of Object.keys(responses))if(op in step)responses[op].push(...step[op]);break;
   case'begin':{const promise=invoke(step);pending.set(step.id,promise);if(step.until)for(let i=0;!held.has(step.until);i++){if(i>10000)throw Error('hold not reached: '+step.until);await immediate();}break;}
   case'join':results.push(await pending.get(step.id));pending.delete(step.id);break;
   case'release':held.get(step.id)!();held.delete(step.id);break;
   case'cancel':{let signal=signals.get(step.signal??'signal');if(!signal){signal=new AbortController();signals.set(step.signal??'signal',signal);}signal.abort();break;}
   case'close':consumer.close();break;
   case'peer':{const roster=JSON.parse(await readFile(rosterPath,'utf8'));if(step.remove)await rm(rosterPath);else if(step.raw!==undefined)await writeFile(rosterPath,step.raw);else{if(step.fields)Object.assign(roster.rows[0],step.fields);if(step.clear)roster.rows=[];if('view'in step)roster.view=step.view;await writeFile(rosterPath,JSON.stringify(roster)+'\n');}break;}
   case'busy':await writeFile(rosterPath+'.claustrum-roster.lock',JSON.stringify({ownerId:'peer',expiresAt:Date.now()+60000}),{mode:0o600});break;
   case'token':if(step.remove)await rm(tokenPath);else{await writeFile(tokenPath,JSON.stringify(step.value));await chmod(tokenPath,0o600);}break;
   default:results.push(await invoke(step));
  }
  await immediate();states.push(step.op==='release'||step.op==='begin'&&!step.until?null:{snapshot:consumer.snapshot()??null,reach:consumer.grantReach()??null});
 }
 consumer.close();await immediate();if(pending.size||held.size)throw Error('unfinished fixture');const after=await readFile(rosterPath,'utf8').catch(()=>null);cases.push({name,steps,results,states,calls,after});await rm(dir,{recursive:true,force:true});
}
const specs:any[]=[];const add=(name:string,steps:any[])=>specs.push([name,steps]);
add('fresh-physical-authorization',[{op:'authorize'},{op:'authorize'},{op:'report'},{op:'report'}]);
add('unchanged-view-notifies-once',[{op:'refresh'},{op:'refresh'},{op:'authorize'}]);
add('inactive',[{op:'set',active:false},{op:'refresh'},{op:'authorize'}]);
add('deactivate',[{op:'authorize'},{op:'set',active:false},{op:'refresh'},{op:'authorize'}]);
add('reactivate',[{op:'refresh'},{op:'set',active:false},{op:'refresh'},{op:'set',active:true},{op:'authorize'}]);
add('inactive-during-refresh',[{op:'set',active:[true,true,false]},{op:'refresh'}]);
add('decline-accept',[{op:'authorize'},{op:'decline'},{op:'authorize'},{op:'accept'},{op:'authorize'}]);
add('missing-route',[{op:'refresh'},{op:'authorize',route:'missing'},{op:'decline',route:'missing'},{op:'accept',route:'missing'}]);
for(const [name,peer]of Object.entries({declined:{fields:{enabled:false}},removed:{remove:true},empty:{clear:true},invalid:{raw:'{'},credential:{fields:{credentialId:'replacement'}},account:{fields:{accountIdentity:'replacement'}},type:{fields:{credentialType:'api-key'}},state:{fields:{state:'expired'}},unchanged:{fields:{label:'new label'}},stale:{fields:{stale:true}},unclaimed:{fields:{unclaimed:true}}}))add('peer-'+name,[{op:'authorize'},{op:'peer',...peer},{op:'authorize'}]);
add('peer-view-hides-grants',[{op:'refresh'},{op:'peer',view:'peer-view'},{op:'busy'},{op:'refresh'}]);
add('same-peer-view-keeps-grants',[{op:'refresh'},{op:'busy'},{op:'refresh'}]);
add('grant-unknown-replaces-known',[{op:'refresh'},{op:'set',list:[{value:{view:'v2',rows:inventory.rows}}]},{op:'refresh'}]);
add('zero-grants',[{op:'set',list:[{value:{view:'none',rows:[],grants:0,grantTuples:[]}}]},{op:'refresh'}]);
for(const where of ['connect','list','get'])add(where+'-failure-retry',[{op:'set',[where]:[{error:{}}]},{op:'authorize'},{op:'authorize'}]);
add('missing-token',[{op:'token',remove:true},{op:'authorize'}]);
add('rotated-token',[{op:'authorize'},{op:'token',value:{token:'b'.repeat(64),token_generation:2}},{op:'authorize'},{op:'report'}]);
add('report-after-inactive',[{op:'authorize'},{op:'set',active:false},{op:'refresh'},{op:'report'}]);
add('closed-methods',[{op:'authorize'},{op:'close'},{op:'refresh'},{op:'authorize'},{op:'decline'},{op:'accept'},{op:'report'}]);
add('cancel-before-admission',[{op:'cancel'},{op:'authorize'}]);
add('shared-refresh',[{op:'set',list:[{hold:'list'}]},{op:'begin',method:'refresh',id:'first',until:'list'},{op:'begin',method:'refresh',id:'second'},{op:'release',id:'list'},{op:'join',id:'first'},{op:'join',id:'second'}]);
add('cancel-leaves-shared-refresh',[{op:'set',list:[{hold:'list'}]},{op:'begin',method:'authorize',id:'first',until:'list'},{op:'cancel'},{op:'join',id:'first'},{op:'begin',method:'refresh',id:'refresh'},{op:'release',id:'list'},{op:'join',id:'refresh'}]);
add('close-late-connection',[{op:'set',connect:[{hold:'connect'}]},{op:'begin',method:'refresh',id:'refresh',until:'connect'},{op:'close'},{op:'release',id:'connect'},{op:'join',id:'refresh'}]);
for(const action of ['cancel','close'])add(action+'-held-get',[{op:'refresh'},{op:'set',get:[{hold:'get'}]},{op:'begin',method:'authorize',id:'request',until:'get'},{op:action},{op:'join',id:'request'},{op:'release',id:'get'}]);
add('close-held-refresh',[{op:'set',list:[{hold:'list'}]},{op:'begin',method:'refresh',id:'refresh',until:'list'},{op:'close'},{op:'release',id:'list'},{op:'join',id:'refresh'}]);
add('shared-initial-authorization',[{op:'set',connect:[{hold:'connect'}]},{op:'begin',method:'authorize',id:'first',until:'connect'},{op:'begin',method:'authorize',id:'second',signal:'second'},{op:'release',id:'connect'},{op:'join',id:'first'},{op:'join',id:'second'}]);
add('refresh-and-send-overlap',[{op:'refresh'},{op:'set',list:[{hold:'list'}]},{op:'begin',method:'refresh',id:'refresh',until:'list'},{op:'authorize'},{op:'release',id:'list'},{op:'join',id:'refresh'}]);
add('report-failure-retry',[{op:'authorize'},{op:'set',report:[{error:{code:'not_found'}}]},{op:'report'},{op:'report'},{op:'report'}]);
add('empty-peer-roster-busy',[{op:'refresh'},{op:'peer',view:'',clear:true},{op:'busy'},{op:'refresh'}]);
for(const args of specs)await run(...args as [string,any[]]);
await writeFile(process.env.RPI_REFERENCE_OUTPUT!,JSON.stringify({reference:'a97f9055017f3c1546afbfe61a19d50b428be3d8',runtime:process.version,inventory,receipt,token,route,cases})+'\n');console.log(`${cases.length} original consumer traces`);
