// Differential traces from unchanged upstream tests, with Pi 1.3.0 transcript helpers.
import { afterAll, mock, test } from 'bun:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { readFile, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

const reference = resolve(process.env.RPI_REFERENCE_DIR!);
const integrated = resolve(process.env.RPI_REFERENCE_INTEGRATED!);
const output = resolve(process.env.RPI_REFERENCE_OUTPUT!);
const pin = 'a97f9055017f3c1546afbfe61a19d50b428be3d8';
assert.equal(execFileSync('git',['-C',reference,'rev-parse','HEAD'],{encoding:'utf8'}).trim(),pin);
const aiRoot = join(integrated,'node_modules/@earendil-works/pi-ai');
const manifest = JSON.parse(await readFile(join(aiRoot,'package.json'),'utf8'));
assert.equal(manifest.version,'1.3.0');
const ai = await import(pathToFileURL(join(aiRoot,manifest.exports['.'].import)).href);
mock.module('@earendil-works/pi-ai',() => ai);
globalThis.fetch = async () => { throw new Error('unexpected network request in conversion reference'); };
const traces: unknown[] = [];
const hashes = {} as Record<string,string>;
const functions = {} as Record<string,(...args:any[]) => any>;
let scenario: string | undefined;
for (const [module,test,name] of [
  ['packages/pi/src/convert.ts','packages/pi/src/convert.test.ts','buildGeminiRequest'],
  ['packages/core/src/transform/gemini.ts','packages/core/src/transform/gemini.test.ts','toGeminiSchema'],
]) {
  const location = join(reference,module);
  const actual = await import(pathToFileURL(location).href);
  const original = actual[name];
  const wrapped = (...args: unknown[]) => {
    // Wrap top-level undefined so the trace distinguishes it from JSON null.
    const encode = (value: unknown) => value === undefined ? {undefined:true} : {value:structuredClone(value)};
    const trace: any = { function:name, source:module, scenario, caller:new Error().stack?.split('\n').find(line => line.includes(test))?.replace(reference+'/','').trim(), args:args.map(encode) };
    try { const result = original(...args); trace.result = encode(result); return result; }
    catch (error) { trace.error = error instanceof Error ? error.message : String(error); throw error; }
    finally { traces.push(trace); }
  };
  functions[name] = wrapped;
  mock.module(location,() => ({...actual,[name]:wrapped}));
  for (const file of [module,test]) hashes[file] = createHash('sha256').update(await readFile(join(reference,file))).digest('hex');
  await import(pathToFileURL(join(reference,test)).href);
}
test('differential boundary corpus: ordered deltas and JSON schema values', () => {
  const schemaCases: unknown[] = [null,false,true,0,'x',[],[{}],{},
    JSON.parse('{"type":"object","properties":{"__proto__":{"type":"string"},"constructor":{"type":"number"},"10":{"type":"boolean"},"2":{"type":"integer"}},"required":["__proto__","constructor","2"]}'),
    {allOf:[{properties:{a:{type:'string',description:'a'}}},{properties:{a:{description:'a',type:'string'},b:{type:'number'}}}],required:['a','b']},
  ];
  for (const type of ['integer','number','array','object',null]) {
    for (const value of [null,false,0,1.5,-1.5,1e-7,1e21,9007199254740992,'2',{},[]]) {
      schemaCases.push({type,description:'bounds',exclusiveMinimum:value,maximum:12,minimum:3,exclusiveMaximum:value,multipleOf:value});
      schemaCases.push({type,description:'meta',const:value,enum:[value],default:value,minLength:value,format:value,title:'title',additionalProperties:false});
    }
  }
  for (const keyword of ['allOf','anyOf','oneOf']) {
    for (const branches of [[],[null],[false,{}],[{type:'null'},{type:'string'}],[{type:'string',enum:['b','a']},{enum:['a','c']}],[{properties:{a:{type:'string'}},required:['a','b']},{items:{type:'number'}}]]) {
      schemaCases.push({description:'union', [keyword]:branches});
    }
  }
  for (const [i,schema] of schemaCases.entries()) {
    for (const options of [{},{claudeValidated:true},{moveNumericConstraintsToDescription:true},{claudeValidated:true,moveNumericConstraintsToDescription:true}]) {
      scenario = `schema/${i}/${JSON.stringify(options)}`;
      functions.toGeminiSchema(schema,options);
    }
  }
  const tool = (name:string,parameters?:unknown) => ({name,description:`Tool ${name}`,parameters});
  const transcript = [
    {role:'system',content:[{type:'text',text:'base'},{type:'text',text:'line'}],sections:{tail:'tail','10':'ten','2':'two'},toolsAdded:[tool('a',{type:'object'}),tool('b')],timestamp:0},
    {role:'user',content:' hi ',timestamp:1},
    {role:'system',content:'',sections:{tail:null,'2':'TWO',head:'head'},toolsRemoved:[{name:'a'}],toolsAdded:[tool('b',null),tool('c',{type:'integer',minimum:0})],timestamp:2},
    {role:'system',content:'next',sections:{tail:'tail again'},toolsAdded:[tool('a',{})],timestamp:3},
  ];
  for (const model of ['gemini-3.8-flash','gpt-oss-120b-medium','antigravity-gpt-oss-120b-medium','Antigravity-gpt-oss-120b-medium']) {
    for (const count of [1,2,3,4]) {
      scenario = `transcript/${model}/${count}`;
      functions.buildGeminiRequest({messages:transcript.slice(0,count),systemPrompt:'ignored',tools:[tool('ignored')]},{provider:'google',model});
    }
  }
  for (const whitespace of ['',' ','\ufeff','\u0085','\u00a0','😀','a\n']) {
    scenario = `text/${JSON.stringify(whitespace)}`;
    functions.buildGeminiRequest({messages:[{role:'user',content:whitespace},{role:'user',content:[{type:'text',text:whitespace}]},{role:'assistant',provider:'google',model:'gemini-3.8-flash',content:[{type:'text',text:whitespace,textSignature:'signed'},{type:'thinking',thinking:whitespace,thinkingSignature:'thought'}]}],systemPrompt:whitespace});
  }
  for (const provider of ['google','other','']) {
    for (const model of ['gemini-3.8-flash','other','']) {
      scenario = `signed-history/${provider}/${model}`;
      functions.buildGeminiRequest({messages:[
        {role:'assistant',provider:'google',model:'gemini-3.8-flash',content:[{type:'thinking',thinking:'thought',thinkingSignature:'sig1'},{type:'text',text:'text',textSignature:'sig2'},{type:'toolCall',name:'a',id:'1',arguments:null,thoughtSignature:'sig3'}]},
        {role:'toolResult',toolCallId:'1',toolName:'a',content:[{type:'text',text:'ok'}]},
        {role:'toolResult',toolCallId:'1',toolName:'a',content:[],isError:true},
        {role:'toolResult',toolCallId:'unmatched',toolName:'a',content:[]},
        {role:'assistant',provider:'other',model:'other',content:[{type:'toolCall',name:'a',id:'1',arguments:{}}]},
      ]},{provider,model});
    }
  }
  scenario = undefined;
});
afterAll(async () => {
  assert(traces.length > 0,'upstream tests must call the traced exports');
  await writeFile(output,JSON.stringify({reference:pin,host:manifest.version,sourceHashes:hashes,traces},null,2)+'\n');
  console.log(JSON.stringify({traces:traces.length,output}));
});
