import {mkdtemp,mkdir,writeFile,rm} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
const root=resolve(process.env.RPI_REFERENCE_DIR!),host=resolve(process.env.RPI_REFERENCE_HOST!);
const load=async(path:string)=>import(pathToFileURL(path).href);
const {registerCortexKitPiAntigravity}=await load(join(root,'packages/pi/src/index.ts'));
const claustrum=await load(join(root,'packages/core/src/common-auth-embedded/claustrum/index.js'));
const {composeModelProvider}=await load(join(host,'node_modules/@earendil-works/pi-coding-agent/dist/core/provider-composer.js'));
const {resolveProviderAuth}=await load(join(host,'node_modules/@earendil-works/pi-ai/dist/auth/resolve.js'));
const {googleProvider}=await load(join(host,'node_modules/@earendil-works/pi-ai/dist/providers/google.js'));
const dir=await mkdtemp('/home/user/provider-test-tmp/vault-host-auth-');
const cases:any[]=[];
try {
 for(const mode of ['local','custody','refusing']){
  const vaultDir=join(dir,mode);await mkdir(vaultDir,{mode:0o700});
  const nativeAuthPath=join(vaultDir,'auth.json');await writeFile(nativeAuthPath,mode==='refusing'?'{':'{}',{mode:0o600});
  if(mode==='custody')await writeFile(join(vaultDir,'pi-enrollment-state.json'),JSON.stringify({version:1,phase:'approved',proposedName:'antigravity-auth-pi',tokenGeneration:1,updatedAt:1000}),{mode:0o600});
  let extension:any;
  await registerCortexKitPiAntigravity({registerProvider(_:string,v:any){extension=v;},registerCommand(){},on(){}},{vaultDir,modeFile:join(vaultDir,'pi-mode.json'),rosterFile:join(vaultDir,'pi-roster.json'),nativeAuthPath,nativeStashPath:join(vaultDir,'stash.json'),readHostSlot(){throw Error('unexpected slot read');},loadClaustrum:async()=>claustrum,connect:async()=>{throw Error('unexpected vault connection');},connectEnrollment:async()=>{throw Error('unexpected enrollment');}});
  const provider=composeModelProvider('google',googleProvider(),{getProvider:()=>undefined},extension);
  for(const credential of [null,{type:'api_key',key:'antigravity-vault-custody'},{type:'api_key',key:'changed-native-key'},{type:'api_key',key:''},{type:'oauth',access:'changed-access',refresh:'changed-refresh',expires:4102444800000}])
   for(const override of [{},{apiKey:'explicit-key'},{apiKey:''},{apiKey:'antigravity-vault-custody'}]){
    const calls:any[]=[];let result;
    try {const value=await resolveProviderAuth(provider,{read:async(id:string)=>{calls.push(['read',id]);return credential??undefined;}},{env:async(name:string)=>{calls.push(['env',name]);return name==='GEMINI_API_KEY'?'ambient-key':undefined;}},override);result={value:value??null};}
    catch(e:any){result={error:{name:e.name,message:e.message}};}
    cases.push({mode,methods:Object.entries(provider.auth).filter(([,v])=>v).map(([k])=>k),credential,override,result,calls});
   }
 }
 await writeFile(resolve(process.env.RPI_REFERENCE_OUTPUT!),JSON.stringify({host:'Pi 1.3.0',cases},null,2)+'\n');
 console.log(`${cases.length} integrated Pi auth-resolution cases captured`);
}finally{await rm(dir,{recursive:true,force:true});}
