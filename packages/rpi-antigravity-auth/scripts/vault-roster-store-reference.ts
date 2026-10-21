import assert from 'node:assert/strict';
import {mkdtemp,writeFile,readFile,readdir,rm} from 'node:fs/promises';
import {resolve,join} from 'node:path';
import {pathToFileURL} from 'node:url';
const root=resolve(process.env.RPI_REFERENCE_DIR!);
const {refreshVaultRoster,mutateVaultRoster}=await import(pathToFileURL(join(root,'packages/core/src/common-auth-embedded/claustrum/roster.js')).href);
const {ClaustrumConsumerError}=await import(pathToFileURL(join(root,'packages/core/src/common-auth-embedded/claustrum/errors.js')).href);
const {ClaustrumScopedCustody}=await import(pathToFileURL(join(root,'packages/core/src/common-auth-embedded/claustrum/custody.js')).href);
const initial={version:1,view:'old',complete:true,rows:[],declined:[]};
const inventory={view:'new',credentials:[{credentialId:'id',credentialType:'oauth',state:'active',accountIdentity:'account'}],skipped:[]};
const cases:any[]=[];
for(const name of ['fresh','same','busy','busy-without-roster','busy-empty-view','inactive-before','inactive-after','discovery-error','cancel-before','cancel-discovery','lost-discovery-lease','invalid-store','mutate','no-change','change-error','fence-error','lost-write-lease-in-change','lost-write-lease-at-rename','write-lock-timeout']) {
 const dir=await mkdtemp('/home/user/provider-test-tmp/roster-store-reference-'),path=join(dir,'roster.json');
 const trace:any[]=[];const abort=new AbortController();let calls=0;const mutation=name.startsWith('mutate')||['no-change','change-error','fence-error','lost-write-lease-in-change','lost-write-lease-at-rename','write-lock-timeout'].includes(name);
 const start=name==='fresh'||name==='busy-without-roster'?undefined:name==='busy-empty-view'?{...initial,view:''}:name==='same'?{version:1,view:'new',complete:true,rows:[{routeId:'vault:id~9af211329b',credentialId:'id',credentialType:'oauth',accountIdentity:'account',state:'active',label:'id',enabled:true,addedAt:1000}],declined:[]}:initial;
 if(start!==undefined)await writeFile(path,JSON.stringify(start)+'\n');
 if(name==='invalid-store')await writeFile(path,'{');
 const before=await readFile(path,'utf8').catch(()=>null);
 const takeover=async(lock:string)=>writeFile(path+`.${lock}.lock`,JSON.stringify({ownerId:'successor',expiresAt:Date.now()+60000})+'\n',{mode:0o600});
 if(name.startsWith('busy'))await takeover('claustrum-roster');
 if(name==='write-lock-timeout')await takeover('claustrum-roster-write');
 if(name==='cancel-before')abort.abort();
 const custody=new ClaustrumScopedCustody({family:{refreshAdapter:'antigravity',category:'antigravity-native',apiKeys:false},requireAssertion:true,readToken:async()=>({token:'a'.repeat(64),token_generation:1}),client:{listScoped:async()=>{
  if(name==='discovery-error')throw new ClaustrumConsumerError('fixture','fixture discovery failed');
  if(name==='cancel-discovery')abort.abort();if(name==='lost-discovery-lease')await takeover('claustrum-roster');
  return {view:inventory.view,rows:inventory.credentials.map(({credentialId,accountIdentity,...rest}:any)=>({...rest,id:credentialId,accountId:accountIdentity,operations:['read'],categories:['antigravity-native'],refreshAdapter:'antigravity'}))};
 },close(){}}});
 let result:any;
 try {
  let value;
  if(mutation) value=await mutateVaultRoster(path,async(current:any,{assertOwned}:any)=>{
   trace.push(['change',current]);
   if(name==='change-error')throw new ClaustrumConsumerError('fixture','fixture change failed');
   if(name==='lost-write-lease-in-change')await takeover('claustrum-roster-write');
   await assertOwned();
   return {next:name==='no-change'?undefined:{...current,view:'changed',number:1e21,extra:{'10':10,'2':2,nested:[1.0,'line\ntext']}},result:'committed'};
  },{beforePublish:async()=>{trace.push(['before-publish']);if(name==='fence-error')throw new ClaustrumConsumerError('fixture','fixture fence failed');if(name==='lost-write-lease-at-rename')await takeover('claustrum-roster-write');}});
  else value=await refreshVaultRoster({path,signal:abort.signal,projection:{now:1000},isActive:async()=>{trace.push(['active']);return !(name==='inactive-before'||name==='inactive-after'&&calls++>0);},custody:{discover:async()=>{trace.push(['discover']);return custody.discover(abort.signal);}}});
  result={value:value??null};
 } catch(e:any) {result={error:{name:e.name,message:e.message.replaceAll(dir,'<DIR>'),...(e.kind!==undefined&&{kind:e.kind}),...(e.code!==undefined&&{code:e.code})}};}
 custody.close();
 const after=await readFile(path,'utf8').catch(()=>null);const remaining=(await readdir(dir)).sort();
 cases.push({name,mutation,start:start??null,before,inventory,trace,result,after,remaining});await rm(dir,{recursive:true,force:true});
}
assert(cases.length===19);
await writeFile(resolve(process.env.RPI_REFERENCE_OUTPUT!),JSON.stringify({runtime:process.version,cases},null,2)+'\n');
console.log(`${cases.length} original roster storage/discovery traces captured`);
