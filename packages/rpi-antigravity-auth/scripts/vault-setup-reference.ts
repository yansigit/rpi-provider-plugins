// Execute the original Pi setup against real isolated files and scripted enrollment calls.
import {mkdtemp,mkdir,writeFile,readFile,rm} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
const root=resolve(process.env.RPI_REFERENCE_DIR!);
const {PiVaultSetup}=await import(pathToFileURL(join(root,'packages/pi/src/vault-setup.ts')).href);
const upstream=await import(pathToFileURL(join(root,'packages/core/src/common-auth-embedded/claustrum/index.js')).href);
const name='antigravity-auth-pi',secret='b'.repeat(64),token={token:'a'.repeat(64),token_generation:1};
class Manager extends upstream.ClaustrumEnrollmentManager { constructor(options:any){super({...options,mintSecret:()=>secret});} }
const claustrum={...upstream,ClaustrumEnrollmentManager:Manager};
const scratch=await mkdtemp(join(process.env.RPI_PRIVATE_TMP!,'setup-reference-'));
const cases:any[]=[],slots:any[]=[];
const pending={version:1,phase:'pending',proposedName:name,requestSecret:secret,requestId:'request-1',createdAt:1000,updatedAt:1000};
const approved={version:1,phase:'approved',proposedName:name,tokenGeneration:1,updatedAt:1000};
const states=[undefined,pending,approved,{version:1,phase:'denied',proposedName:name,updatedAt:1000},{version:1,phase:'blocked',proposedName:name,errorCode:'missing_token',updatedAt:1000},{...approved,proposedName:'other'},{}];
async function capture(spec:any){
 const dir=join(scratch,String(cases.length));await mkdir(dir,{mode:0o700});
 const mode=join(dir,'pi-mode.json'),state=join(dir,'pi-enrollment-state.json'),credential=join(dir,'pi-enrollment.json');
 for(const [path,value] of [[mode,spec.mode],[state,spec.state],[credential,spec.token]]) if(value!==undefined)await writeFile(path,JSON.stringify(value),{mode:0o600});
 if(spec.modeDirectory)await mkdir(mode);
 const read=async(path:string)=>{try{return JSON.parse(await readFile(path,'utf8'));}catch(e:any){return e.code==='ENOENT'?null:{error:e.code};}};
 const snapshot=async()=>({mode:await read(mode),state:await read(state),token:await read(credential)});
 const calls:any[]=[];
 const setup=new PiVaultSetup({claustrum,stateDir:dir,modeFile:mode,now:()=>42000,
  readHostSlot:()=>{calls.push(['hostSlot']);return spec.hostSlot;},
  connectEnrollment:async()=>{calls.push(['connect']);if(spec.connectError)throw Error('connect failed');return {
   enrollPropose:async(input:any)=>{calls.push(['propose',input,await snapshot()]);return {requestId:'request-1'};},
   enrollPoll:async(input:any)=>{calls.push(['poll',input,await snapshot()]);return spec.reply??{status:'pending'};},
   close:()=>calls.push(['close'])};},
  onModeChanged:async(record:any)=>{calls.push(['changed',record,await snapshot()]);if(spec.changeError)throw Error('change failed');}
 });
 let result;try{let value;if(spec.action==='useVault')value=await setup.useVault(spec.route);else value=await (setup as any)[spec.action]();result={value:value??null};}catch(e:any){result={error:{name:e.name,message:spec.modeDirectory?'<mode-write-error>':e.message}};}
 cases.push({spec,result,calls,after:await snapshot()});
}
try{
 for(const state of states)for(const credential of [undefined,token])for(const action of ['status','useVault','useLocal','disconnect','connectStep'])await capture({state,token:credential,action,mode:{version:1,mode:'custody',routeId:'chosen'},hostSlot:{type:'oauth',refresh:'refresh'}});
 for(const route of [undefined,'',' ','\ufeff','chosen',' chosen '])await capture({action:'useVault',token,route});
 for(const mode of [undefined,{version:1,mode:'local',routeId:'old'},{version:1,mode:'custody',routeId:'chosen'},{}])for(const reply of [{status:'pending'},{status:'denied'},{status:'approved',name,token:token.token,tokenGeneration:1}])await capture({action:'connectStep',mode,reply});
 for(const action of ['useVault','useLocal','disconnect','connectStep'])await capture({action,token:action==='useLocal'?undefined:token,changeError:true});
 await capture({action:'connectStep',connectError:true});
 for(const action of ['useVault','useLocal','disconnect'])await capture({action,token:action==='useLocal'?undefined:token,modeDirectory:true});
 for(const hostSlot of [null,{},[],{type:'api_key',key:'key'},{type:'api',key:''},{type:'api',key:'key'},...['','refresh','claustrum-tombstone:v1:google','claustrum-tombstone:v1:other'].map(refresh=>({type:'oauth',refresh}))]){slots.push({hostSlot,result:upstream.classifyHostSlot(hostSlot,'google')});await capture({action:'status',hostSlot});}
 await writeFile(resolve(process.env.RPI_REFERENCE_OUTPUT!),JSON.stringify({cases,slots})+'\n');console.log(`${cases.length} Pi setup transitions, ${slots.length} host-slot cases`);
}finally{await rm(scratch,{recursive:true,force:true});}
