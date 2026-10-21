import {test,mock} from 'node:test';
import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {mkdtemp,copyFile,symlink,rm,readFile,writeFile} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {tmpdir} from 'node:os';
import {pathToFileURL} from 'node:url';
const root=resolve(process.env.RPI_REFERENCE_DIR!);
const integrated=resolve(process.env.RPI_REFERENCE_INTEGRATED!);
const pin='a97f9055017f3c1546afbfe61a19d50b428be3d8';
assert.equal(execFileSync('git',['-C',root,'rev-parse','HEAD'],{encoding:'utf8'}).trim(),pin);
const aiRoot=join(integrated,'node_modules/@earendil-works/pi-ai');
const manifest=JSON.parse(await readFile(join(aiRoot,'package.json'),'utf8')); assert.equal(manifest.version,'1.3.0');
const ai=await import(pathToFileURL(join(aiRoot,manifest.exports['.'].import)).href);
let events:any[]=[],requests:any[]=[],ids:string[]=[],sequence=0;
mock.module(pathToFileURL(join(aiRoot,manifest.exports['.'].import)).href,{exports:{...ai,createAssistantMessageEventStream:()=>{
 const stream=ai.createAssistantMessageEventStream(),push=stream.push.bind(stream);
 stream.push=(event:any)=>{push(event);events.push(structuredClone(event));};return stream;
}}});
const crypto=await import('node:crypto');
const nextId=()=>{const value=`00000000-0000-4000-8000-${String(++sequence).padStart(12,'0')}`;ids.push(value);return value;};
mock.module('node:crypto',{exports:{...Object.fromEntries(Object.entries(crypto).filter(([key])=>key!=='default')),randomUUID:nextId}});
const oldRandom=globalThis.crypto.randomUUID,oldNow=Date.now,oldMonotonic=performance.now;
globalThis.crypto.randomUUID=nextId as any;Date.now=()=>1784285195116;performance.now=()=>1000;
globalThis.fetch=async()=>{throw new Error('unexpected local project network request');};
const corePath=pathToFileURL(join(integrated,'node_modules/@cortexkit/antigravity-auth-core/dist/index.js')).href;
const core=await import(corePath);
const text='data: {"candidates":[{"content":{"parts":[{"text":"hello"}]},"finishReason":"STOP"}]}\n\n';
mock.module(corePath,{exports:{...core,fetchWithAgyCliTransport:async(url:string,init:any)=>{
 requests.push({url,method:init.method,headers:init.headers,body:init.body});return new Response(text);
}}});
const temporary=await mkdtemp(join(tmpdir(),'rpi-agy-local-reference-'));
for(const file of ['stream.ts','convert.ts','credential-cache.ts']) await copyFile(join(root,'packages/pi/src',file),join(temporary,file));
await symlink(join(integrated,'node_modules'),join(temporary,'node_modules'),'junction');
const {streamCortexKitAntigravity}=await import(pathToFileURL(join(temporary,'stream.ts')).href);
const {rememberPackedRefresh}=await import(pathToFileURL(join(temporary,'credential-cache.ts')).href);
test('original local admission credential bridge and execution ordering',async()=>{
 try{
  const fixtures=JSON.parse(await readFile(new URL('../tests/fixtures/stream.json',import.meta.url),'utf8'));
  const base=fixtures.cases[0]; const cases=[];
  for(const input of [
   {name:'local-success'}, {name:'missing-key',missingKey:true}, {name:'retired',retired:true},
   {name:'first-admission',failAdmission:1}, {name:'second-admission',failAdmission:2},
   {name:'cancel-first',cancelAdmission:1}, {name:'cancel-second',cancelAdmission:2},
  ]){
   events=[];requests=[];ids=[];let admissions=0;
   const model={...base.model,id:(input as any).retired?'antigravity-gemini-3.7-flash':'antigravity-gemini-3.8-flash'};
   const options={sessionId:input.name,...((input as any).missingKey?{}:{apiKey:'fixture-access'})};
   const packed=`fixture-refresh-${input.name}|fixture-project|fixture-managed`;
   rememberPackedRefresh('fixture-access',packed);
   const controller=new AbortController();
   const result=await streamCortexKitAntigravity(model,base.context,{...options,signal:controller.signal},async()=>{
    ++admissions;
    if(admissions===(input as any).cancelAdmission)controller.abort();
    if(admissions===(input as any).failAdmission)throw new Error(`admission ${admissions} refused`);
   }).result();
   cases.push({...input,model,context:base.context,options,packed,admissions,ids:[...ids],requests:structuredClone(requests),events:structuredClone(events),result:structuredClone(result)});
  }
  const sourceHashes=Object.fromEntries(await Promise.all(['stream.ts','convert.ts','credential-cache.ts'].map(async file=>[file,crypto.createHash('sha256').update(await readFile(join(temporary,file))).digest('hex')])));
  await writeFile(process.env.RPI_REFERENCE_OUTPUT!,JSON.stringify({reference:pin,host:manifest.version,runtime:process.version,sourceHashes,now:Date.now(),body:text,cases},null,2)+'\n');
  console.log(JSON.stringify({cases:cases.length}));
 }finally{globalThis.crypto.randomUUID=oldRandom;Date.now=oldNow;performance.now=oldMonotonic;await rm(temporary,{recursive:true,force:true});}
});
