import {mkdtemp,mkdir,writeFile,readFile,rm} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {tmpdir} from 'node:os';
import {pathToFileURL} from 'node:url';
import {createHash} from 'node:crypto';
const root=resolve(process.env.RPI_REFERENCE_DIR!);
const {registerCortexKitPiAntigravity}=await import(pathToFileURL(join(root,'packages/pi/src/index.ts')).href);
const claustrum=await import(pathToFileURL(join(root,'packages/core/src/common-auth-embedded/claustrum/index.js')).href);
const piSlot=await import(pathToFileURL(join(root,'packages/core/src/common-auth-embedded/pi-slot/slot.js')).href);
const proposedName='antigravity-auth-pi';
const approved={version:1,phase:'approved',proposedName,tokenGeneration:1,updatedAt:1000};
const cases=[];
const states=[undefined,approved,{...approved,proposedName:'other'},{version:1,phase:'pending',proposedName,requestSecret:'a'.repeat(64),createdAt:1,updatedAt:1}];
const tokens=[undefined,{token:'b'.repeat(64),token_generation:1},{token:'invalid',token_generation:1}];
const modes=[undefined,{version:1,mode:'local'},{version:1,mode:'custody',routeId:' chosen '},{version:1,mode:'invalid'}];
const auths=['{ "google": { "key": "fixture-original" }, "other": {} }\n','{}','{'];
const specs:any[]=[];
for(const state of states)for(const token of tokens)for(const mode of modes)for(const auth of auths)specs.push({state,token,mode,auth});
for(const [state,token,mode,afterSwap] of [
 [undefined,undefined,undefined,{state:approved}],
 [approved,tokens[1],{version:1,mode:'local'},{state:null,token:null}],
 [approved,tokens[1],{version:1,mode:'custody',routeId:'old'},{mode:{version:1,mode:'custody',routeId:'new'}}],
 [undefined,undefined,undefined,{mode:{version:1,mode:'custody'}}],
 [approved,tokens[1],undefined,{state:{invalid:true}}],
])specs.push({state,token,mode,auth:auths[0],afterSwap});
const directory=await mkdtemp(join(process.env.RPI_PRIVATE_TMP??tmpdir(),'vault-startup-'));
try {
 for(const spec of specs) {
  const {state,token,mode,auth,afterSwap}=spec;
  const dir=join(directory,String(cases.length));await mkdir(dir,{mode:0o700});const vaultDir=join(dir,'vault');await mkdir(vaultDir,{mode:0o700});
  const nativeAuthPath=join(dir,'auth.json'),nativeStashPath=join(vaultDir,'google-login-stash.json'),modeFile=join(vaultDir,'pi-mode.json');
  for(const [path,value] of [[join(vaultDir,'pi-enrollment-state.json'),state],[join(vaultDir,'pi-enrollment.json'),token],[modeFile,mode]])if(value!==undefined)await writeFile(path,JSON.stringify(value),{mode:0o600});
  await writeFile(nativeAuthPath,auth,{mode:0o600});
  let config:any;const commands:string[]=[];
  const loadPiSlot=async()=>({...piSlot,openPiSlot(options:any){const slot=piSlot.openPiSlot(options);return {...slot,...Object.fromEntries(['enterVault','exitVault'].map(op=>[op,async()=>{const result=await slot[op]();for(const [key,path] of [['state',join(vaultDir,'pi-enrollment-state.json')],['token',join(vaultDir,'pi-enrollment.json')],['mode',modeFile]])if(afterSwap&&key in afterSwap){await rm(path,{force:true});if(afterSwap[key]!==null)await writeFile(path,JSON.stringify(afterSwap[key]),{mode:0o600});}return result;}]))};}});
  await registerCortexKitPiAntigravity({registerProvider(id:string,value:any){if(id!=='google')throw Error('wrong provider');config=value;},registerCommand(name:string){commands.push(name);},on(){}},{vaultDir,modeFile,rosterFile:join(vaultDir,'pi-roster.json'),nativeAuthPath,nativeStashPath,readHostSlot(){throw Error('unexpected credential inspection');},loadClaustrum:async()=>claustrum,loadPiSlot,connect:async()=>{throw Error('unexpected connection');},connectEnrollment:async()=>{throw Error('unexpected enrollment');}});
  let refusal;
  if(config.name.endsWith('unavailable)')) {
   const model={...config.models[0],provider:'google',api:config.api,baseUrl:config.baseUrl};
   const result=await config.streamSimple(model,{messages:[]},{apiKey:config.apiKey}).result();
   refusal=result.errorMessage;
  }
  const hash=createHash('sha256').update(JSON.stringify(nativeAuthPath)).digest('hex');
  const read=async(path:string)=>{try{return(await readFile(path,'utf8')).replaceAll(hash,'<auth-identity>');}catch(e:any){if(e.code==='ENOENT')return null;throw e;}};
  cases.push({spec,configuration:{name:config.name,oauth:!!config.oauth,apiKey:config.apiKey??null,commands,refusal:refusal??null},auth:await read(nativeAuthPath),stash:await read(nativeStashPath)});
 }
 await writeFile(resolve(process.env.RPI_REFERENCE_OUTPUT!),JSON.stringify({runtime:process.version,cases},null,2)+'\n');console.log(`${cases.length} original factory startup scenarios captured`);
}finally{await rm(directory,{recursive:true,force:true});}
