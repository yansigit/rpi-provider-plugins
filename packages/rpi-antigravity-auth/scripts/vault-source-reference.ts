import {mkdtemp,mkdir,writeFile,chmod,rm} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {tmpdir} from 'node:os';
import {pathToFileURL} from 'node:url';
const root=resolve(process.env.RPI_REFERENCE_DIR!);
const {PiVaultSetup,readPiVaultMode}=await import(pathToFileURL(join(root,'packages/pi/src/vault-setup.ts')).href);
const {createPiVaultRuntime}=await import(pathToFileURL(join(root,'packages/pi/src/vault-source.ts')).href);
const claustrum=await import(pathToFileURL(join(root,'packages/core/src/common-auth-embedded/claustrum/index.js')).href);
const proposedName='antigravity-auth-pi';
const approved={version:1,phase:'approved',proposedName,tokenGeneration:1,updatedAt:1000};
const pending={version:1,phase:'pending',proposedName,requestSecret:'a'.repeat(64),createdAt:1000,updatedAt:1000};
const states=[{}, {value:approved},{value:pending},{value:{...approved,proposedName:'other'}},{value:{version:1,phase:'denied',proposedName,updatedAt:1000}}, {raw:'{'}, {value:null}, {value:approved,mode:0o644}, {directory:true}];
const tokens=[{}, {value:{token:'b'.repeat(64),token_generation:1}},{value:{token:'invalid',token_generation:1}}, {raw:'{'},{directory:true}];
const modes:any[]=[{}, {value:{version:1,mode:'local'}},{value:{version:1,mode:'custody',routeId:' route '}},{raw:'{'},{directory:true},{value:{version:1,mode:'custody',unknown:'refuse'}}];
for(const routeId of [null,42,false,'',' ','\ufeff','x','  x  '])modes.push({value:{version:1,mode:'local',routeId}});
for(const value of [null,[],false,1,{}, {version:1.0,mode:'local'},{version:2,mode:'local'},{version:1,mode:'unknown'}])modes.push({value});
const dir=await mkdtemp(join(process.env.RPI_PRIVATE_TMP??tmpdir(),'vault-source-'));
const modeFile=join(dir,'pi-mode.json'),tokenFile=join(dir,'pi-enrollment.json'),stateFile=join(dir,'pi-enrollment-state.json');
const connect=async()=>{throw Error('unexpected network');};const readHostSlot=()=>{throw Error('source inspection read credentials');};
const setup=new PiVaultSetup({claustrum,stateDir:dir,modeFile,readHostSlot,connectEnrollment:connect});
const runtime=createPiVaultRuntime(readHostSlot,{vaultDir:dir,modeFile,rosterFile:join(dir,'pi-roster.json'),loadClaustrum:async()=>claustrum,connect,connectEnrollment:connect});
async function write(path:string,spec:any){await rm(path,{recursive:true,force:true});if(spec.directory)await mkdir(path,{mode:0o700});else if('raw'in spec||'value'in spec){await writeFile(path,spec.raw??JSON.stringify(spec.value),{mode:0o600});if(spec.mode)await chmod(path,spec.mode);}}
const cases:any[]=[];
try {
 for(const [mi,mode] of modes.entries())for(const state of (mi<6?states:[{}]))for(const token of (mi<6?tokens:[{}])) {
  await write(modeFile,mode);await write(stateFile,state);await write(tokenFile,token);
  let requestMode;try{requestMode={value:await runtime.requestPath.readMode()};}catch(e:any){requestMode={error:e.message};}
  cases.push({spec:{mode,state,token},modeRead:readPiVaultMode(modeFile),status:await setup.sourceStatus(),requestMode});
 }
 await writeFile(resolve(process.env.RPI_REFERENCE_OUTPUT!),JSON.stringify({runtime:process.version,platform:process.platform,cases},null,2)+'\n');
 console.log(`${cases.length} original source admission and settings scenarios captured`);
}finally{await rm(dir,{recursive:true,force:true});}
