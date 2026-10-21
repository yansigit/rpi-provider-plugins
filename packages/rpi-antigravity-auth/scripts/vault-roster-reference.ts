// Run unchanged pinned roster functions with a controlled locale, clock and inventory.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { pathToFileURL } from 'node:url';
const root = resolve(process.env.RPI_REFERENCE_DIR!);
const { projectVaultRoster, readVaultRoster, declineVaultRoute, acceptVaultRoute } = await import(pathToFileURL(join(root,'packages/core/src/common-auth-embedded/claustrum/roster.js')).href);
const { isDeclined, declineAccount, acceptAccount } = await import(pathToFileURL(join(root,'packages/core/src/common-auth-embedded/claustrum/interlock.js')).href);
const directory = await mkdtemp('/home/user/provider-test-tmp/roster-reference-');
const file = join(directory,'roster.json');
const cases: any[] = [], decodes: any[] = [], changes: any[] = [], interlocks: any[] = [], sorts: any[] = [];
const locale = new Intl.Collator().resolvedOptions().locale;
let seed=0x735f21ab;
function rand(n: number) { seed^=seed<<13; seed^=seed>>>17; seed^=seed<<5; return (seed>>>0)%n; }
const pick=(a:any[])=>a[rand(a.length)];
const credential=(id:string,identity?:string,more:any={})=>({credentialId:id,credentialType:'oauth',state:'active',...(identity!==undefined && {accountIdentity:identity}),...more});
function capture(name:string,previous:any,credentials:any[],skipped:any[]=[],options:any={}) {
 const inventory={view:`view-${cases.length}`,credentials,skipped};
 const input={now:1000,...options};
 const result=projectVaultRoster(previous,inventory,{...input,reservedRouteIds:new Set(input.reservedRouteIds??[])});
 cases.push(structuredClone({name,...(previous!==undefined&&{previous}),inventory,options:input,result})); return result;
}
const first=capture('initial',undefined,[credential('oauth:google: Account ','account',{email:'email',orgName:'org'})]);
for(const identity of [undefined,'account','replacement','']) for(const state of ['active','disabled','refreshing','']) {
 const current=capture(`identity-state-${cases.length}`,first,[credential('oauth:google: Account ',identity,{state})]);
 capture(`restore-${cases.length}`,current,[credential('oauth:google: Account ','account')]);
}
for(const skipped of [[],[{reason:'empty credential id'}],[{credentialId:'oauth:google: Account ',reason:'duplicate credential id'}],[{credentialId:'other',reason:'empty state'}]]) capture(`incomplete-${cases.length}`,first,[],skipped);
const aliasBase=capture('aliases-initial',undefined,[credential('c','same'),credential('a','same'),credential('b','same'),credential('other','else')]);
for(const removed of ['a','b','c','other']) for(const reason of ['empty credential id','duplicate credential id']) capture(`aliases-removed-${cases.length}`,aliasBase,['a','b','c','other'].filter(id=>id!==removed).map(id=>credential(id,id==='other'?'else':'same')),[{...(reason!=='empty credential id'&&{credentialId:removed}),reason}]);
const collision=structuredClone(aliasBase); collision.rows[0].routeId='';
capture('reserved-collisions',collision,[credential('a','same')],[],{reservedRouteIds:[aliasBase.rows[0].routeId,`${aliasBase.rows[0].routeId}~2`]});
let previous:any;
const ids=['oauth:google:lower','oauth:google:Upper','oauth:google:é','a','a-1','a_1','a10','a2','😀','z','Z','ä','Á','a\u0301'];
for(let i=0;i<1200;i++) {
 const credentials=ids.filter(()=>rand(4)!==0).map(id=>credential(id,pick([undefined,'alpha','beta','gamma','previous','']),{state:pick(['active','active','disabled','refreshing']),...(rand(3)===0&&{email:pick(['',' email ','test@example.invalid'])}),...(rand(3)===0&&{orgName:pick(['','org'])})}));
 const skipped=rand(3)===0?[{...(rand(3)!==0&&{credentialId:pick(ids)}),reason:pick(['empty credential id','duplicate credential id','blank account identity','empty state'])}]:[];
 previous=capture(`sequence-${i}`,previous,credentials,skipped,{...(rand(6)===0&&{reservedRouteIds:previous?.rows.slice(0,2).map((r:any)=>r.routeId)??[]}),...(rand(12)===0&&{routePrefix:'custom:'})});
 if(previous.rows.length&&rand(3)===0){ const row=pick(previous.rows); row.quota={limits:[],extra:{preserved:true}}; previous.declined=declineAccount(previous.declined,row.credentialId,row.accountIdentity); row.enabled=false; }
 if(i%60===0) previous=undefined;
}
const base={version:1,view:'view',rows:[{routeId:'route',credentialId:'id',credentialType:'oauth',state:'active',label:'label',enabled:true,addedAt:123,accountIdentity:'account',aliases:['alias'],extra:{kept:true}}],declined:[]};
const errorValue=(e:any)=>({name:e.name,message:e.message,...(e.kind!==undefined&&{kind:e.kind})});
async function decode(name:string,value:any,raw?:string) {
 await writeFile(file,raw??JSON.stringify(value),{mode:0o600});
 try { decodes.push({name,...(raw===undefined?{input:value}:{raw}),result:await readVaultRoster(file)}); }
 catch(e) { decodes.push({name,...(raw===undefined?{input:value}:{raw}),error:errorValue(e)}); }
}
await decode('valid',base); await decode('bad-json',null,'{');
for(const value of [null,[],0,'',{},true]) await decode(`root-${decodes.length}`,value);
for(const field of ['version','view','rows','declined','complete','rejected']) for(const value of [undefined,null,0,1,'',true,false,[],{},['x']]) {
 const item=structuredClone(base); if(value===undefined)delete(item as any)[field];else(item as any)[field]=value; await decode(`root-${field}-${decodes.length}`,item);
}
for(const field of ['routeId','credentialId','credentialType','state','label','enabled','addedAt','accountIdentity','email','orgName','aliases','stale','unclaimed','quota']) for(const value of [undefined,null,0,-1,1.5,'',true,false,[],{},['x'],[null]]) {
 const item=structuredClone(base); if(value===undefined)delete(item.rows[0] as any)[field];else(item.rows[0] as any)[field]=value; await decode(`row-${field}-${decodes.length}`,item);
}
const quotas=[{limits:[]},{limits:[{kind:'reading',scope:'all',label:'primary',checkedAt:1,usedPercent:100,windowMinutes:1}]},{limits:[{kind:'absent',scope:'all',label:'primary',checkedAt:1}]},{limits:[{kind:'retired',scope:'all',label:'primary',retiredAt:1}]}];
for(const quota of quotas) for(const budget of [undefined,null,{kind:'cleared',checkedAt:1},{kind:'reading',checkedAt:1,reached:false,remaining:-3},{kind:'reading',checkedAt:1,reached:'false'}]) for(const counters of [undefined,null,{}, {good:{value:1,checkedAt:1},bad:{value:-1,checkedAt:1}}, {one:{value:1.5,checkedAt:1}},[]]) {
 const item=structuredClone(base); (item.rows[0] as any).quota={...quota,...(budget!==undefined&&{budget}),...(counters!==undefined&&{counters}),future:'keep'}; await decode(`quota-${decodes.length}`,item);
}
for(const row of quotas.slice(1))for(const field of Object.keys(row.limits[0]))for(const value of [null,'',false,0,-1,{},undefined]){
 const item=structuredClone(base),quota=structuredClone(row);if(value===undefined)delete(quota.limits[0] as any)[field];else(quota.limits[0] as any)[field]=value;(item.rows[0] as any).quota=quota;await decode(`quota-limit-${field}-${decodes.length}`,item);
}
for(const declined of [[{}],[{credentialId:'id',accountIdentity:null}],[{credentialId:'',extra:1}],['id']]) await decode(`declines-${decodes.length}`,{...base,declined});
for(const accept of [true,false]) for(const id of ['route','absent']) for(const declined of [[],[{credentialId:'alias'}],[{credentialId:'id',accountIdentity:'old'}],[{credentialId:'other',accountIdentity:'account'}]]){
 const input={...base,declined};await writeFile(file,JSON.stringify(input));
 try { await(accept?acceptVaultRoute:declineVaultRoute)(file,id); changes.push({input,route:id,accept,result:await readVaultRoster(file)}); }
 catch(e){ changes.push({input,route:id,accept,error:errorValue(e)}); }
}
for(const identity of [undefined,'a','b',''])for(const oldIdentity of [undefined,'a','b',''])for(const id of ['id','other']){
 const entries=[{credentialId:id,...(oldIdentity!==undefined&&{accountIdentity:oldIdentity}),future:'kept'}];
 interlocks.push({entries,id:'id',...(identity!==undefined&&{identity}),isDeclined:isDeclined(entries,'id',identity),decline:declineAccount(entries,'id',identity),accept:acceptAccount(entries,['id','alias'],identity)});
}
const words=['','a','A','ä','Ä','a\u0308','ß','ss','ẞ','z','Z','Æ','æ','å','Å','ö','ø','Ø','oe','œ','a 1','a-1','a_1','a.1','a10','a2','😀','🚀','~','!','@','#','$','%','^','&','*','(',')','_', '+','=','[',']','{','}','\u0000','\u200b','\uFEFF','I','ı','İ','i','é','è','e','E','É','č','ch','c','C','中','國','国','重','は','ハ','ば','ぱ','α','Α','а','А','ع','آ'];
for(const locale of ['en-US','sv-SE','de-DE','tr-TR','zh-CN','ja-JP','ru-RU','ar-EG'])sorts.push({locale,input:words,result:words.toSorted(new Intl.Collator(locale).compare)});
const unicode=[...words];for(let i=0;i<4096;i++){const code=rand(0x110000);if(code<0xd800||code>0xdfff)unicode.push(String.fromCodePoint(code));}
sorts.push({locale:'en-US',input:unicode,result:unicode.toSorted(new Intl.Collator('en-US').compare)});
const normalizationHash=createHash('sha256');let scalarCount=0;const allScalars:string[]=[];
for(let cp=0;cp<0x110000;cp++){
 if(cp>=0xd800&&cp<=0xdfff)continue;
 const raw=String.fromCodePoint(cp);scalarCount++;allScalars.push(raw);
 for(const value of [raw,'a\u0315'+raw+'\u0300\u0327'])normalizationHash.update(cp.toString(16)+':'+value.normalize('NFD')+'\n');
}
const normalization={scalars:scalarCount,sha256:normalizationHash.digest('hex')};
allScalars.sort(new Intl.Collator('en-US').compare);const collationHash=createHash('sha256');for(const value of allScalars)collationHash.update(value.codePointAt(0)!.toString(16)+'\n');
const collation={scalars:scalarCount,sha256:collationHash.digest('hex')};
await rm(directory,{recursive:true});
assert(cases.length>1200&&decodes.length>300);
await writeFile(resolve(process.env.RPI_REFERENCE_OUTPUT!),JSON.stringify({runtime:process.version,icu:process.versions.icu,locale,cases,decodes,changes,interlocks,sorts,normalization,collation})+'\n');
console.log(JSON.stringify({projection:cases.length,decode:decodes.length,change:changes.length,interlock:interlocks.length,sort:sorts.length,locale}));
