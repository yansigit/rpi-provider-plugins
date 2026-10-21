// Ordinary OAuth fetch is distinct from the custom AGY transport.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { readFile,writeFile } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { createServer } from 'node:tls';
import { gzipSync,deflateSync,deflateRawSync,brotliCompressSync,zstdCompressSync } from 'node:zlib';
import { join,resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
const root=resolve(process.env.RPI_REFERENCE_DIR!);
const pin='a97f9055017f3c1546afbfe61a19d50b428be3d8';
assert.equal(execFileSync('git',['-C',root,'rev-parse','HEAD'],{encoding:'utf8'}).trim(),pin);
const {fetchWithActiveTimeout}=await import(pathToFileURL(join(root,'packages/core/dist/fetch-timeout.js')).href);
const text='{"access_token":"fixture-access","message":"😀é"}';
const gzip=gzipSync(text), deflate=deflateSync(text), br=brotliCompressSync(text);
const inputs:any[]=[
  {name:'plain',wireBody:Buffer.from(text)},
  {name:'bom',wireBody:Buffer.concat([Buffer.from([239,187,191]),Buffer.from(text)])},
  {name:'utf8 ignores declared charset',headers:[['Content-Type','text/plain;charset=iso-8859-1']],wireBody:Buffer.from([0xe9,0xf0,0x9f])},
  {name:'gzip',coding:'gzip',wireBody:gzip},
  {name:'gzip members',coding:'gzip',wireBody:Buffer.concat([gzip,gzip])},
  {name:'gzip zero padding',coding:'gzip',wireBody:Buffer.concat([gzip,Buffer.alloc(32)])},
  {name:'gzip partial header',coding:'gzip',wireBody:gzip.subarray(0,5)},
  {name:'gzip partial trailer',coding:'gzip',wireBody:gzip.subarray(0,gzip.length-3)},
  {name:'gzip invalid',coding:'gzip',wireBody:Buffer.from('not a compressed response')},
  {name:'deflate',coding:'deflate',wireBody:deflate},
  {name:'deflate raw',coding:'deflate',wireBody:deflateRawSync(text)},
  {name:'deflate empty',coding:'deflate',wireBody:Buffer.alloc(0)},
  {name:'deflate invalid',coding:'deflate',wireBody:Buffer.from('invalid')},
  {name:'deflate trailing bytes',coding:'deflate',wireBody:Buffer.concat([deflate,Buffer.from('trailing')])},
  {name:'deflate raw partial',coding:'deflate',wireBody:deflateRawSync(text).subarray(0,-2)},
  {name:'deflate partial',coding:'deflate',wireBody:deflate.subarray(0,deflate.length-3)},
  {name:'brotli',coding:'br',wireBody:br},
  {name:'brotli empty',coding:'br',wireBody:Buffer.alloc(0)},
  {name:'brotli invalid',coding:'br',wireBody:Buffer.from([255,255,255,255])},
  {name:'brotli partial',coding:'br',wireBody:br.subarray(0,br.length-2)},
  {name:'zstd partial',coding:'zstd',wireBody:zstdCompressSync(text).subarray(0,-2)},
  {name:'zstd',coding:'zstd',wireBody:zstdCompressSync(text)},
  {name:'layered',coding:'br, gzip',wireBody:gzipSync(br)},
  {name:'unknown disables all decoding',coding:'unknown, gzip',wireBody:gzip},
  {name:'too many encodings',coding:'gzip,gzip,gzip,gzip,gzip,gzip',wireBody:gzip},
  {name:'no body status ignores codings',statusLine:'HTTP/1.1 204 No Content',coding:'gzip,gzip,gzip,gzip,gzip,gzip',wireBody:Buffer.alloc(0)},
  {name:'duplicate coding fields',headers:[['Content-Encoding','br'],['Content-Encoding','gzip']],wireBody:gzipSync(br)},
  {name:'custom reason',statusLine:'HTTP/1.1 400 Custom reason',wireBody:Buffer.from('invalid_grant')},
  {name:'empty reason',statusLine:'HTTP/1.1 200',wireBody:Buffer.from('ok')},
  {name:'body outlives active deadline',wireBody:Buffer.from(text),delay:150,timeout:50},
];
const key=await readFile(new URL('../tests/fixtures/tls/server-key.pem',import.meta.url));
const cert=await readFile(new URL('../tests/fixtures/tls/server-cert.pem',import.meta.url));
const cases=[];
for(const input of inputs) {
  let requestHeaders:any;
  const sockets=new Set<any>();
  const server=createServer({key,cert},socket=>{
    sockets.add(socket);socket.on('close',()=>sockets.delete(socket));socket.on('error',()=>{});
    let request='';let sent=false;
    socket.on('data',chunk=>{
      request+=chunk.toString();if(sent || !request.includes('\r\n\r\n'))return;sent=true;
      requestHeaders=Object.fromEntries(request.split('\r\n\r\n')[0].split('\r\n').slice(1).map(line=>{const index=line.indexOf(':');return [line.slice(0,index).toLowerCase(),line.slice(index+1).trim()];}));
      delete requestHeaders.host;
      const headers=[...(input.headers??[]),...(input.coding?[['Content-Encoding',input.coding]]:[]),['Content-Length',String(input.wireBody.length)]];
      socket.write((input.statusLine??'HTTP/1.1 200 OK')+'\r\n'+headers.map(([name,value]:string[])=>`${name}: ${value}`).join('\r\n')+'\r\n\r\n');
      if(input.delay)setTimeout(()=>socket.end(input.wireBody),input.delay);else socket.end(input.wireBody);
    });
  });
  await new Promise<void>(done=>server.listen(0,'127.0.0.1',done));
  const port=(server.address() as any).port;
  let result;
  try{
    const response=await fetchWithActiveTimeout(`https://localhost:${port}/token`,{method:'POST',headers:{'Content-Type':'application/x-www-form-urlencoded'},body:'grant_type=fixture'},{timeoutMs:input.timeout??1000});
    result={status:response.status,statusText:response.statusText,text:await response.text()};
  }catch(error:any){result={error:error.message};}
  for(const socket of sockets)socket.destroy();
  await new Promise<void>(done=>server.close(()=>done()));
  cases.push({...input,wireBody:Array.from(input.wireBody),result,requestHeaders});
}
const file='packages/core/src/fetch-timeout.ts';
await writeFile(process.env.RPI_REFERENCE_OUTPUT!,JSON.stringify({reference:pin,runtime:process.version,sourceHashes:{[file]:createHash('sha256').update(await readFile(join(root,file))).digest('hex'),undici:createHash('sha256').update((process as any).binding('natives')['internal/deps/undici/undici']).digest('hex')},cases},null,2)+'\n');
console.log(JSON.stringify({cases:cases.length}));
