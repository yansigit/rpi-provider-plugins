// Run with Node 24 against the unchanged pinned Pi-slot implementation.
import {mkdtemp,mkdir,writeFile,readFile,chmod,lstat,rm,symlink,rename} from 'node:fs/promises';
import {join,resolve,dirname} from 'node:path';
import {tmpdir} from 'node:os';
import {pathToFileURL} from 'node:url';
import {createHash} from 'node:crypto';
const root=resolve(process.env.RPI_REFERENCE_DIR!);
const {createPiSlot}=await import(pathToFileURL(join(root,'packages/core/src/common-auth-embedded/pi-slot/slot.js')).href);
const marker='antigravity-vault-custody';
const steps=['inspect','enter','inspect','enter','exit','inspect','exit'].map(op=>({op}));
const originals:any[]=[null,'{}',' { \n}\n','{"other":{"a":[1,{"nested":"},\\\""}]}}',
 '{\n "other" : {"type":"api_key","key":"unrelated"},\n "google" : { "type" : "oauth", "refresh":"fixture-secret", "expires": 1e3 }  \n}\n',
 '{"google":{"key":"first"}, "other":{"key":"second"}}',
 '{"a":{}, "google":{"key":"middle"}, "b":{}}',
 '{"google":{"\\ue000":1,"𐀀":2,"2":3,"10":4,"array":[-0,1e21,1e-7,9007199254740993],"nested":{"z":false,"a":null}}}',
 '[]','null','42','{"google":null}','{"google":[]}','{"google":"secret-do-not-echo"}',
 '{"google":{},"google":{}}','{"other":{},"other":{},"google":{}}','{"google":{},"\\u0067oogle":{}}',
 '{"google":{"type":"oauth","refresh":"secret-do-not-echo"}', '\ufeff{}',
 JSON.stringify({google:{type:'api_key',key:marker}}), JSON.stringify({google:{type:'api_key',key:marker,extra:true}})];
const specs:any[]=originals.map((raw,index)=>({name:`round trip ${index}`,raw,steps}));
for(const hook of ['stash-written','auth-written','stash-deleted','before-rename-auth','before-rename-stash']) {
 for(const raw of [null,'{ "other": {}, "google" : { "key": "fixture-secret" } }\n']) specs.push({name:`crash ${hook}`,raw,hook,steps:[...steps,{op:'enter'},{op:'exit'}]});
}
for(const raw of ['{"google":{"key":"new login"},"other":{"changed":true}}','{}',JSON.stringify({google:{type:'api_key',key:marker,extra:true}})]) specs.push({name:'foreign login refuses restore',raw:'{"google":{"key":"original"},"other":{}}',steps:[{op:'enter'},{op:'write',target:'auth',raw},{op:'inspect'},{op:'exit'},{op:'enter'}]});
for(const target of ['auth','stash']) {
 for(const mode of [0o600,0o640,0o644,0o400]) specs.push({name:`mode ${target} ${mode}`,raw:'{"google":{}}',steps:[{op:'enter'},{op:'chmod',target,mode},{op:'inspect'},{op:'exit'}]});
 for(const op of ['directory','symlink','remove','invalid-utf8']) specs.push({name:`unsafe ${target} ${op}`,raw:'{"google":{}}',steps:[{op:'enter'},{op,target},{op:'inspect'},{op:'exit'}]});
 for(const hook of [`replace-${target}`,`unlink-${target}`]) specs.push({name:hook,raw:'{"google":{}}',hook,hookAt:1,steps:[{op:'enter'},{op:'inspect'},{op:'exit'}]});
}
for(const [field,value] of [['version',2],['provider','other'],['authIdentity','wrong'],['placeholderIdentity','wrong'],['entry',null],['entry',[]],['entryJson',null],['entryJson','{"key":"foreign"}'],['sha256','wrong']]) specs.push({name:`invalid stash ${field}`,raw:'{"google":{"key":"fixture"}}',steps:[{op:'enter'},{op:'patch',field,value},{op:'inspect'},{op:'exit'}]});
for(const mode of [0o700,0o755,0o770]) specs.push({name:`stash parent ${mode}`,raw:'{}',steps:[{op:'chmod',target:'stashDir',mode},{op:'enter'},{op:'inspect'}]});
const directory=await mkdtemp(join(process.env.RPI_PRIVATE_TMP??tmpdir(),'vault-slot-'));
const cases:any[]=[];
try {
 for(const [index,spec] of specs.entries()) {
  const dir=join(directory,String(index));await mkdir(dir,{mode:0o700});
  const paths={auth:join(dir,'auth.json'),stash:join(dir,'vault','stash.json'),stashDir:join(dir,'vault')};
  await mkdir(paths.stashDir,{mode:0o700});
  if(spec.raw!==null)await writeFile(paths.auth,spec.raw,{mode:0o600});
  const hash=createHash('sha256').update(JSON.stringify(paths.auth)).digest('hex');
  let fired=false,at=0;
  const events:any[]=[];
  const step=(name:string)=>{events.push(name);if(!fired&&name===spec.hook){fired=true;throw Error('injected crash');}};
  const slot=createPiSlot({authPath:paths.auth,stashPath:paths.stash,provider:'google',placeholderKey:marker},step,{
   beforeRename:async(path:string)=>{const name=`before-rename-${path===paths.auth?'auth':'stash'}`;if(!fired&&name===spec.hook){fired=true;throw Error('injected crash');}},
   afterReadCheck:async(path:string)=>{const target=path===paths.auth?'auth':'stash';if(fired||at!==(spec.hookAt??0))return;if(spec.hook===`unlink-${target}`){fired=true;await rm(path);}else if(spec.hook===`replace-${target}`){fired=true;await rename(path,`${path}.old`);await writeFile(path,'{}',{mode:0o600});}}
  });
  const snapshot=async(path:string)=>{try{const stat=await lstat(path);if(stat.isDirectory())return {kind:'directory'};if(stat.isSymbolicLink())return {kind:'symlink'};const bytes=await readFile(path);const text=bytes.toString('utf8');return {kind:'file',mode:stat.mode&0o777,...(Buffer.from(text).equals(bytes)?{text:text.replaceAll(hash,'<auth-identity>')}:{bytes:[...bytes]})};}catch(e:any){if(e.code==='ENOENT')return null;throw e;}};
  const results=[];
  for(const [i,action] of spec.steps.entries()) {
   at=i;let outcome:any={};
   try {
    const path=paths[action.target??'auth'];
    if(action.op==='inspect')outcome={value:await slot.inspect()};
    else if(action.op==='enter'){await slot.enterVault();outcome={value:null};}
    else if(action.op==='exit')outcome={value:await slot.exitVault()};
    else if(action.op==='chmod')await chmod(path,action.mode);
    else if(action.op==='write')await writeFile(path,action.raw);
    else if(action.op==='patch'){const stash=JSON.parse(await readFile(paths.stash,'utf8'));stash[action.field]=action.value;await writeFile(paths.stash,JSON.stringify(stash));}
    else {await rm(path,{recursive:true,force:true});if(action.op==='directory')await mkdir(path,{mode:0o700});else if(action.op==='symlink'){await writeFile(`${path}.target`,'{}',{mode:0o600});await symlink(`${path}.target`,path);}else if(action.op==='invalid-utf8')await writeFile(path,Buffer.from([0xff]),{mode:0o600});}
   }catch(e:any){outcome={error:{name:e.name,message:e.message,code:e.code}};}
   results.push({outcome,events:[...events],auth:await snapshot(paths.auth),stash:await snapshot(paths.stash)});
  }
  cases.push({spec,results});
 }
 await writeFile(resolve(process.env.RPI_REFERENCE_OUTPUT!),JSON.stringify({runtime:process.version,platform:process.platform,cases},null,2)+'\n');
 console.log(`${cases.length} original credential-slot scenarios captured`);
}finally{await rm(directory,{recursive:true,force:true});}
