import {writeFile,readFile,mkdtemp,readdir,rm} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {tmpdir} from 'node:os';
import {pathToFileURL} from 'node:url';
const root=resolve(process.env.RPI_REFERENCE_DIR!);
const {writeEnrollmentTokenFile}=await import(pathToFileURL(join(root,'node_modules/.bun/@cortexkit+claustrum-client@0.12.0/node_modules/@cortexkit/claustrum-client/dist/enrollment.js')).href);
const {ClaustrumEnrollmentManager}=await import(pathToFileURL(join(root,'packages/core/src/common-auth-embedded/claustrum/enrollment.js')).href);
const cases:any[]=[];
for(const kind of ['token','state']){
 const directory=await mkdtemp(join(process.env.RPI_PRIVATE_TMP??tmpdir(),'vault-fsync-'));let calls=0;
 try{
  const tokenPath=join(directory,kind==='token'?'sync-failure.json':'token.json'),statePath=join(directory,kind==='state'?'sync-failure.json':'state.json');
  const old={token:'a'.repeat(64),token_generation:1};await writeFile(tokenPath,JSON.stringify(old)+'\n',{mode:0o600});
  let result:any;
  try{
   if(kind==='token')await writeEnrollmentTokenFile(tokenPath,{token:'b'.repeat(64),token_generation:2});
   else{const manager=new ClaustrumEnrollmentManager({paths:{statePath,tokenPath},proposedName:'antigravity-auth-pi',now:()=>42000,mintSecret:()=>'a'.repeat(64),client:{async proposeEnrollment(){calls++;throw new Error('unexpected RPC')},async pollEnrollment(){calls++;throw new Error('unexpected RPC')}}});await manager.reconcile();}
   result={ok:true};
  }catch(e:any){result={error:{name:e.name,code:e.code,message:e.message}};}
  cases.push({kind,result,calls,token:JSON.parse(await readFile(tokenPath,'utf8')),entries:(await readdir(directory)).sort()});
 }finally{await rm(directory,{recursive:true,force:true});}
}
await writeFile(resolve(process.env.RPI_REFERENCE_OUTPUT!),JSON.stringify({runtime:process.version,platform:process.platform,cases},null,2)+'\n');
console.log(`${cases.length} original fsync failure cases captured`);
