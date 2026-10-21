import { test, mock } from 'bun:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { readFile, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
const root = resolve(process.env.RPI_REFERENCE_DIR!);
const output = resolve(process.env.RPI_REFERENCE_OUTPUT!);
const pin = 'a97f9055017f3c1546afbfe61a19d50b428be3d8';
assert.equal(execFileSync('git',['-C',root,'rev-parse','HEAD'],{encoding:'utf8'}).trim(),pin);
globalThis.fetch = async () => { throw new Error('unexpected network in diagnostics reference'); };
const aiRoot = join(resolve(process.env.RPI_REFERENCE_INTEGRATED!),'node_modules/@earendil-works/pi-ai');
const manifest = JSON.parse(await readFile(join(aiRoot,'package.json'),'utf8'));
assert.equal(manifest.version,'1.3.0');
const ai = await import(pathToFileURL(join(aiRoot,manifest.exports['.'].import)).href);
mock.module('@earendil-works/pi-ai',() => ai);
const redaction = await import(pathToFileURL(join(root,'packages/core/src/credential-redaction.ts')).href);
const {parseGeminiSse,updateUsage} = await import(pathToFileURL(join(root,'packages/pi/src/stream.ts')).href);

test('redaction and SSE byte-boundary reference cases', async () => {
  const cases: any[] = [];
  const texts = ['opaque-selected-token Bearer other-token eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.signature',
    'project-0 and proj', 'prefixproject=private-project-123456','éproject=private-project-123456',
    'Project: Unknown project=na project=n/a project=**** project=abc project=abc-def-long',
    'Bearer Kelvin Bearer ſuffix', 'Bearer opaque== trailing', 'eyJtest.part.last_ eyJtest.part.last-',
    'project = PRIVATE-PROJECT-123456', 'opaque opaque-long opaque', 'fixed costs: token count 12'];
  for (const ws of [' ','\t','\r\n','\u00a0','\ufeff','\u0085','\u2003','\u2028']) texts.push(`Project${ws}id${ws}:${ws}private-project-123456 Bearer${ws}selected`);
  for (const text of texts) for (const credentials of [[],['opaque-selected-token'],['opaque','opaque-long'],['selected']]) for (const projects of [[],['project-0','proj'],['private-project-123456']]) {
    cases.push({text,credentials,projects,result:redaction.redactCredentialText(text,credentials,projects)});
  }
  const frames: any[] = [];
  for (const value of [{error:{message:'opaque-selected-token'}},{type:'error',message:'Bearer selected',project:'private-project-123456'},
    {response:{error:'opaque-selected-token'}},{response:{candidates:[{content:{parts:[{text:'error opaque-selected-token'}]}}]}},
    {error:null,output:'opaque-selected-token'}, {type:'done',error:0,output:'opaque-selected-token'}]) {
    for (const text of [JSON.stringify(value),JSON.stringify(value).replaceAll('opaque','\\u006fpaque').replaceAll('error','\\u0065rror')]) {
      const credentials = ['opaque-selected-token'], projects = ['private-project-123456'];
      frames.push({text,credentials,projects,result:redaction.redactCredentialFrame(text,credentials,projects)});
    }
  }
  const encode = (value:string) => Array.from(new TextEncoder().encode(value));
  const inputs = [
    encode('data: {"response":{"x":"😀é"}}\r\n\r\ndata: {"y":2}\n\n'),
    encode('event: x\ndata: invalid\ndata: [DONE]\ndata: \ndata: {"a":1}\ndata: {"b":2}\n\n'),
    encode('\ufeffdata: {"x":1}\n\ndata: [1,2]\n\ndata: null'),
    [...encode('data: {"x":"'),255,...encode('"}\n\n'),240,159],
    encode('data: {"response":null}\ndata: {"response":[]}\ndata: {"response":1}\n\n'),
  ];
  const sse: any[] = [];
  for (const input of inputs) for (const width of [1,2,3,7,input.length]) {
    const chunks = [];
    for (let i=0; i<input.length; i+=width) chunks.push(input.slice(i,i+width));
    let read = 0, eof = false;
    const body = new ReadableStream({pull(controller) {
      if (read < chunks.length) controller.enqueue(Uint8Array.from(chunks[read++]));
      else { eof = true; controller.close(); }
    }},{highWaterMark:0});
    const events = [];
    for await (const value of parseGeminiSse(new Response(body))) events.push({value,read,eof});
    sse.push({chunks,events});
  }
  const sourceHashes = {} as Record<string,string>;
  for (const file of ['packages/pi/src/stream.ts','packages/core/src/credential-redaction.ts']) sourceHashes[file] = createHash('sha256').update(await readFile(join(root,file))).digest('hex');
  const usage: any[] = [];
  for (const cost of [{input:0,output:0,cacheRead:0,cacheWrite:0},{input:2,output:5,cacheRead:1,cacheWrite:3,tiers:[{inputTokensAbove:50,input:4,output:8,cacheRead:2,cacheWrite:6},{inputTokensAbove:100,input:8,output:16,cacheRead:4,cacheWrite:12}]}]) {
    const model = {cost};
    const output = {usage:{input:5,output:7,cacheRead:3,cacheWrite:2,cacheWrite1h:1,totalTokens:17,cost:{input:0,output:0,cacheRead:0,cacheWrite:0,total:0}}};
    for (const metadata of [undefined,null,{}, {promptTokenCount:48},{promptTokenCount:49},{promptTokenCount:99},{promptTokenCount:100},{candidatesTokenCount:8,thoughtsTokenCount:2},{thoughtsTokenCount:3},{cachedContentTokenCount:200},{candidatesTokenCount:null}, {totalTokenCount:99999}]) {
      const before = structuredClone(output);
      updateUsage(model,output,metadata);
      usage.push({model,before,...(metadata === undefined ? {} : {metadata}),result:structuredClone(output)});
    }
  }
  await writeFile(output,JSON.stringify({reference:pin,host:manifest.version,sourceHashes,cases,frames,sse,usage},null,2)+'\n');
  console.log(JSON.stringify({redaction:cases.length,frames:frames.length,sse:sse.length,usage:usage.length}));
});
