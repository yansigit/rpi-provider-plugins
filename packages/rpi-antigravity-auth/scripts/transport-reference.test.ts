// Capture the unchanged serializer through its TLS boundary; no sockets are opened.
import { test, mock } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { readFile, writeFile } from 'node:fs/promises';
import { Duplex } from 'node:stream';
import { gzipSync } from 'node:zlib';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
const root = resolve(process.env.RPI_REFERENCE_DIR!);
const output = resolve(process.env.RPI_REFERENCE_OUTPUT!);
const pin = 'a97f9055017f3c1546afbfe61a19d50b428be3d8';
assert.equal(execFileSync('git',['-C',root,'rev-parse','HEAD'],{encoding:'utf8'}).trim(),pin);
let connections: any[] = [], writes: any[] = [];
let reply = Buffer.from('HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok');
class Socket extends Duplex {
  readonly stage:string;
  readonly parent?:Socket;
  constructor(stage:string,parent?:Socket) { super(); this.stage = stage; this.parent = parent; }
  _read() {}
  _write(chunk:Buffer,_encoding:string,callback:(error?:Error) => void) {
    writes.push({stage:this.stage,bytes:Array.from(chunk)});
    queueMicrotask(() => {
      this.push(this.stage === 'proxy' ? Buffer.from('HTTP/1.1 200 Connection established\r\n\r\n') : reply);
      if (this.stage === 'tls') this.push(null);
    });
    callback();
  }
  _destroy(error:Error|null,callback:(error:Error|null) => void) { this.parent?.destroy(); callback(error); }
}
const net = await import('node:net'), tls = await import('node:tls');
mock.module('node:net',{exports:{...Object.fromEntries(Object.entries(net).filter(([key]) => key !== 'default')),connect:(options:any) => {
  connections.push({stage:'proxy',host:options.host,port:options.port});
  const socket = new Socket('proxy'); queueMicrotask(() => socket.emit('connect')); return socket;
}}});
mock.module('node:tls',{exports:{...Object.fromEntries(Object.entries(tls).filter(([key]) => key !== 'default')),connect:(options:any) => {
  connections.push({stage:'tls',host:options.host,port:options.port,servername:options.servername,tunnel:!!options.socket});
  const socket = new Socket('tls',options.socket); queueMicrotask(() => socket.emit('secureConnect')); return socket;
}}});
const transport = await import(pathToFileURL(join(root,'packages/core/dist/agy-transport.js')).href);
const fingerprint = await import(pathToFileURL(join(root,'packages/core/dist/fingerprint.js')).href);
globalThis.fetch = async () => { throw new Error('unexpected fetch in transport fixture'); };
const keys = ['HTTPS_PROXY','https_proxy','ALL_PROXY','all_proxy','NO_PROXY','no_proxy'];
const original = Object.fromEntries(keys.map(key => [key,process.env[key]]));
const environment = (values:Record<string,string>) => { for (const key of keys) { if (key in values) process.env[key] = values[key]; else delete process.env[key]; } };

test('raw ordered request bytes and proxy selection', {timeout:10000}, async () => {
  try {
    const cases: any[] = [];
    const run = async (url:string,init:any,env:Record<string,string>) => {
      environment(env); connections = []; writes = [];
      const pairs = transport.buildAgyCliHeaderPairs(url,init);
      const response = await transport.fetchWithAgyCliTransport(url,init,{timeoutMs:500,idleTimeoutMs:500});
      assert.equal(await response.text(),'ok');
      cases.push({url,init,environment:env,pairs,connections:structuredClone(connections),writes:structuredClone(writes)});
    };
    for (const url of ['https://service.example.test/v1internal:streamGenerateContent?alt=sse#ignored','https://service.example.test:443/json','https://service.example.test:8443/json?x=%20']) {
      for (const body of ['', 'hello','😀é', '{"request":{"contents":[]}}']) for (const headers of [[],[['Authorization',' Bearer token '],['User-Agent','fixture-agent'],['X-Ignored','drop']], [['Content-Type',''],['Accept-Encoding','identity'],['authorization',''],['user-agent','']],[['authorization','Bearer a'],['Authorization','Bearer b'],['content-type','text/plain'],['CONTENT-TYPE','application/json']]]) {
        await run(url,{method:'POST',headers,body},{NO_PROXY:'*'});
      }
    }
    for (const env of [{},{HTTPS_PROXY:'http://proxy.test'},{HTTPS_PROXY:'https://proxy.test:443'}, {https_proxy:'http://user:p%40ss@proxy.test:9000'},
      {HTTPS_PROXY:'bad-url',ALL_PROXY:'http://fallback.test:8000'}, {HTTPS_PROXY:'',https_proxy:'http://lower.test:9001'},
      {ALL_PROXY:'socks5://proxy.test:1234'}, {all_proxy:'http://proxy.test:8080'},
      ...['example.test','.example.test','service.example.test:443','*.example.test','*','EXAMPLE.TEST','other.test',' .test '].map(NO_PROXY => ({HTTPS_PROXY:'http://proxy.test:8000',NO_PROXY})),
      {HTTPS_PROXY:'http://proxy.test:8000',NO_PROXY:'',no_proxy:'*'}, {HTTPS_PROXY:'http://proxy.test:8000',NO_PROXY:'no-match',no_proxy:'*'}]) {
      await run('https://service.example.test/v1internal:streamGenerateContent',{body:'x'},env);
    }
    const agents = [];
    for (const version of ['1.3.5','1.1.24']) for (const platform of ['linux','darwin','win32','']) for (const arch of ['x64','ia32','arm64','']) for (const auth of ['consumer','workload']) {
      agents.push({version,platform,arch,auth,result:fingerprint.buildAntigravityHarnessUserAgent(version,platform,arch,auth)});
    }
    environment({NO_PROXY:'*'});
    const compressed = gzipSync('ok');
    const responses: any[] = [];
    for (const [head,body] of [
      ['HTTP/1.0 201 Created\r\nContent-Length: 2\r\nX-Test: one\r\nX-Test: two\r\nSet-Cookie: a=1\r\nSet-Cookie: b=2',Buffer.from('ok')],
      [`HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: ${compressed.length}`,compressed],
      [`HTTP/1.1 200 OK\r\nContent-Length: ${compressed.length}\r\nContent-Encoding: gzip`,compressed],
      ['HTTP/1.1 200 OK\r\nTransfer-Encoding: CHUNKED\r\nContent-Length: 100',Buffer.from('2\r\nok\r\n0\r\n\r\n')],
      ['HTTP/1.1 200 OK\r\nContent-Length: +2suffix\r\nbroken\r\n:ignored',Buffer.from('okextra')],
      ['HTTP/1.1 200\r\nX-Header:  value  ',Buffer.from('ok')],
      ['HTTP/1.1 200 OK\r\nContent-Length: invalid',Buffer.from('ok')],
      ['HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Length: -1',Buffer.from('okextra')],
    ] as const) {
      reply = Buffer.concat([Buffer.from(head+'\r\n\r\n'),body]);
      const response = await transport.fetchWithAgyCliTransport('https://service.example.test/json',{body:'x'},{timeoutMs:500,idleTimeoutMs:500});
      responses.push({head,wireBody:Array.from(body),status:response.status,statusText:response.statusText,headers:Array.from(response.headers),body:await response.text()});
    }
    const headerInputs: any[] = [];
    for (const headers of [[['bad name','x']],[['x','a\0b']],[['Authorization','\n Bearer a \r\n']],[['x','Ā']],[['Ā','x']],[['bad name','x'],['x','😀']],[['x','a\nb']],[['x','\u000b x \u000b']],[['x','\u00a0 x \u00a0']]]) {
      try { headerInputs.push({headers,result:transport.buildAgyCliHeaderPairs('https://service.example.test/json',{headers})}); }
      catch (error:any) { headerInputs.push({headers,error:error.message}); }
    }
    const sourceHashes = {} as Record<string,string>;
    for (const file of ['packages/core/src/agy-transport.ts','packages/core/src/fingerprint.ts','packages/core/dist/agy-transport.js','packages/core/dist/fingerprint.js']) sourceHashes[file] = createHash('sha256').update(await readFile(join(root,file))).digest('hex');
    await writeFile(output,JSON.stringify({reference:pin,runtime:process.version,platform:process.platform,arch:process.arch,sourceHashes,cases,agents,responses,headerInputs},null,2)+'\n');
    console.log(JSON.stringify({cases:cases.length,agents:agents.length,responses:responses.length}));
  } finally {
    for (const key of keys) { if (original[key] === undefined) delete process.env[key]; else process.env[key] = original[key]; }
  }
});
