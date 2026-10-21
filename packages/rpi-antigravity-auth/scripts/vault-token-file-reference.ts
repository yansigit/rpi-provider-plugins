import {writeFile,readFile,mkdtemp,mkdir,chmod,stat,readdir,rm,symlink} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {tmpdir} from 'node:os';
import {pathToFileURL} from 'node:url';
const root=resolve(process.env.RPI_REFERENCE_DIR!);
const {writeEnrollmentTokenFile}=await import(pathToFileURL(join(root,'node_modules/.bun/@cortexkit+claustrum-client@0.12.0/node_modules/@cortexkit/claustrum-client/dist/enrollment.js')).href);
const valid={token:'b'.repeat(64),token_generation:2};
const specs:any[]=[];
for(const token of [undefined,null,'','a'.repeat(63),'a'.repeat(64),'A'.repeat(64),['a'.repeat(64)],42])for(const generation of [undefined,null,0,1,1.5,'1',9007199254740991,9007199254740992])specs.push({name:'validation',value:{token,token_generation:generation},parentMode:0o700});
for(const parentMode of [0o700,0o755,0o770,0o777,0o1777])specs.push({name:'parent-mode',value:valid,parentMode});
for(const target of ['missing-parents','directory','symlink','dangling-parent','file-parent'])specs.push({name:target,value:valid,target,parentMode:0o700});
const cases:any[]=[];
for(const spec of specs){
 const directory=await mkdtemp(join(process.env.RPI_PRIVATE_TMP??tmpdir(),'vault-token-file-'));
 try{
  const parent=join(directory,'parent');await mkdir(parent,{mode:spec.parentMode});await chmod(parent,spec.parentMode);
  let path=join(parent,'token.json');
  if(spec.target==='missing-parents')path=join(parent,'nested/deep/token.json');
  if(spec.target==='directory')await mkdir(path,{mode:0o700});
  if(spec.target==='symlink'){await writeFile(join(directory,'untouched'),'original');await symlink(join(directory,'untouched'),path);}
  if(spec.target==='dangling-parent'){await symlink(join(directory,'absent'),join(parent,'link'));path=join(parent,'link/token.json');}
  if(spec.target==='file-parent'){await writeFile(join(parent,'file'),'original');path=join(parent,'file/token.json');}
  let result:any;try{await writeEnrollmentTokenFile(path,spec.value);result={ok:true,bytes:await readFile(path,'utf8'),mode:(await stat(path)).mode&0o777};}
  catch(e:any){result={error:{name:e.name,code:e.code,message:e.message.replaceAll(directory,'<DIR>').replace(/\.token\.json\.\d+\.[a-f0-9]{24}\.tmp/g,'.token.json.<PID>.<RANDOM>.tmp')}};}
  if(spec.target==='symlink')result.untouched=await readFile(join(directory,'untouched'),'utf8');
  result.parentMode=(await stat(parent)).mode&0o777;
  result.entries=(await readdir(parent)).sort();
  cases.push({spec,result});
 }finally{await rm(directory,{recursive:true,force:true});}
}
await writeFile(resolve(process.env.RPI_REFERENCE_OUTPUT!),JSON.stringify({runtime:process.version,platform:process.platform,cases},null,2)+'\n');
console.log(`${cases.length} original enrollment token-file cases captured`);
