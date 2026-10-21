// Deterministic calls into the pinned source; no credentials, sockets or source writes.
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
mock.module('@earendil-works/pi-ai',() => ai);
const crypto = await import('node:crypto');
let id = 0;
const ids: string[] = [];
mock.module('node:crypto',() => ({...crypto,randomUUID:() => {
  const value = `00000000-0000-4000-8000-${String(++id).padStart(12,'0')}`;
  ids.push(value);
  return value;
}}));
globalThis.fetch = async () => { throw new Error('unexpected network request in metadata reference'); };
const metadata = await import(pathToFileURL(join(root,'packages/core/src/agy-request-metadata.ts')).href);
const generation = await import(pathToFileURL(join(root,'packages/core/src/transform/agy-generation.ts')).href);
const registry = await import(pathToFileURL(join(root,'packages/core/src/model-registry.ts')).href);
const stream = await import(pathToFileURL(join(root,'packages/pi/src/stream.ts')).href);

test('request metadata, generation and session-store differential traces', async () => {
  const hashes = {} as Record<string,string>;
  for (const file of ['packages/core/src/agy-request-metadata.ts','packages/core/src/transform/agy-generation.ts','packages/core/src/model-registry.ts','packages/pi/src/stream.ts']) {
    hashes[file] = createHash('sha256').update(await readFile(join(root,file))).digest('hex');
  }
  const fnv = ['','hello','file:///workspace','file:///C:/Users/test','😀','é','a\u0000b'].map(input => ({input,result:metadata.fnv1a64Signed(input)}));
  const models = [...Object.keys(registry.AGY_NATIVE_MODEL_DEFINITIONS),
    'gemini-3.1-pro-low','gemini-3.1-pro-high','gemini-pro-agent',
    'gemini-3.6-flash-low','gemini-3.6-flash-medium','gemini-3.6-flash-high',
    'gemini-3.7-flash-low','gemini-3.7-flash-medium','gemini-3.7-flash-high',
    'claude-sonnet-4-6','claude-opus-4-6-thinking','unknown-model','CLAUDE-SONNET-5-5-LOW'];
  const payloads = [{},{contents:[]},{contents:null},{contents:[null,{},[],{parts:[]}]},
    {contents:[{role:'user',parts:[{text:'hello'}]},{role:'model',parts:[{thought:true,text:'think'},{functionCall:{id:'one',name:'tool'}}]},{role:'model',parts:[{functionResponse:{id:'one',name:'tool'}},{functionResponse:{id:'two',name:'tool'}}]}]},
    {contents:[{parts:[null,[],{functionResponse:null},{text:'x'}]}]},
  ];
  const steps = payloads.flatMap(payload => ['parts','contents','cli'].map(mode => ({payload,mode,result:metadata.countAgyRequestSteps(payload,mode)})));
  const calls: unknown[] = [];
  const session = metadata.createAgyRequestSessionContext('',{conversationId:'conversation',trajectoryId:'trajectory'});
  for (const model of models) for (const payload of payloads) for (const mode of ['parts','contents','cli']) {
    const before = structuredClone(session);
    const result = metadata.buildAgyAgentRequestMetadata(session,payload,model,1784285195116,{stepCountMode:mode});
    calls.push({model,payload,mode,before,result,after:structuredClone(session)});
    if (calls.length % 7 === 0) session.lastExecutionId = `execution-${calls.length}`;
  }
  const generations: unknown[] = [];
  for (const model of models) for (const config of [{},{maxOutputTokens:0},{maxOutputTokens:1000,max_output_tokens:2000},{max_output_tokens:4000},{maxOutputTokens:null,max_output_tokens:3000},{thinkingConfig:{thinkingBudget:111,includeThoughts:false}},{max_output_tokens:null},{maxOutputTokens:1e22}]) {
    const result = structuredClone(config);
    generation.applyAgyNativeGenerationConfig(model,result);
    generations.push({model,config,result});
  }
  const finalizations: unknown[] = [];
  for (const model of models) for (const tools of [undefined,[],[{functionDeclarations:[{name:'read',parameters:{type:'OBJECT'}}]}]]) {
    const request:any = {generationConfig:{maxOutputTokens:99},'2':'integer key',sessionId:'old',contents:payloads[4].contents,toolConfig:{old:true},...(tools ? {tools} : {}),systemInstruction:{parts:[{text:'system'}]},other:true};
    const before = structuredClone(request);
    const session = metadata.createAgyRequestSessionContext('',{conversationId:'conversation',trajectoryId:'trajectory'});
    const result = stream.finalizePiAntigravityRequest(request,model,{session,timestamp:1000});
    finalizations.push({model,before,result,request,session});
  }
  const stores: unknown[] = [];
  for (const limits of [{ttlMs:100,maxEntries:3},{ttlMs:0,maxEntries:0},{ttlMs:-1,maxEntries:2}]) {
    let now = 0;
    const store = new metadata.AgyRequestSessionStore('file:///workspace',{...limits,now:() => now});
    const operations: any[] = [];
    const inputs = [
      ['beginRequest','a',100],['beginRequest','a',100],['getOrCreate','b',100],['getOrCreate','c',100],['beginRequest','d',101],
      ['completeExecution','d',101],['beginRequest','d',90],['getOrCreate','b',201],['getOrCreate','b',500],['completeExecution','missing',500],
      ['getOrCreate','',500],['getOrCreate','x',500],['getOrCreate','y',500],['getOrCreate','z',500],['delete','y',500],['clear','',500],['beginRequest','a',-10],
    ] as const;
    for (const [op,key,time] of inputs) {
      now = time;
      ids.length = 0;
      const result = store[op](key);
      operations.push({op,key,now,ids:[...ids],...(result === undefined ? {} : {result:structuredClone(result)}),size:store.size,has:['a','b','c','d','','x','y','z','missing'].map(key => [key,store.has(key)])});
    }
    stores.push({limits,operations});
  }
  await writeFile(output,JSON.stringify({reference:pin,host:manifest.version,sourceHashes:hashes,fnv,steps,calls,generations,finalizations,stores},null,2)+'\n');
  console.log(JSON.stringify({calls:calls.length,generations:generations.length,finalizations:finalizations.length,stores:stores.length}));
});
