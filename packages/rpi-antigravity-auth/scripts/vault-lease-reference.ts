import {writeFile,readFile,mkdtemp,mkdir,rm,utimes} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {tmpdir} from 'node:os';
import {pathToFileURL} from 'node:url';
const timers=new Map<any,()=>void>();const realTimeout=globalThis.setTimeout,realClear=globalThis.clearTimeout;
(globalThis as any).setTimeout=(fn:any,ms:number,...args:any[])=>{if(ms>=1000){const id={unref(){}};timers.set(id,()=>fn(...args));return id;}return realTimeout(fn,ms,...args);};
(globalThis as any).clearTimeout=(id:any)=>{if(timers.delete(id))return;return realClear(id);};
const root=resolve(process.env.RPI_REFERENCE_DIR!);
const {acquireRefreshFileLockWithClock}=await import(pathToFileURL(join(root,'packages/core/src/common-auth-embedded/fs/refresh-file-lock.js')).href);
const specs:any[]=[];
for(const initial of [undefined,{ownerId:'previous',expiresAt:110000},{ownerId:'previous',expiresAt:100000},{ownerId:'previous',expiresAt:99999},{ownerId:'previous',expiresAt:'110000'},{ownerId:'previous',expiresAt:null},{ownerId:'previous',expiresAt:'broken'},null,[],{expiresAt:99999},'malformed'])for(const age of [0,31000])specs.push({name:'initial',...(initial===undefined?{}:{initial}),age,steps:['assert','release']});
for(const legacy of [true,false])for(const markerAge of [0,6000])specs.push({name:'marker',initial:{ownerId:'previous',expiresAt:90000},legacy,markerAge,steps:['assert','release']});
for(const steps of [['renew','assert','release'],['expire','renew','assert','release'],['takeover','assert','release'],['marker','renew','assert','release'],['renew','renew','assert','release']])specs.push({name:'lifecycle',renew:true,steps});
for(const [step,mutation] of [['stale-lock-observed','takeover'],['stale-lock-confirmed','takeover'],['stale-lock-recreated','marker'],['renewal-write-fenced','marker'],['renewal-write-ready','takeover'],['renewal-write-finished','marker'],['release-owner-confirmed','marker']])specs.push({name:'race',initial:step.startsWith('stale')?{ownerId:'previous',expiresAt:90000}:undefined,renew:true,race:{step,mutation},steps:step.startsWith('renewal')?['renew','assert','release']:['assert','release']});
const cases:any[]=[];
for(const spec of specs){
 const directory=await mkdtemp(join(process.env.RPI_PRIVATE_TMP??tmpdir(),'vault-lease-'));const path=join(directory,'state.json'),lockPath=path+'.ceremony.lock',marker=lockPath+'.evicting';let now=100000;const trace:string[]=[];let fired=false,lock:any;let owner:string|undefined;
 const save=async(path:string,value:any)=>writeFile(path,JSON.stringify(value)+'\n',{mode:0o600});
 const mutate=async(kind:string)=>{if(kind==='takeover')await save(lockPath,{ownerId:'successor',expiresAt:now+60000});else{await mkdir(marker,{recursive:true});await save(join(marker,'owner.json'),{ownerId:'other-marker',createdAt:now});await utimes(marker,now/1000,now/1000);}};
 const normalize=(value:any):any=>Array.isArray(value)?value.map(normalize):value&&typeof value==='object'?Object.fromEntries(Object.entries(value).map(([k,v])=>[k,k==='ownerId'&&v===owner?'<OWNER>':normalize(v)])):value;
 const snapshot=async()=>{
  let value:any;try{value=JSON.parse(await readFile(lockPath,'utf8'));}catch(e:any){if(e.code==='EISDIR')value=JSON.parse(await readFile(join(lockPath,'owner.json'),'utf8'));else if(e.code!=='ENOENT')value='malformed';}
  let markerValue:any;try{markerValue=JSON.parse(await readFile(join(marker,'owner.json'),'utf8'));}catch{}
  return {lock:normalize(value??null),marker:markerValue?.ownerId==='other-marker'?'other-marker':markerValue?'present':null};
 };
 try{
  if(spec.initial!==undefined){if(spec.legacy){await mkdir(lockPath);await save(join(lockPath,'owner.json'),spec.initial);}else await writeFile(lockPath,spec.initial==='malformed'?'broken':JSON.stringify(spec.initial)+'\n',{mode:0o600});await utimes(lockPath,(now-(spec.age??0))/1000,(now-(spec.age??0))/1000);}
  if(spec.markerAge!==undefined){await mutate('marker');await utimes(marker,(now-spec.markerAge)/1000,(now-spec.markerAge)/1000);}
  let finishRenewal:(()=>void)|undefined;
  lock=await acquireRefreshFileLockWithClock({path,name:'ceremony',ttlMs:30000,renew:spec.renew??false,now:()=>now,onStep:async(step:string)=>{trace.push(step);if(!fired&&spec.race?.step===step){fired=true;await mutate(spec.race.mutation);}if(step==='renewal-finished')finishRenewal?.();}});
  owner=lock?.ownerId;
  const results:any[]=[{operation:'acquire',acquired:!!lock,after:await snapshot()}];
  if(lock)for(const step of spec.steps){
   let result:any={ok:true};
   if(step==='expire')now+=30000;
   else if(step==='takeover'||step==='marker')await mutate(step);
   else if(step==='renew'){
    now+=10000;const timer=timers.entries().next().value;
    if(timer){timers.delete(timer[0]);await new Promise<void>(resolve=>{finishRenewal=resolve;timer[1]();});finishRenewal=undefined;}else result={scheduled:false};
   }else{try{await lock[step==='assert'?'assertOwned':'release']();}catch(e:any){result={error:{name:e.name,message:e.message.replaceAll(directory,'<DIR>')}};}}
   results.push({operation:step,result,lost:lock.hasLost(),after:await snapshot()});
  }
  if(lock && !spec.steps.includes('release'))await lock.release();
  cases.push({spec,trace,results});
 }finally{timers.clear();await rm(directory,{recursive:true,force:true});}
}
await writeFile(resolve(process.env.RPI_REFERENCE_OUTPUT!),JSON.stringify({runtime:process.version,platform:process.platform,cases},null,2)+'\n');
console.log(`${cases.length} original file-lease scenarios captured`);
