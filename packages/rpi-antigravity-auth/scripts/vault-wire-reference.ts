// Invoke the pinned enrolled-consumer client, with only its Subc connection scripted.
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {readFile, writeFile} from 'node:fs/promises';
import {dirname, join, resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
const root=resolve(process.env.RPI_REFERENCE_DIR!);
const clientRoot=join(root,'packages/core/node_modules/@cortexkit/claustrum-client');
assert.equal(JSON.parse(await readFile(join(clientRoot,'package.json'),'utf8')).version,'0.12.0');
const entry=join(clientRoot,'dist/index.js');
const {ClaustrumClient}=await import(pathToFileURL(entry).href);
const producer=JSON.parse(await readFile(resolve(process.env.RPI_VAULT_PRODUCER_FIXTURE!), 'utf8'));
const validToken='0123456789abcdef'.repeat(4);
const row={id:'credential-1',categories:['antigravity-native'],type:'oauth',serves:['google'],state:'active',record_version:3,operations:['read'],refresh_adapter:'antigravity',account_id:'account-1'};
const defaults={
 get:{result:{payload:[65,66,67],record_version:3,expires_at_ms:4102444800000,credential_id:'credential-1',project_id:'project-1',account_id:'account-1'}},
 list:{result:{credentials:[row],view:'view-1',grants:1,grant_tuples:[{selector_kind:'category',selector:'antigravity-native',operation:'read'}]}},
 propose:{result:{request_id:'request-1'}},
 poll:{result:{status:'approved',name:'consumer',token:validToken,token_generation:1}},
 rotate:{result:{token:validToken,token_generation:2}},
 report:{result:{accepted:true}},
};
const inputs={get:{credentialId:'credential-1',enrollmentToken:validToken,minTtlMs:300000},list:{enrollmentToken:validToken},propose:{name:'consumer',requestSecretHash:'a'.repeat(64)},poll:{requestId:'request-1',requestSecret:'b'.repeat(64)},rotate:{token:validToken,expectedTokenGeneration:1},report:{credentialId:'credential-1',enrollmentToken:validToken,providerStatus:401,recordVersion:3,reporterSource:'direct'}};
const methods={get:'getScoped',list:'listScoped',propose:'enrollPropose',poll:'enrollPoll',rotate:'enrollRotate',report:'reportAuthFailureScoped'};
const identity={projectId:'fixture-project',storageFingerprint:'fixture-storage'};
const cases:any[]=[];
async function capture(operation:string,response:any,input:any=inputs[operation],name=''){
 const trace:any[]=[],logs:string[]=[];
 const client=new ClaustrumClient({call:async(...args:any[])=>{trace.push(args);return structuredClone(response)},close(){}},()=>{throw Error('unexpected reconnect')},'/fixture/connection',2000,identity,(line:string)=>logs.push(line));
 let result;
 try {const value=await client[methods[operation]](operation==='list'?input.enrollmentToken:input);result={value:value??null};}
 catch(e:any){result={error:{name:e.name,message:e.message,code:e.code,class:e.class,action:e.action}};}
 client.close();cases.push({operation,name,input,response,result,trace,logs});
}
for(const [operation,response] of Object.entries(defaults)){
 await capture(operation,response);
 await capture(operation,response,{},'missing optional inputs');
 for(const malformed of [null,{},[],{result:null},{result:[]},{result:{}},{error:{class:'permanent',code:'not_found'}}])await capture(operation,malformed,undefined,'malformed envelope');
 for(const cls of [undefined,null,'transient','permanent','auth_required','context_overflow','unknown-class'])for(const code of [undefined,null,'not_found',''])await capture(operation,{result:{error:{class:cls,code}}},undefined,'error envelope');
}
async function mutate(operation:string,field:string,values:any[]){for(const value of values){const response=structuredClone(defaults[operation]);if(value===undefined)delete response.result[field];else response.result[field]=value;await capture(operation,response,undefined,field)}}
await mutate('get','payload',[undefined,null,[],[0],[256],[-1],[1.5],[true],[255],[239,187,191,65],'ABC',{}]);
await mutate('get','record_version',[undefined,null,-1,1.5,0,9007199254740991,9007199254740992,'3',{}]);
await mutate('get','expires_at_ms',[undefined,null,0,-1,1.5,'100',{},[]]);
for(const key of ['credential_id','account_id','project_id','email','org_name'])await mutate('get',key,[undefined,null,'',12,{},'value']);
await mutate('list','view',[undefined,null,'',3,{},'ok']);
await mutate('list','credentials',[undefined,null,[],[null],[{}],{},'bad']);
await mutate('list','grants',[undefined,null,-1,0,1.5,9007199254740992,'1']);
await mutate('list','grant_tuples',[undefined,null,[],[{}],[{selector_kind:'',selector:'',operation:''}],[{selector_kind:'x',selector:'y',operation:'z',extra:true}]]);
for(const field of ['id','type','state','record_version','categories','serves','provider_ids','operations','created_at_ms','plan_tier','plan_tier_source','auth_method','refresh_adapter','account_id','email','org_name']){
 for(const value of [undefined,null,0,1.5,'',[],['read'],{},'free','oauth','detected','invalid\n']){
  const response=structuredClone(defaults.list);if(value===undefined)delete response.result.credentials[0][field];else response.result.credentials[0][field]=value;
  await capture('list',response,undefined,'row.'+field);
 }
}
await mutate('propose','request_id',[undefined,null,'',0,'ok']);
for(const status of ['pending','denied','approved','other',null,0])await capture('poll',{result:{status}},undefined,'status');
for(const field of ['name','token','token_generation'])await mutate('poll',field,[undefined,null,'',-1,0,1.5,9007199254740992,'valid']);
for(const field of ['token','token_generation'])await mutate('rotate',field,[undefined,null,'',-1,0,1.5,9007199254740992,'valid']);
await mutate('report','accepted',[undefined,null,false,true,0,'true']);
const operationNames={'credential.get_scoped':'get','credential.list_scoped':'list','auth.enroll_propose':'propose','auth.enroll_poll':'poll','auth.enroll_rotate':'rotate','credential.report_auth_failure':'report'};
for(const fixture of producer.operations){const operation=operationNames[fixture.op];if(!operation)continue;for(const reply of [...(Array.isArray(fixture.success)?fixture.success:[]),...(fixture.reply?[fixture.reply]:[]),...(fixture.list_only_reply?[fixture.list_only_reply]:[])])await capture(operation,JSON.parse(reply),undefined,'producer:'+fixture.op);}
assert(cases.length>500);
const hashes={};for(const file of ['wire.js','errors.js'])hashes[file]=createHash('sha256').update(await readFile(join(dirname(entry),file))).digest('hex');
await writeFile(resolve(process.env.RPI_REFERENCE_OUTPUT!),JSON.stringify({runtime:process.version,client:'@cortexkit/claustrum-client@0.12.0',sourceHashes:hashes,producerSha256:createHash('sha256').update(await readFile(resolve(process.env.RPI_VAULT_PRODUCER_FIXTURE!))).digest('hex'),cases},null,2)+'\n');
console.log(`${cases.length} original enrolled-consumer wire cases captured`);
