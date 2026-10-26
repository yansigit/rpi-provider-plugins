// OAuth inputs and I/O boundaries from the unchanged core and Pi 1.3.0 integration.
import { mock } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
const root = resolve(process.env.RPI_REFERENCE_DIR!);
const integrated = resolve(process.env.RPI_REFERENCE_INTEGRATED!);
const output = resolve(process.env.RPI_REFERENCE_OUTPUT!);
const pin = 'a97f9055017f3c1546afbfe61a19d50b428be3d8';
assert.equal(execFileSync('git',['-C',root,'rev-parse','HEAD'],{encoding:'utf8'}).trim(),pin);
// The upstream client identity is supplied by the operator (RPI_REFERENCE_OAUTH_CLIENT_ID and
// RPI_REFERENCE_OAUTH_CLIENT_SECRET) and every occurrence is mapped to the scrubbed placeholders
// before writing, so the committed fixture never carries the real values.
const scrubbedClient:[string,string][]=[['RPI_REFERENCE_OAUTH_CLIENT_ID','antigravity-oauth-client-id.invalid'],['RPI_REFERENCE_OAUTH_CLIENT_SECRET','antigravity-oauth-client-secret-placeholder']];
function normalizeOAuthClient(text:string):string{
  let result=text;
  for(const [variable,placeholder] of scrubbedClient){
    const value=process.env[variable];
    assert.ok(value,`${variable} must be set to the upstream value so it can be scrubbed`);
    assert.ok(result.includes(value),`${variable} does not occur in the regenerated document`);
    result=result.split(value).join(placeholder);
  }
  return result;
}
const crypto = await import('node:crypto');
const entropy = Buffer.from(Array.from({length:32}, (_,i) => i));
mock.module('node:crypto', {exports:{...Object.fromEntries(Object.entries(crypto).filter(([key]) => key !== 'default')),randomBytes: (length: number) => { assert.ok(length<=32); return entropy.subarray(0,length); }}});
const clock = 1784285195116;
Date.now = () => clock;
let queue: any[] = [], trace: any[] = [];
function respond(url: string, init: any = {}, agy = false, timeoutMs = 15000) {
  trace.push({kind:'send', request:{url,method:init.method ?? 'GET',headers:Object.entries(init.headers ?? {}),...(init.body === undefined ? {} : {body:String(init.body)}),agy,timeoutMs}});
  const next = queue.shift();
  assert.ok(next, 'unexpected network request');
  if (next.error) throw new Error(next.error);
  const read = () => { trace.push({kind:'read'}); if (next.readError) throw new Error(next.readError); return next.raw ?? JSON.stringify(next.json); };
  return Promise.resolve({ok:next.status >= 200 && next.status < 300,status:next.status,statusText:next.statusText ?? '',text:async () => read(),json:async () => JSON.parse(read())});
}
globalThis.fetch = respond as any;
const installedCore = join(integrated,'node_modules/@cortexkit/antigravity-auth-core');
for (const base of [join(root,'packages/core'), installedCore]) {
  mock.module(pathToFileURL(join(base,'dist/agy-transport.js')).href, {exports:{DEFAULT_AGY_RESPONSE_HEADER_TIMEOUT_MS:180000,DEFAULT_AGY_IDLE_TIMEOUT_MS:180000,fetchWithAgyCliTransport:(url:string,init:any,options:any = {}) => respond(url,init,true,options.timeoutMs ?? 180000)}});
}
const core = await import(pathToFileURL(join(root,'packages/core/dist/antigravity/oauth.js')).href);
const helpers = await import(pathToFileURL(join(root,'packages/core/dist/auth.js')).href);
const cache = await import(pathToFileURL(join(root,'packages/pi/src/credential-cache.ts')).href);
const temporary = await mkdtemp(join(process.env.TMPDIR ?? '/tmp','rpi-agy-auth-'));
process.env.PI_CODING_AGENT_DIR = temporary;
try {
  const installed = join(integrated,'node_modules/@cortexkit/pi-antigravity-auth');
  const host = JSON.parse(await readFile(join(integrated,'node_modules/@earendil-works/pi-ai/package.json'),'utf8'));
  assert.equal(host.version,'1.3.0');
  const {registerCortexKitPiAntigravity} = await import(pathToFileURL(join(installed,'dist/index.js')).href);
  const registrations: any[] = [];
  await registerCortexKitPiAntigravity({registerProvider:(_id:string,config:any) => registrations.push(config), registerCommand:()=>{}, on:()=>{}}, {
    vaultDir:join(temporary,'vault'),modeFile:join(temporary,'mode.json'),rosterFile:join(temporary,'roster.json'),nativeAuthPath:join(temporary,'auth.json'),nativeStashPath:join(temporary,'stash.json'),
    readHostSlot:()=>undefined,connect:async()=>{throw new Error('unexpected vault access');},connectEnrollment:async()=>{throw new Error('unexpected enrollment');},
    loadPiSlot:async()=>({openPiSlot:()=>({exitVault:async()=>'absent',enterVault:async()=>{throw new Error('unexpected custody');}})}),
  });
  assert.equal(registrations.length,1);
  const provider = registrations[0];
  const authorizations = [];
  for (const project of ['', 'project-one', '😀/x +%']) authorizations.push({project,entropy:Array.from(entropy),result:await core.authorizeAntigravity(project)});
  const packed = ['', 'r','r|','r|p','r||m','r|p|m|ignored',' r | p | m '].map(input => {const parsed=helpers.parseRefreshParts(input);return {input,parsed,formatted:helpers.formatRefreshParts(parsed)};});
  const expiries = [null,{},'1',-5,0,1,0.25,3600].map(seconds => ({seconds,result:helpers.calculateTokenExpiry(clock,seconds)}));
  const expired = [];
  for (const access of ['',null,'token']) for (const expires of [null,'1',clock+59999,clock+60000,clock+60001]) expired.push({auth:{access,expires},result:helpers.accessTokenExpired({access,expires})});
  const cacheTrace = [];
  for (const [access,refresh] of [['a','one'],['b','two'],['c','three'],['d','four'],['a','replace'],['e','five'],['','ignored']]) {
    cache.rememberPackedRefresh(access,refresh);
    cacheTrace.push({access,refresh,values:Object.fromEntries(['','a','b','c','d','e'].map(key=>[key,cache.getPackedRefresh(key) ?? null]))});
  }
  const token = {status:200,json:{access_token:'access-test',refresh_token:'refresh-test',expires_in:1800}};
  const user = {status:200,json:{email:'fixture@example.test',name:'  Alice 😀  '}};
  const managed = {status:200,json:{cloudaicompanionProject:{id:'managed-project'}}};
  const cases: any[] = [];
  async function exchangeCase(name:string,responses:any[],project='',refuseAt=0,cancelAt=0,stateOverride?:any) {
    queue=structuredClone(responses); trace=[];
    let admissions=0, cancelled=false;
    const controller=new AbortController();
    const state=stateOverride === undefined ? new URL((await core.authorizeAntigravity(project)).url).searchParams.get('state')! : Buffer.from(JSON.stringify(stateOverride)).toString('base64url');
    const result = await core.exchangeAntigravity('code +😀',state,{signal:controller.signal,admit:()=>{admissions++;trace.push({kind:'admit'});if(admissions===cancelAt){cancelled=true;controller.abort();}if(admissions===refuseAt)throw new Error('custody changed');}});
    cases.push({name,kind:'exchange',responses,project,state,code:'code +😀',refuseAt,cancelAt,result,trace:structuredClone(trace),remaining:queue.length,admissions,cancelled});
  }
  await exchangeCase('explicit project',[token,user],'project-one');
  await exchangeCase('discovered project',[token,user,managed]);
  await exchangeCase('second endpoint',[token,user,{status:403,raw:'denied'},managed]);
  await exchangeCase('no project',[token,user,{status:200,json:{}},{status:200,json:{cloudaicompanionProject:''}}]);
  await exchangeCase('project network failure',[token,user,{error:'first failed'},managed]);
  await exchangeCase('project malformed shape',[token,user,{status:200,json:{cloudaicompanionProject:42}},managed]);
  await exchangeCase('token error',[{status:400,raw:'invalid_grant'}]);
  await exchangeCase('token read error',[{status:400,readError:'broken body'}]);
  await exchangeCase('no refresh',[{status:200,json:{access_token:'access-test',expires_in:60}},user]);
  await exchangeCase('userinfo forbidden',[token,{status:403,raw:'ignored'}],'project-one');
  await exchangeCase('userinfo read error',[token,{status:200,readError:'userinfo read failed'}],'project-one');
  await exchangeCase('empty user name',[token,{status:200,json:{name:' \ufeff '}}],'project-one');
  for (const seconds of [null,'3600',-1,0,0.25]) await exchangeCase(`expiry ${JSON.stringify(seconds)}`,[{status:200,json:{...token.json,expires_in:seconds}},user],'project-one');
  for(let at=1;at<=9;at++) {await exchangeCase(`admission ${at}`,[token,user,managed],'',at);await exchangeCase(`cancellation ${at}`,[token,user,managed],'',0,at);}
  for (const state of [{}, {verifier:3}, {verifier:'',projectId:3}, null, [], 'text']) await exchangeCase(`state ${JSON.stringify(state)}`,[token,user,managed],'',0,0,state);
  for (const packed of ['refresh','refresh|project|managed|tail','claustrum-tombstone:v1:fixture']) for(const response of [token,{status:200,json:{access_token:'next',expires_in:0,refresh_token:''}},{status:200,json:{access_token:'next'}},{status:400,statusText:'Bad Request',raw:'invalid_grant'},{status:500,statusText:'Oops',readError:'broken'}]) {
    queue=[response];trace=[];let result;
    try {result={value:await provider.oauth.refreshToken({refresh:packed,access:'old',expires:0})};}catch(error:any){result={error:error.message};}
    cases.push({kind:'refresh',packed,responses:[response],result,trace:structuredClone(trace),remaining:queue.length});
  }
  const logins=[];
  for(const input of [' raw-code ','https://callback.test/?code=URL%2Bcode&state='+new URL(authorizations[1].result.url).searchParams.get('state'),'https://callback.test/?code=&state=','http://not-callback.test/path']) {
    queue=[token,user,managed];trace=[];const ui:any[]=[];let result;
    try{result={value:await provider.oauth.login({onAuth:(auth:any)=>ui.push({kind:'auth',...auth}),onPrompt:async(prompt:any)=>{ui.push({kind:'prompt',...prompt});return input;}})};}catch(error:any){result={error:error.message};}
    logins.push({input,responses:[token,user,managed],result,ui,trace:structuredClone(trace)});
  }
  const sourceHashes=Object.fromEntries(await Promise.all(['packages/core/src/antigravity/oauth.ts','packages/core/src/auth.ts','packages/pi/src/index.ts','packages/pi/src/credential-cache.ts'].map(async file=>[file,crypto.createHash('sha256').update(await readFile(join(root,file))).digest('hex')])));
  await writeFile(output,normalizeOAuthClient(JSON.stringify({reference:pin,host:host.version,runtime:process.version,clock,sourceHashes,authorizations,packed,expiries,expired,cacheTrace,cases,logins},null,2)+'\n'));
  console.log(JSON.stringify({cases:cases.length,logins:logins.length}));
} finally {await rm(temporary,{recursive:true,force:true});}
