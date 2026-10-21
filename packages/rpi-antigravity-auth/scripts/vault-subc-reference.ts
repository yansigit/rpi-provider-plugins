// Exercise the original consumer over an authenticated, framed TCP peer.
import assert from 'node:assert/strict';
import net from 'node:net';
import {mkdtemp, readFile, writeFile, rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
const root=resolve(process.env.RPI_REFERENCE_DIR!);
const modules=join(root,'packages/core/node_modules/@cortexkit');
const {ClaustrumClient}=await import(pathToFileURL(join(modules,'claustrum-client/dist/index.js')).href);
const {SubcSocket,computeProof,SERVER_PROOF_DOMAIN,CLIENT_AUTH_DOMAIN,FrameType,Priority,buildFrame,buildFlags,encodeFrame,PROTOCOL_VERSION}=await import(pathToFileURL(join(modules,'subc-client/dist/index.js')).href);
const key=Array(32).fill(7),daemonId=Array(16).fill(9),serverNonce=Array(32).fill(11);
const identity={project_root:'/fixture/project',harness:'opencode',session:'store-fixture'};
const good=Buffer.from(JSON.stringify({result:{view:'view',credentials:[]}}));
const cases:any[]=[];
const directory=await mkdtemp(join(process.env.RPI_PRIVATE_TMP??tmpdir(),'vault-subc-'));
try {
 for(const spec of [
  {name:'json',body:[...good]},
  {name:'binary',body:[...good],binary:true},
  {name:'invalid JSON',body:[123]},
  {name:'invalid UTF-8 in JSON string',body:[...Buffer.from('{"result":{"view":"'),255,...Buffer.from('","credentials":[]}}')]},
  {name:'stream end',body:[],type:FrameType.StreamEnd},
  {name:'route closes after send',body:[],type:FrameType.Goodbye},
  ...['missing_identity','invalid_control_body','unknown_module','module_warming','module_reloading','unexpected_failure'].map(code=>({name:'error:'+code,type:FrameType.Error,body:[...Buffer.from(JSON.stringify({code,message:'private diagnostic'}))]})),
 ]) {
  const connections:any[]=[],jobs:Promise<void>[]=[],sockets:any[]=[];
  const server=net.createServer(socket=>{
   socket.setNoDelay(true);sockets.push(socket);
   const frames:any[]=[];connections.push(frames);
   jobs.push((async()=>{
    const sock=new SubcSocket(socket),deadline=Date.now()+10000;
    const readMessage=async()=>{const prefix=Buffer.from(await sock.readExact(4,deadline));const len=prefix.readUInt32LE();assert(len<4096);return JSON.parse(Buffer.from(await sock.readExact(len,deadline)).toString('utf8'));};
    const writeMessage=async(value:any)=>{const body=Buffer.from(JSON.stringify(value)),prefix=Buffer.alloc(4);prefix.writeUInt32LE(body.length);await sock.write(Buffer.concat([prefix,body]),deadline);};
    const hello=await readMessage();assert.equal(hello.role,'client');assert.equal(hello.client_nonce.length,32);
    await writeMessage({daemon_id:daemonId,server_nonce:serverNonce,daemon_ver:'fixture',server_proof:[...computeProof(key,SERVER_PROOF_DOMAIN,hello.client_nonce,serverNonce,daemonId)]});
    const auth=await readMessage();assert.deepEqual(auth.client_auth,[...computeProof(key,CLIENT_AUTH_DOMAIN,hello.client_nonce,serverNonce,daemonId)]);
    for(;;){
     let request;try{request=await sock.readFrame(Date.now()+10000,{afterHeaderMs:10000});}catch(e:any){if(e.constructor.name==='SocketClosedError')break;throw e;}
     const h=request.header,body=Buffer.from(request.body);
     frames.push({type:h.ty,flags:h.flags,channel:h.channel,epoch:h.epoch,corr:h.corr.toString(),body:body.length?JSON.parse(body.toString('utf8')):null});
     if(h.ty===FrameType.Goodbye)break;
     assert.equal(h.ty,FrameType.Request);
     const response=h.channel===0?Buffer.from(JSON.stringify({op:'route.open',route_channel:17,route_epoch:3})):Buffer.from(spec.body);
     const type=h.channel===0?FrameType.Response:(spec.type??FrameType.Response);
     await sock.write(encodeFrame(buildFrame(type,buildFlags(h.channel!==0&&Boolean(spec.binary),Priority.Interactive,false),h.channel,h.epoch,type===FrameType.Goodbye?0n:h.corr,response)),Date.now()+10000);
    }
    sock.close();
   })());
  });
  await new Promise<void>(resolve=>server.listen(0,'127.0.0.1',resolve));
  const connectionFile=join(directory,'connection.json');
  await writeFile(connectionFile,JSON.stringify({schema:1,wire_version:PROTOCOL_VERSION,endpoints:[{host:'127.0.0.1',port:(server.address() as net.AddressInfo).port}],key,daemon_id:daemonId,pid:process.pid,daemon_ver:'fixture'}),{mode:0o600});
  const client=await ClaustrumClient.connect({connectionFile,identity,handshakeTimeoutMs:10000,logger:()=>{}});
  let result;try{result={value:await client.listScoped('fixture-token')};}catch(e:any){result={error:{code:e.code,class:e.class,action:e.action,message:e.message}};}
  client.close();await Promise.all(jobs);await new Promise<void>(resolve=>server.close(()=>resolve()));
  for(const socket of sockets)socket.destroy();
  cases.push({spec,result,connections});
 }
 await writeFile(resolve(process.env.RPI_REFERENCE_OUTPUT!),JSON.stringify({runtime:process.version,client:JSON.parse(await readFile(join(modules,'subc-client/package.json'),'utf8')).version,normalization:'Random handshake nonces are authenticated, not compared. TCP ports are assigned by the OS. Frames are grouped by connection; per-connection order, IDs and payloads are retained.',cases},null,2)+'\n');
 console.log(`${cases.length} original authenticated Subc cases captured`);
} finally { await rm(directory,{recursive:true,force:true}); }
