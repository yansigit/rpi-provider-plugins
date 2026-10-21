import {writeFile,mkdtemp,chmod,mkdir,symlink,rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
const root=resolve(process.env.RPI_REFERENCE_DIR!);
const {checkPosixOwnerOnlyPath}=await import(pathToFileURL(join(root,'node_modules/.bun/@cortexkit+private-files@0.2.1/node_modules/@cortexkit/private-files/dist/index.js')).href);
const {readClaustrumEnrollmentToken}=await import(pathToFileURL(join(root,'packages/core/src/common-auth-embedded/claustrum/enrollment.js')).href);
const passwd='fixture:x:1000:1000::/home/fixture:/bin/sh\n',group='fixture:x:1000:\n';
const cases:any[]=[],files:any[]=[];
const meta=(mode=0o700,uid=1000,gid=1000,directory=true)=>({mode,uid,gid,directory});
function capture(name:string,chain:any[],accountFiles:any={passwd:[...Buffer.from(passwd)],group:[...Buffer.from(group)]}){
 const paths=['/fixture/child','/fixture','/'];
 const lookup=(path:string,which:string)=>{const value=chain[paths.indexOf(path)][which];return {...value,isDirectory:()=>value.directory}};
 const result=checkPosixOwnerOnlyPath('/fixture/child',{euid:1000,egid:1000,directoryOnly:true,fs:{realpathSync:(path:string)=>path,lstatSync:(path:string)=>lookup(path,'lstat'),statSync:(path:string)=>lookup(path,'stat'),readFileSync:(path:string)=>Uint8Array.from(accountFiles[path==='/etc/passwd'?'passwd':'group'])}});
 cases.push({name,chain,accountFiles,result});
}
const pair=(value:any)=>({lstat:value,stat:value});
for(const mode of [0,0o700,0o755,0o750,0o770,0o777,0o1777,0o1770,0o1700])for(const uid of [0,1000,2000])for(const gid of [1000,2000])for(const directory of [true,false])for(const childMode of [0o700,0o770,0o777])capture('ancestor',[pair(meta(childMode)),pair(meta(mode,uid,gid,directory)),pair(meta(0o755,0,0))]);
const fields:any[]=[['passwd',''],['group',''],['passwd',passwd+passwd],['group',group+group],['passwd',passwd+'other:x:2000:1000::/:/bin/sh\n'],['group','fixture:x:1000:other\n'],['group','fixture:x:1000:fixture,fixture\n'],['group','fixture:x:1000:fixture,\n'],['passwd','\ufeff'+passwd],['passwd','  # comment\n'+passwd],['passwd','  bad line\n'+passwd],['passwd',passwd.replace('1000:1000','4294967296:1000')],['passwd',passwd.replace('1000:1000','1000:4294967296')],['group',group.replace('1000','4294967296')],['passwd',passwd.replace('fixture','fixture,one')],['group',group.replace('fixture','fixture,one')]];
for(const [field,text] of fields){const accounts={passwd:[...Buffer.from(passwd)],group:[...Buffer.from(group)]};accounts[field]=[...Buffer.from(text)];capture('accounts',[pair(meta()),pair(meta(0o770)),pair(meta(0o755,0,0))],accounts);}
capture('fatal account UTF-8',[pair(meta()),pair(meta(0o770)),pair(meta(0o755,0,0))],{passwd:[255,...Buffer.from(passwd)],group:[...Buffer.from(group)]});
const directory=await mkdtemp(join(process.env.RPI_PRIVATE_TMP??tmpdir(),'vault-private-'));
const path=join(directory,'token.json'),target=join(directory,'target.json');
const valid={token:'a'.repeat(64),token_generation:1};
const specs=[{name:'missing',missing:true},{name:'valid',value:valid,mode:0o600},{name:'group-readable',value:valid,mode:0o640},{name:'public',value:valid,mode:0o644},{name:'bad JSON',raw:'{"token":"secret-do-not-echo"',mode:0o600},{name:'too large',raw:' '.repeat(16385),mode:0o600},{name:'exact limit',raw:JSON.stringify(valid).padEnd(16384,' '),mode:0o600},{name:'directory',directory:true},{name:'symlink',symlink:true}];
try {
 for(const spec of specs){
  await rm(path,{recursive:true,force:true});
  if(spec.directory)await mkdir(path,{mode:0o700});
  else if(spec.symlink){await writeFile(target,JSON.stringify(valid),{mode:0o600});await symlink(target,path);}
  else if(!spec.missing){await writeFile(path,spec.raw??JSON.stringify(spec.value),{mode:0o600});await chmod(path,spec.mode);}
  let result;try{result={value:await readClaustrumEnrollmentToken(path)};}catch(e:any){result={error:{name:e.name,kind:e.kind,message:e.message}};}
  files.push({spec,result});
 }
 await writeFile(resolve(process.env.RPI_REFERENCE_OUTPUT!),JSON.stringify({runtime:process.version,platform:process.platform,cases,files},null,2)+'\n');
 console.log(`${cases.length} original POSIX parent-policy and ${files.length} secret-file cases captured`);
} finally {await rm(directory,{recursive:true,force:true});}
