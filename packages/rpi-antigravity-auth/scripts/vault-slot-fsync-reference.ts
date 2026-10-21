import {mkdtemp,mkdir,writeFile,readFile,readdir,rm} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {tmpdir} from 'node:os';
import {pathToFileURL} from 'node:url';
import {createHash} from 'node:crypto';
const {createPiSlot}=await import(pathToFileURL(join(resolve(process.env.RPI_REFERENCE_DIR!),'packages/core/src/common-auth-embedded/pi-slot/slot.js')).href);
const cases=[];
for(const boundary of ['stash-file','stash-directory','auth-file','auth-directory','delete-directory']) {
 const directory=await mkdtemp(join(process.env.RPI_PRIVATE_TMP??tmpdir(),'vault-slot-fsync-'));
 try {
  const authDir=join(directory,'auth-dir'),vaultDir=join(directory,'vault');await mkdir(authDir,{mode:0o700});await mkdir(vaultDir,{mode:0o700});
  const auth=join(authDir,'auth.json'),stash=join(vaultDir,'stash.json');
  await writeFile(auth,'{ "google" : { "key": "fixture-original" }, "other": {} }\n',{mode:0o600});
  const hash=createHash('sha256').update(JSON.stringify(auth)).digest('hex');
  const events:string[]=[];
  const slot=createPiSlot({authPath:auth,stashPath:stash,provider:'google',placeholderKey:'antigravity-vault-custody'},(step:string)=>events.push(step));
  process.env.RPI_FSYNC_FAILURE_MATCH='__disabled__';
  if(boundary==='delete-directory')await slot.enterVault();
  process.env.RPI_FSYNC_FAILURE_MATCH=boundary==='stash-file'?'stash.json.':boundary==='auth-file'?'auth.json.':boundary==='auth-directory'?authDir:vaultDir;
  process.env.RPI_FSYNC_FAILURE_KIND=boundary.endsWith('directory')?'directory':'file';
  const bytes=async(path:string)=>{try{return (await readFile(path,'utf8')).replaceAll(hash,'<auth-identity>');}catch(e:any){if(e.code==='ENOENT')return null;throw e;}};
  const capture=async(fn:()=>Promise<any>)=>{let outcome;try{outcome={value:(await fn())??null};}catch(e:any){outcome={error:{name:e.name,code:e.code,message:e.message}};}return {outcome,events:[...events],auth:await bytes(auth),stash:await bytes(stash),authFiles:(await readdir(authDir)).sort(),vaultFiles:(await readdir(vaultDir)).sort()};};
  const failed=await capture(()=>boundary==='delete-directory'?slot.exitVault():slot.enterVault());
  process.env.RPI_FSYNC_FAILURE_MATCH='__disabled__';
  const recovered=await capture(async()=>{if(boundary!=='delete-directory')await slot.enterVault();return slot.exitVault();});
  cases.push({boundary,failed,recovered});
 }finally{await rm(directory,{recursive:true,force:true});}
}
await writeFile(resolve(process.env.RPI_REFERENCE_OUTPUT!),JSON.stringify({runtime:process.version,platform:process.platform,cases},null,2)+'\n');
console.log(`${cases.length} original credential-slot fsync failure boundaries captured`);
