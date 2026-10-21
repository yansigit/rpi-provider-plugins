import { mock } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { readFile, writeFile } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
const root=resolve(process.env.RPI_REFERENCE_DIR!);
const pin='a97f9055017f3c1546afbfe61a19d50b428be3d8';
assert.equal(execFileSync('git',['-C',root,'rev-parse','HEAD'],{encoding:'utf8'}).trim(),pin);
const initialClock=1784285195116;
let now=initialClock, queue:any[]=[], trace:any[]=[];
Date.now=()=>now;
const originalTimeout=globalThis.setTimeout;
globalThis.setTimeout=((callback:()=>void,ms:number,...args:any[])=>{
  if(ms===5000 || ms===7) {trace.push({kind:'wait',milliseconds:ms});now+=ms;queueMicrotask(callback);return {} as any;}
  return originalTimeout(callback,ms,...args);
}) as any;
globalThis.fetch=async()=>{throw new Error('unexpected fetch');};
mock.module(pathToFileURL(join(root,'packages/core/dist/agy-transport.js')).href,{exports:{fetchWithAgyCliTransport:async(url:string,init:any)=>{
  trace.push({kind:'send',request:{url,method:init.method,headers:Object.entries(init.headers),body:String(init.body),agy:true,timeoutMs:180000}});
  const next=queue.shift();assert.ok(next,'unexpected project request');
  if(next.error)throw new Error(next.error);
  return {ok:next.status>=200&&next.status<300,status:next.status,statusText:next.statusText??'',json:async()=>{trace.push({kind:'read'});if(next.readError)throw new Error(next.readError);return structuredClone(next.json);}};
}}});
const project=await import(pathToFileURL(join(root,'packages/core/dist/project.js')).href);
const auth=(refresh='refresh|requested', access='access')=>({type:'oauth',refresh,access,expires:initialClock+3600000});
const ensure=(value=auth())=>({op:'ensure',auth:value});
const payload=(json:any)=>({status:200,json});
const managed=payload({cloudaicompanionProject:'managed',currentTier:{id:'pro'},paidTier:{id:'paid'}});
const onboard=payload({done:true,response:{cloudaicompanionProject:{id:'created'}}});
const failures=[{status:403},{status:503}];
const cases:any[]=[];
async function run(name:string,responses:any[],operations:any[]) {
  project.invalidateProjectContextCache();queue=structuredClone(responses);trace=[];now=initialClock;
  const results=[];
  for(const operation of operations) {
    try {
      let value;
      switch(operation.op){
        case 'ensure':value=await project.ensureProjectContext(operation.auth);break;
        case 'parallel':value=await Promise.all(operation.auths.map((auth:any)=>project.ensureProjectContext(auth)));break;
        case 'invalidate':project.invalidateProjectContextCache(operation.refresh);break;
        case 'clearFailures':project.clearProvisionFailedKeys();break;
        case 'advance':now+=operation.milliseconds;break;
        case 'onboard':value=await project.onboardManagedProject(operation.access,operation.tier,operation.project,operation.attempts,operation.delay);break;
      }
      results.push({value:value??null});
    }catch(error:any){results.push({error:error.message});}
  }
  assert.equal(queue.length,0,`unused scripted responses: ${name}`);
  cases.push({name,responses,operations,results,trace:structuredClone(trace),clock:now});
}
await run('no access',[],[ensure(auth('refresh',''))]);
await run('packed managed project',[],[ensure(auth('refresh|requested|stored'))]);
await run('discovered project and captured tiers',[managed],[ensure()]);
for(const value of ['managed',{id:'object-project'}]) await run('project shape '+JSON.stringify(value),[payload({cloudaicompanionProject:value})],[ensure()]);
await run('second endpoint',[{status:403},managed],[ensure()]);
await run('read failure fallback',[{status:200,readError:'broken JSON'},managed],[ensure()]);
await run('network failure fallback',[{error:'load failed'},managed],[ensure()]);
await run('cache across packed project changes',[managed],[ensure(),ensure(auth('refresh|different|managed','new-access'))]);
await run('concurrent requests share the first auth',[managed],[{op:'parallel',auths:[auth(),auth('refresh|changed','new-access')]}]);
await run('expiry boundary',[managed,managed],[ensure(),{op:'advance',milliseconds:1799999},ensure(),{op:'advance',milliseconds:1},ensure()]);
await run('invalidate stable refresh key',[managed,managed],[ensure(),{op:'invalidate',refresh:' refresh|another|managed '},ensure()]);
await run('invalidate no key does nothing',[managed],[ensure(),{op:'invalidate',refresh:'   '},ensure()]);
await run('invalidate globally',[managed,managed],[ensure(),{op:'invalidate'},ensure()]);
await run('no cache without refresh',[managed,managed],[ensure({...auth(),refresh:''}),ensure({...auth(),refresh:''})]);
for(const allowedTiers of [undefined,[],[{id:'first'},{id:'second',isDefault:true}],[{id:'first'},{id:'second'}],[{isDefault:true}],[{id:'',isDefault:true}]]) {
  await run('onboard tier '+JSON.stringify(allowedTiers),[payload({allowedTiers,currentTier:{id:'free'},paidTier:'paid'}),onboard],[ensure()]);
}
await run('onboard next endpoint',[payload({}),{status:403},onboard],[ensure()]);
await run('onboard pending then success',[payload({}),payload({done:false}),onboard],[ensure()]);
await run('onboard completed requested fallback',[payload({}),payload({done:true})],[ensure()]);
await run('failed provisioning falls back and suppresses later probes',[...failures,...failures],[ensure(),{op:'advance',milliseconds:1800000},ensure()]);
await run('failed provisioning hardcoded fallback',[...failures,...failures],[ensure(auth('refresh'))]);
await run('clear failed provisioning after expiry',[...failures,...failures,managed],[ensure(),{op:'advance',milliseconds:1800000},{op:'clearFailures'},ensure()]);
await run('invalidate failed provisioning',[...failures,...failures,managed],[ensure(),{op:'invalidate',refresh:'refresh|different'},ensure()]);
await run('exhaust onboarding attempts',[payload({}),...Array.from({length:20},()=>payload({done:false}))],[ensure()]);
await run('onboard custom retry settings',[payload({done:false}),payload({done:false}),onboard],[{op:'onboard',access:'access',tier:'custom',attempts:2,delay:7}]);
const file='packages/core/src/project.ts';
await writeFile(process.env.RPI_REFERENCE_OUTPUT!,JSON.stringify({reference:pin,runtime:process.version,initialClock,sourceHashes:{[file]:createHash('sha256').update(await readFile(join(root,file))).digest('hex')},cases},null,2)+'\n');
console.log(JSON.stringify({cases:cases.length}));
