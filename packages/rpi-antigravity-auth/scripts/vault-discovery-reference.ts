import {mkdtemp,mkdir,writeFile,rm,symlink} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {tmpdir,userInfo} from 'node:os';
import {pathToFileURL} from 'node:url';
const root=resolve(process.env.RPI_REFERENCE_DIR!);
const modulePath=join(root,'node_modules/.bun/@cortexkit+claustrum-client@0.12.0/node_modules/@cortexkit/claustrum-client/dist/detect.js');
const {resolveClaustrumConnectionPath,userConnectionToken}=await import(pathToFileURL(modulePath).href);
const saved={...process.env},home=userInfo().homedir;
const dir=await mkdtemp(join(process.env.RPI_PRIVATE_TMP??tmpdir(),'vault-discovery-'));
const uid=process.getuid?.()??null,cases:any[]=[],tokens:any[]=[];
const expand=(s:string)=>s.replaceAll('$ROOT',dir),clean=(s:string)=>s.replaceAll(dir,'$ROOT').replaceAll(home,'$OS_HOME');
async function run(spec:any){
 await rm(join(dir,'files'),{recursive:true,force:true});await mkdir(join(dir,'files','tmp'),{recursive:true});
 for(const key of ['SUBC_CONNECTION_FILE','CLAUSTRUM_SUBC_CONNECTION','XDG_RUNTIME_DIR','HOME','TMPDIR','TMP','TEMP'])delete process.env[key];
 process.env.TMPDIR=join(dir,'files/tmp');
 for(const [key,value] of Object.entries(spec.env??{}))process.env[key]=expand(value as string);
 for(const file of spec.files??[]){const path=expand(file.path);await mkdir(resolve(path,'..'),{recursive:true});if(file.directory)await mkdir(path);else if(file.link)await symlink(expand(file.link),path);else await writeFile(path,'{}');}
 cases.push({spec,uid,result:clean(resolveClaustrumConnectionPath(spec.explicit))});
}
try{
 const runtime='$ROOT/files/run',homedir='$ROOT/files/home',temp='$ROOT/files/tmp';
 const files=[{path:runtime+'/subc-connection.json'},{path:homedir+'/.local/share/cortexkit/run/subc-connection.json'},{path:temp+`/subc-${uid}.connection.json`},{path:temp+'/subc-other.connection.json'}];
 for(let mask=0;mask<16;mask++)for(const selected of [{XDG_RUNTIME_DIR:runtime,HOME:homedir},{HOME:homedir},{}])await run({env:selected,files:files.filter((_,i)=>mask&(1<<i))});
 for(const key of ['SUBC_CONNECTION_FILE','CLAUSTRUM_SUBC_CONNECTION'])for(const value of ['', ' ', ' relative ', '$ROOT/files/absent','\ufeff $ROOT/files/absent \n'])await run({env:{HOME:homedir,[key]:value},files:[files[3]]});
 for(const explicit of ['', ' ', ' relative ', '\ufeff path \n'])await run({explicit,env:{SUBC_CONNECTION_FILE:' pinned ',CLAUSTRUM_SUBC_CONNECTION:'ignored',HOME:homedir},files:[]});
 for(const spec of [
  {env:{XDG_RUNTIME_DIR:runtime,HOME:homedir},files:[{path:runtime+'/subc-connection.json',directory:true},files[1]]},
  {env:{XDG_RUNTIME_DIR:runtime,HOME:homedir},files:[{path:runtime+'/subc-connection.json',link:homedir+'/absent'},files[3]]},
  {env:{HOME:homedir},files:[{path:temp+'/subc-one.connection.json',directory:true},files[3]]},
  {env:{HOME:homedir},files:[{path:temp+'/not-subc-x.connection.json'},files[3]]},
  {env:{HOME:homedir},files:[{path:temp+'/subc-one.connection.json'},{path:temp+'/subc-two.connection.json'}]},
  {env:{HOME:homedir+'/../home/./'},files:[]},
 ])await run(spec);
 for(const uid of [null,0,1000])for(const env of [{},{USER:'a b/é😀'},{USER:'',USERNAME:'user.name'},{HOME:'/path/to/home'},{USERPROFILE:'C:\\Users\\Tester'}, {USER:'first',USERNAME:'second'},{USER:'-_.AZ09'}])tokens.push({uid,env,result:userConnectionToken(uid===null?null:()=>uid,env)});
 await writeFile(resolve(saved.RPI_REFERENCE_OUTPUT!),JSON.stringify({platform:process.platform,cases,tokens})+'\n');
 console.log(`${cases.length} original connection discoveries and ${tokens.length} user tokens`);
}finally{process.env=saved;await rm(dir,{recursive:true,force:true});}
