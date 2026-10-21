// Scripted transport around the original provider, using the integrated Pi 1.3.0 host.
import { test, mock } from 'bun:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { readFile, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

const root = resolve(process.env.RPI_REFERENCE_DIR!);
const integrated = resolve(process.env.RPI_REFERENCE_INTEGRATED!);
const output = resolve(process.env.RPI_REFERENCE_OUTPUT!);
const pin = 'a97f9055017f3c1546afbfe61a19d50b428be3d8';
assert.equal(execFileSync('git',['-C',root,'rev-parse','HEAD'],{encoding:'utf8'}).trim(),pin);
const aiRoot = join(integrated,'node_modules/@earendil-works/pi-ai');
const manifest = JSON.parse(await readFile(join(aiRoot,'package.json'),'utf8'));
assert.equal(manifest.version,'1.3.0');
const ai = await import(pathToFileURL(join(aiRoot,manifest.exports['.'].import)).href);
const createStream = ai.createAssistantMessageEventStream;
let events: any[] = [];
mock.module('@earendil-works/pi-ai',() => ({...ai,createAssistantMessageEventStream:() => {
  const stream = createStream();
  const push = stream.push.bind(stream);
  stream.push = (event:any) => { push(event); events.push(structuredClone(event)); };
  return stream;
}}));
const crypto = await import('node:crypto');
let sequence = 0;
let ids: string[] = [];
const nextId = () => {
  const value = `00000000-0000-4000-8000-${String(++sequence).padStart(12,'0')}`;
  ids.push(value);
  return value;
};
mock.module('node:crypto',() => ({...crypto,randomUUID:nextId}));
const originalRandom = globalThis.crypto.randomUUID;
globalThis.crypto.randomUUID = nextId as any;
const originalNow = Date.now;
Date.now = () => 1784285195116;
const originalMonotonic = performance.now;
performance.now = () => 1000;
globalThis.fetch = async () => { throw new Error('unexpected network request in stream reference'); };
const core = await import(pathToFileURL(join(root,'packages/core/dist/index.js')).href);
let response: Response;
let requests: any[] = [];
mock.module('@cortexkit/antigravity-auth-core',() => ({...core,fetchWithAgyCliTransport:async (url:string,init:any) => {
  requests.push({url,method:init.method,headers:init.headers,body:init.body});
  return response;
}}));
const stream = await import(pathToFileURL(join(root,'packages/pi/src/stream.ts')).href);
const registry = await import(pathToFileURL(join(root,'packages/core/src/model-registry.ts')).href);
const jsonFrame = (value:any) => `data: ${JSON.stringify(value)}\n\n`;
const terminal = {candidates:[{content:{parts:[{text:'hello'}]},finishReason:'STOP'}]};

test('original physical requests and emitted stream frames', async () => {
  try {
    const cases: any[] = [];
    const baseModel = {id:'antigravity-gemini-3.8-flash',name:'test',api:'google-generative-ai',provider:'google',baseUrl:'https://cloudcode-pa.googleapis.com',reasoning:true,input:['text','image'],cost:{input:0,output:0,cacheRead:0,cacheWrite:0},contextWindow:1048576,maxTokens:65536};
    const context = {messages:[{role:'system',content:'Be useful',toolsAdded:[{name:'read',description:'Read text',parameters:{type:'object',properties:{path:{type:'string'}},required:['path']}}],timestamp:0},{role:'user',content:'hello',timestamp:10}]};
    const run = async (name:string,model:any,context:any,options:any,chunks:string[],status=200,bodyNull=false) => {
      events = []; requests = []; ids = [];
      let pulls = 0, cancelled = false;
      response = new Response(bodyNull ? null : new ReadableStream({pull(controller) {
        if (pulls < chunks.length) controller.enqueue(new TextEncoder().encode(chunks[pulls++]));
        else controller.close();
      },cancel(){cancelled = true;}}),{status});
      const vault = stream.createCortexKitAntigravityVaultStream(async (dispatch:any) => dispatch({accessToken:'fixture-access',projectId:'fixture-project'}),'fixture-marker');
      const result = await vault(model,context,{...options,apiKey:'fixture-marker'}).result();
      cases.push({name,model,context,options,chunks,status,bodyNull,ids:[...ids],requests:structuredClone(requests),events:structuredClone(events),result:structuredClone(result),pulls,cancelled});
    };
    let n = 0;
    for (const id of ['antigravity-gemini-4-argon','antigravity-gemini-3.8-flash','antigravity-gemini-3.5-flash','antigravity-claude-sonnet-5-5','antigravity-claude-opus-5-5','antigravity-gpt-oss-120b-medium','antigravity-gemini-3.7-flash','unknown-model']) {
      for (const reasoning of [undefined,'minimal','low','medium','high','xhigh']) {
        await run(`prepare/${++n}`,{...baseModel,id},context,{sessionId:`prepare-${n}`,...(reasoning ? {reasoning} : {}),maxTokens:123,temperature:0.2,topP:0.7},[jsonFrame(terminal)]);
      }
    }
    const streaming: [string,string[]][] = [
      ['text',[jsonFrame({response:{candidates:[{content:{parts:[{text:'a'}]}}]}}),jsonFrame({candidates:[{content:{parts:[{text:'b',thoughtSignature:'text-sig'}]},finishReason:'MAX_TOKENS'}],usageMetadata:{promptTokenCount:20,cachedContentTokenCount:5,candidatesTokenCount:8,thoughtsTokenCount:3,totalTokenCount:999}})]],
      ['thinking-tools',[jsonFrame({candidates:[{content:{parts:[{thought:true,text:'think',thoughtSignature:'thought-sig'},{thought:true,text:'',thoughtSignature:'batch-sig'},{functionCall:{name:'read',args:{path:'x'}}},{functionCall:{id:'second',name:'read'},thoughtSignature:'call-sig'}]},finishReason:'STOP'}]})]],
      ['signature-only',[jsonFrame({candidates:[{content:{parts:[{thoughtSignature:'before-text'},{text:'a'},{thoughtSignature:'after-text'},{thought:true,text:'hmm'},{text:'b'}]},finishReason:'STOP'}]})]],
      ['missing-terminal',[jsonFrame({candidates:[{content:{parts:[{text:'partial'}]}}]})]],
      ['empty-terminal',[jsonFrame({candidates:[{content:{parts:[]},finishReason:'STOP'}]})]],
      ['embedded-error',[jsonFrame({candidates:[{content:{parts:[{text:'partial'}]}}]}),jsonFrame({error:{code:403,status:'PERMISSION_DENIED',message:'fixture-access at fixture-project',details:[{reason:'NO_ACCESS'}]}})]],
      ['blocked',[jsonFrame({promptFeedback:{blockReason:'SAFETY',blockReasonMessage:'blocked'}})]],
      ['malformed',['event: hi\ndata: broken\n\ndata: [DONE]\n\ndata: \n\n',jsonFrame(terminal)]],
      ['crlf-split',['data: '+JSON.stringify(terminal)+'\r','\n\r','\n']],
      ['trailing-frame',['data: '+JSON.stringify(terminal)]],
      ['unknown-finish',[jsonFrame({candidates:[{content:{parts:[{text:'text'}]},finishReason:'OTHER'}]})]],
    ];
    for (const [name,chunks] of streaming) await run(`stream/${name}`,baseModel,context,{sessionId:`stream-${name}`},chunks);
    await run('stream/http-error',baseModel,context,{sessionId:'http-error'},['fixture-access / fixture-project'],403);
    await run('stream/no-body',baseModel,context,{sessionId:'no-body'},[],204,true);
    await run('stream/late-usage',{...baseModel,id:'antigravity-gpt-oss-120b-medium'},context,{sessionId:'late-usage'},[jsonFrame(terminal),jsonFrame({usageMetadata:{promptTokenCount:5,candidatesTokenCount:3,thoughtsTokenCount:2}})]);
    for (const [name,current] of [['continuation-tool',{candidates:[{content:{parts:[{functionCall:{id:'tool',name:'read',args:{}}}]},finishReason:'STOP'}]}],['continuation-text',terminal],['continuation-final',terminal]] as const) {
      await run(name,baseModel,context,{sessionId:'continuation'},[jsonFrame(current)]);
    }
    const hashes = {} as Record<string,string>;
    for (const file of ['packages/pi/src/stream.ts','packages/pi/src/convert.ts','packages/core/src/agy-request-metadata.ts','packages/core/src/transform/agy-generation.ts','packages/core/dist/index.js']) hashes[file] = createHash('sha256').update(await readFile(join(root,file))).digest('hex');
    await writeFile(output,JSON.stringify({reference:pin,host:manifest.version,now:Date.now(),sourceHashes:hashes,cases},null,2)+'\n');
    assert(cases.length > 60);
    console.log(JSON.stringify({cases:cases.length,requests:cases.reduce((n,c) => n+c.requests.length,0),events:cases.reduce((n,c) => n+c.events.length,0)}));
  } finally { Date.now = originalNow; performance.now = originalMonotonic; globalThis.crypto.randomUUID = originalRandom; }
});
