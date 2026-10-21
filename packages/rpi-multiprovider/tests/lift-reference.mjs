import assert from 'node:assert/strict';
import crypto, { createHash } from 'node:crypto';
import { syncBuiltinESMExports } from 'node:module';
import { readFile } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { pathToFileURL } from 'node:url';
const root = resolve(process.argv[2] ?? '../provider-references/multiprovider');
const hashes = {};
for (const name of ['lift.ts', 'service.ts', 'errors.ts']) hashes[name] = createHash('sha256').update(await readFile(join(root, 'src', name))).digest('hex');
assert.equal(hashes['service.ts'], '2eafa61662b3ee75a96a7b07a6ccad62ca600bbb2139a407b81cd9983564a026');
assert.equal(hashes['lift.ts'], '887b91aab533565504449b6094c1a2b076832b0fdc73bfc352e2c46a53ac3695');
let now = 1700000000000, sequence = 0, pause;
const originalDate = Date;
class Clock extends originalDate { constructor(...args) { super(...(args.length ? args : [now])); } static now() { return now; } }
globalThis.Date = Clock;
Object.defineProperty(globalThis.performance, "now", { value: () => now - 1700000000000 });
crypto.randomUUID = () => `lease-${++sequence}`;
syncBuiltinESMExports();
const originalTimeout = globalThis.setTimeout;
globalThis.setTimeout = (fn, ms, ...args) => {
  if (ms !== 250) return originalTimeout(fn, ms, ...args);
  pause(); queueMicrotask(() => fn(...args)); return 0;
};
const { liftProvider } = await import(pathToFileURL(join(root, 'src/lift.ts')));
const { MultiProviderService } = await import(pathToFileURL(join(root, 'src/service.ts')));
const model = { id:'m', name:'M', provider:'p', api:'test-api', baseUrl:'https://fixture.invalid', reasoning:false, input:['text'], cost:{input:0,output:0,cacheRead:0,cacheWrite:0}, contextWindow:1000, maxTokens:100 };
const message = (stopReason='pending', errorMessage) => ({ role:'assistant', content:[], api:model.api, provider:model.provider, model:model.id, usage:{input:0,output:0,cacheRead:0,cacheWrite:0,totalTokens:0,cost:{input:0,output:0,cacheRead:0,cacheWrite:0,total:0}}, stopReason, ...(errorMessage === undefined ? {} : {errorMessage}), timestamp:0 });
const start = tag => ({type:'start',partial:{...message(),fixture:tag}});
const done = {type:'done',reason:'stop',message:message('stop')};
const failed = (text='429: limited', reason='error') => ({type:'error',reason,error:message(reason,text)});
const error = (text, extra={}) => ({events:[start('rejected'),failed(text)],...extra});
const success = {events:[start('accepted'),done]};
const output = type => ({type,contentIndex:0,...(type==='toolcall_start'?{id:'call-1',name:'fixture'}:{}),partial:message()});
const cases = [
 {name:'success',sends:{a:[success]}},
 {name:'same-account tolerance then switch',sends:{a:[error(),error(),error()],b:[success]}},
 {name:'fatal errors also receive same-account tolerance',sends:{a:[error('400: bad'),error('400: bad'),error('400: bad')],b:[success]}},
 {name:'recovery on same account',sends:{a:[error(),success]}},
 {name:'start is replaced by latest',sends:{a:[{events:[start('discarded'),start('retained'),done]}]}},
 {name:'no initial start',sends:{a:[{events:[done]}]}},
 {name:'no start on error',errorsBeforeSwitch:1,sends:{a:[{events:[failed('400: bad')]}]}},
 {name:'response status wins classification',sends:{a:[error('opaque',{response:{status:503,headers:{'retry-after':'2'}}}),error('opaque'),error('opaque',{response:{status:503,headers:{'retry-after':'3'}}})],b:[success]}},
 {name:'response reset between retries',sends:{a:[error('opaque',{response:{status:503,headers:{'retry-after':'2'}}}),error('opaque'),error('opaque')]}},
 {name:'throw on start skips tolerance',sends:{a:[{throw:'503 setup'}],b:[success]}},
 {name:'throw during iteration before output',sends:{a:[{events:[start('hidden'),{throw:'503 iteration'}]}],b:[success]}},
 {name:'missing terminal is fatal',sends:{a:[{events:[start('hidden')]}],b:[success]}},
 {name:'missing terminal custom retry',classification:{kind:'transient',retryable:true},sends:{a:[{events:[start('hidden')]}],b:[success]}},
 {name:'auth failure skips account',authErrors:{a:'429 auth setup'},sends:{b:[success]}},
 {name:'auth failure fatal',authErrors:{a:'400 auth setup'},sends:{b:[success]}},
 {name:'exhausted uses last setup error',authErrors:{a:'429 auth A',b:'429 auth B'},sends:{}},
 {name:'last rejection beats later setup error',errorsBeforeSwitch:1,authErrors:{b:'429 auth B'},sends:{a:[error()]}},
 {name:'attempt bound surfaces later setup error',errorsBeforeSwitch:1,maxAccountAttempts:2,authErrors:{b:'429 auth B'},sends:{a:[error()]}},
 {name:'all accounts excluded',excluded:['a','b'],sends:{}},
 {name:'zero attempts',maxAccountAttempts:0,sends:{}},
 {name:'one account attempt',maxAccountAttempts:1,sends:{a:[error(),error(),error()],b:[success]}},
 {name:'all accounts rejected',errorsBeforeSwitch:1,sends:{a:[error('429 first')],b:[error('503 last')]}},
 {name:'external failover callback failure',errorsBeforeSwitch:1,failoverError:'fixture callback failed',sends:{a:[error()],b:[success]}},
 {name:'external failover repins implicit affinity',errorsBeforeSwitch:1,surface:true,affinityKey:'session',sends:{a:[error()],b:[success]}},
 {name:'external failover retains explicit pin',errorsBeforeSwitch:1,surface:true,affinityKey:'session',pin:'a',sends:{a:[error()],b:[success]}},
 {name:'external failover without affinity',errorsBeforeSwitch:1,surface:true,affinity:false,affinityKey:'session',sends:{a:[error()],b:[success]}},
 {name:'cancel during tolerance pause',abortOnPause:true,sends:{a:[error()],b:[success]}},
 {name:'abort terminal has no tolerance',sends:{a:[{events:[start('aborted'),failed('cancelled','aborted')]}],b:[success]}},
 {name:'cancel before error',sends:{a:[{abort:true,events:[start('aborted'),failed()]}],b:[success]}},
 {name:'cancel before setup throw',sends:{a:[{abort:true,throw:'503 cancelled'}],b:[success]}},
 {name:'preserve delegated retries when requested',disableProviderRetries:false,sends:{a:[success]}},
];
for (const type of ['text_start','thinking_start','toolcall_start','text_delta','thinking_delta','toolcall_delta']) {
 cases.push({name:`${type} closes retry window`,sends:{a:[{events:[start('visible'),output(type),failed()]}],b:[success]}});
 cases.push({name:`${type} before iterator throw`,sends:{a:[{events:[start('visible'),output(type),{throw:'503 iterator'}]}],b:[success]}});
}
for (const threshold of [0,1,2,2.9,4]) cases.push({name:`tolerance ${threshold}`,errorsBeforeSwitch:threshold,sends:{a:Array.from({length:4},()=>error()),b:[success]}});

function canonicalFailure(failure) { return {...failure,...(failure.cause===undefined?{}:{cause:{name:failure.cause.name,message:failure.cause.message}})}; }
const variants = cases.flatMap(scenario => ["streamSimple", "stream"].map(kind => ({...scenario, name:`${scenario.name} (${kind})`, kind})));
for (const scenario of variants) {
 now=1700000000000; sequence=0;
 const signal = new AbortController(), trace=[], events=[], counts={};
 pause=()=>{trace.push({pause:250});now+=250;if(scenario.abortOnPause)signal.abort();};
 const accounts=['a','b'].map(id=>({id,label:id.toUpperCase(),authKind:'api-key',credentialRef:id}));
 const service=new MultiProviderService({...(scenario.errorsBeforeSwitch===undefined?{}:{errorsBeforeSwitch:scenario.errorsBeforeSwitch}),...(scenario.affinity===undefined?{}:{affinity:scenario.affinity})});
 service.registerProvider({id:'p',label:'P',accounts:()=>accounts,classifyFailure(failure,account){trace.push({failure:canonicalFailure(failure),account:account.id});return scenario.classification;}});
 if(scenario.pin) await service.pinAccount('p','session',scenario.pin);
 function send(kind,model,_context,options) {
  const account=options.apiKey;
  const at=counts[account]??0;counts[account]=at+1;
  trace.push({send:account,kind,maxRetries:options.maxRetries});
  const script=scenario.sends[account]?.[at]??success;
  if(script.abort) signal.abort();
  if(script.throw) throw new Error(script.throw);
  return (async function*(){
   if(script.response)await options.onResponse?.(script.response,model);
   for(const event of script.events??[]) {if(event.throw)throw new Error(event.throw);yield structuredClone(event);}
  })();
 }
 const provider={id:'p',name:'P',auth:{},getModels:()=>[model],stream:(...args)=>send('stream',...args),streamSimple:(...args)=>send('streamSimple',...args)};
 const lifted=liftProvider(provider,service,{
  resolveAuth(account){trace.push({auth:account.id});if(scenario.authErrors?.[account.id])throw new Error(scenario.authErrors[account.id]);return{auth:{apiKey:account.id}};},
  ...(scenario.maxAccountAttempts===undefined?{}:{maxAccountAttempts:scenario.maxAccountAttempts}),
  ...(scenario.disableProviderRetries===undefined?{}:{disableProviderRetries:scenario.disableProviderRetries}),
  excludeAccountIds:()=>scenario.excluded??[],
  ...(scenario.affinityKey===undefined?{}:{affinityKey:()=>scenario.affinityKey}),
  onFailover(info){trace.push({failover:{...info,failure:canonicalFailure(info.failure)}});if(scenario.failoverError)throw new Error(scenario.failoverError);return scenario.surface;},
 });
 const kind=scenario.kind;
 for await(const event of lifted[kind](model,{messages:[]},{signal:signal.signal,maxRetries:7})) events.push(structuredClone(event));
 scenario.result={trace,events,snapshot:await service.snapshot(),affinity:service.getAffinity("p","session")??null,now};
}
process.stdout.write(JSON.stringify({reference:'bd97d65a36f541935dc91e33ba15d7910a30f9d7',hashes,model,cases:variants},null,2)+'\n');
