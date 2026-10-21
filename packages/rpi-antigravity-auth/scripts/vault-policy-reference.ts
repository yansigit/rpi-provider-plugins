import assert from 'node:assert/strict';
import {readFile,writeFile} from 'node:fs/promises';
import {resolve,join} from 'node:path';
import {pathToFileURL} from 'node:url';
const root=resolve(process.env.RPI_REFERENCE_DIR!);
const dist=join(root,'packages/core/src/common-auth-embedded/claustrum');
const {ClaustrumScopedCustody,isScopedCredentialRotation}=await import(pathToFileURL(join(dist,'custody.js')).href);
const {VAULT_FAILURE_CLASSES,classifyVaultFailure}=await import(pathToFileURL(join(dist,'availability.js')).href);
const {ClaustrumCredentialError}=await import(pathToFileURL(join(root,'packages/core/node_modules/@cortexkit/claustrum-client/dist/index.js')).href);
const family={refreshAdapter:'antigravity',category:'antigravity-native',apiKeys:false};
const identity={credentialId:'id',credentialType:'oauth',accountIdentity:'account'};
const served={credentialId:'id',accountId:'account',material:'token',recordVersion:3,expiresAtMs:301000,projectId:'project'};
const cases:any[]=[];
async function capture(kind:string,value:any,identityValue:any=identity){
 const calls:any[]=[],warnings:any[]=[];
 const custody=new ClaustrumScopedCustody({family,requireAssertion:true,now:()=>1000,readToken:async()=>({token:'a'.repeat(64),token_generation:1}),client:{getScoped:async(input:any)=>{calls.push(input);return value;},listScoped:async(token:string)=>{calls.push(token);return value;},close(){}},logger:{debug(){},warn(...args:any[]){warnings.push(args)},info(){}},setTimeoutImpl:()=>({unref(){}}),clearTimeoutImpl:()=>{}});
 let result;
 try {const answer=kind==='authorize'?await custody.authorize(identityValue):await custody.discover();result=kind==='authorize'?{value:JSON.parse(JSON.stringify(answer)),accessToken:answer.accessToken}:{value:answer};}
 catch(e:any){result={error:{name:e.name,kind:e.kind,message:e.message}};}
 custody.close();cases.push({kind,input:value,identity:identityValue,result,calls,warnings});
}
await capture('authorize',served);
for(const field of ['credentialId','accountId','recordVersion','expiresAtMs','projectId'])for(const value of [undefined,'',null,'different',0,-1,1.5,9007199254740992,300999,301000,301000.5]){
 // Decoder-invalid scalar types are covered by wire fixtures, not injected here.
 if(['credentialId','accountId','projectId'].includes(field)&&value!==undefined&&typeof value!=='string')continue;
 if(field==='recordVersion'&&(value===undefined||value===null||typeof value==='string'))continue;
 if(field==='expiresAtMs'&&(value===undefined||typeof value==='string'))continue;
 const input=structuredClone(served);if(value===undefined)delete input[field];else input[field]=value;
 await capture('authorize',input);
}
for(const material of ['', ' token ', '\ufefftoken\ufeff','\u0085token\u0085','tok en','tok\nen','☃','claustrum-tombstone:v1:marker','{"access_token":"token"}','{"access":"token"}','{"access_token":null,"access":"token"}','{"access_token":0,"access":"token"}','{"access_token":" token "}','{"access_token":""}','{}','{invalid','["token"]'])await capture('authorize',{...served,material});
for(const credentialId of [undefined,'','id','other'])for(const accountIdentity of [undefined,'','account','other']) {const input={...identity,credentialId,accountIdentity};await capture('authorize',served,input);}
const row={id:'id',credentialType:'oauth',categories:['antigravity-native'],operations:['read'],refreshAdapter:'antigravity',state:'active',accountId:'account'};
const rows:any[]=[[],[row],[row,row],[{...row,id:''}],[{...row,id:'\ufeff'}],[{...row,id:'\u0085'}],[{...row,accountId:''}],[{...row,accountId:'\u0085'}],[{...row,state:' '}],[{...row,state:'disabled'}],[{...row,accountId:undefined}]];
for(const field of ['credentialType','refreshAdapter','categories','operations'])for(const value of (['categories','operations'].includes(field)?[[],['other'],['read','antigravity-native']]:['','oauth','api_key','antigravity','other']))rows.push([{...row,[field]:value}]);
for(const input of rows)for(const grants of [{},{grants:1},{grantTuples:[]},{grants:1,grantTuples:[{selectorKind:'category',selector:'antigravity-native',operation:'read'}]}])await capture('inventory',{rows:input,view:'view',...grants});
const failures=[];for(const code of [...Object.keys(VAULT_FAILURE_CLASSES),'unknown-new-code',''])for(const action of ['retry','gone','reauth','reduce_and_retry'])failures.push({code,action,value:classifyVaultFailure(new ClaustrumCredentialError(code,'transient',action))});
const rotations=[];const receipt={credentialId:'id',accountIdentity:'account',recordVersion:3};
for(const credentialId of ['id','other'])for(const accountIdentity of ['account','other'])for(const recordVersion of [2,3,4])for(const fromKeptReceipt of [undefined,false,true])for(const servedKept of [undefined,false,true]){
 const served={...receipt,fromKeptReceipt:servedKept},current={credentialId,accountIdentity,recordVersion,fromKeptReceipt};rotations.push({served,current,value:isScopedCredentialRotation(served,current)});
}
rotations.push({served:receipt,value:isScopedCredentialRotation(receipt,undefined)});
assert(cases.length>100);
await writeFile(resolve(process.env.RPI_REFERENCE_OUTPUT!),JSON.stringify({runtime:process.version,cases,failures,rotations},null,2)+'\n');
console.log(`${cases.length} custody validation, ${failures.length} classification and ${rotations.length} rotation cases captured`);
