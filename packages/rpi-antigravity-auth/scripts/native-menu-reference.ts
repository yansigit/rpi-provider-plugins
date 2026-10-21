// Actual pinned Pi factory/menu and Subc transport behind the same scripted UI as rpi.
import {readFileSync} from 'node:fs';
import {join, resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {createInterface} from 'node:readline';
const root=resolve(process.env.RPI_REFERENCE_DIR!);
const agent=resolve(process.env.RPI_CODING_AGENT_DIR!);
const load=(path:string)=>import(pathToFileURL(join(root,path)).href);
const {registerCortexKitPiAntigravity}=await load('packages/pi/src/index.ts');
const commands=await load('packages/core/src/common-auth-embedded/commands/index.js');
const handlers=new Map<string,any>();
const pending=new Map<string,(answer:any)=>void>();
let next=0;
const write=(value:any)=>process.stdout.write(JSON.stringify(value)+'\n');
const show=(ui:any)=>new Promise<any>(done=>{
 const requestId=String(++next);pending.set(requestId,done);write({type:'ui_dialog_request',requestId,ui:{nativeDialog:ui}});
});
const ui={
 select:async(title:string,labels:string[])=>{
  const result=await show({kind:'select',title,choices:labels.map((label,i)=>({label,value:String(i)}))});
  return result==null?undefined:labels[Number(result.values[0])];
 },
 confirm:async(title:string,description:string)=>(await show({kind:'confirm',title,description}))?.confirmed===true,
 notify:(text:string,kind='info')=>write({type:'ui_notification',text,kind}),
};
const vaultDir=join(agent,'antigravity-auth-vault');
await registerCortexKitPiAntigravity({registerProvider(){},registerCommand(name:string,options:any){handlers.set(name,options);},on(){}},{
 vaultDir,modeFile:join(vaultDir,'pi-mode.json'),rosterFile:join(vaultDir,'pi-roster.json'),nativeAuthPath:join(agent,'auth.json'),nativeStashPath:join(vaultDir,'google-login-stash.json'),
 readHostSlot(){try{return JSON.parse(readFileSync(join(agent,'auth.json'),'utf8').replace(/^\uFEFF/,''))?.google;}catch{return undefined;}},
 loadCommands:async()=>commands,
});
write({type:'ready'});
createInterface({input:process.stdin}).on('line',async line=>{
 const input=JSON.parse(line);
 if(input.type==='ui_dialog_response'){
  const done=pending.get(input.requestId);pending.delete(input.requestId);done?.(input.cancelled?undefined:input.answer);return;
 }
 try{
  if(input.type==='extension_command')await handlers.get(input.name).handler(input.args??'',{ui,sessionManager:{getSessionId:()=> 'reference-menu-session'}});
  else if(input.type!=='ping')throw Error('Unsupported fixture operation');
  write({type:'response',id:input.id,success:true,data:null});
 }catch(error){write({type:'response',id:input.id,success:false,error:String(error)});}
}).on('close',()=>process.exit(0));
