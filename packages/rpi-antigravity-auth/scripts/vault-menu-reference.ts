import crypto from 'node:crypto';
import {syncBuiltinESMExports} from 'node:module';
import {writeFile} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
const originalRandom=crypto.randomBytes;
(crypto as any).randomBytes=(size:number)=>size===16?Buffer.alloc(16,7):originalRandom(size);syncBuiltinESMExports();
const root=resolve(process.env.RPI_REFERENCE_DIR!);
const load=(path:string)=>import(pathToFileURL(join(root,path)).href);
const {createPiVaultSection,PiVaultSetupError}=await load('packages/pi/src/vault-setup.ts');
const {createPiAntigravityAccountSections}=await load('packages/pi/src/commands.ts');
const {createAntigravityCommandMenu}=await load('packages/core/src/antigravity-command-menu.ts');
const commands=await load('packages/core/src/common-auth-embedded/commands/index.js');
const {sharedVaultTokenFolderOutcome}=await load('packages/core/src/vault-folder-refusal.ts');
const logger={warn:()=>{},debug:()=>{},info:()=>{},error:()=>{}};
const cases:any[]=[];
const refused="Choose a vault account to use.";
async function capture(spec:any,request?:any,answers?:any[]){
 const calls:any[]=[],notifications:any[]=[];let connections=0;
 const setup={status:async()=>{calls.push(['status']);if(spec.statusError)throw Error(spec.statusError);return structuredClone(spec.status);},
  useVault:async(route?:string)=>perform('useVault',route??null),useLocal:async()=>perform('useLocal'),disconnect:async()=>perform('disconnect'),
  connectStep:async()=>{if(connections++){calls.push(['watch']);return new Promise(()=>{});}await perform('connectStep');return spec.step??{state:'approved'};},
 };
 async function perform(op:string,...args:any[]){calls.push([op,...args]);if(spec.error){if(spec.error==='refused')throw new PiVaultSetupError(refused);throw Error(spec.error);} }
 const port={list:async()=>{calls.push(['list']);if(spec.listError)throw Error(spec.listError);return structuredClone(spec.rows??[]);},grantReach:()=>spec.reach,accept:async(route:string)=>perform('accept',route),decline:async(route:string)=>perform('decline',route)};
 const accounts=()=>spec.missingPort?undefined:port;
 const menu=createAntigravityCommandMenu({commands,source:'sections',sections:createPiAntigravityAccountSections({setup:async()=>setup,accounts}),extras:[createPiVaultSection({setup:async()=>setup,accounts})],logger});
 const invocation={sessionId:'session-fixture',notify:(...args:any[])=>notifications.push(args)};
 const open=await menu.open(invocation);let result=open;
 if(request){calls.length=0;const r=structuredClone(request);if(typeof r.itemIndex==='number'){r.itemId=open.menu.sections.find((v:any)=>v.id==='vault').items[r.itemIndex].id;delete r.itemIndex;}result=await menu.apply(r,invocation);request=r;}
 const uiTrace:any[]=[];
 if(answers){calls.length=0;const pending=[...answers];await commands.runPiCommandMenu(menu,{
 select:async(title:string,labels:string[])=>{uiTrace.push(['select',title,labels]);const selected=pending.shift();return selected==null?undefined:labels[selected];},
 confirm:async(title:string,message:string)=>{uiTrace.push(['confirm',title,message]);return pending.shift()===true;},
 notify:(text:string,kind?:string)=>{uiTrace.push(['notify',text,kind??'info']);},
 },{sessionId:'session-fixture'});result=null;if(pending.length)throw Error('unconsumed UI answers');}
 cases.push({spec,request,answers,result,calls,notifications,...(answers?{uiTrace}:{})});
}
const enrollments=[{state:'idle'},{state:'pending'},{state:'pending',requestId:'request-1'},{state:'pending',retryCode:'transport_error'},{state:'approved'},{state:'denied'},...['pending_exists','missing_token','unreadable-state','unknown'].map(code=>({state:'blocked',code})),{state:'unavailable',code:'transport_error'},{state:'busy'}];
const one={routeId:'route/private:owner@example.test',label:'private-label',email:'owner@example.test',declined:false},two={...one,routeId:'another-private-route',label:'other-private-label',email:'other@example.test'};
for(const mode of ['local','custody','unreadable'])for(const enrollment of enrollments)for(const hostSlot of ['empty','placeholder','login','dormant'])await capture({status:{mode,enrollment,hostSlot},rows:[one]});
for(const rows of [[],[one],[one,two],[{...one,declined:true},two],[{...one,declined:true}],[one,{...one,declined:true}]])for(const selectedRouteId of [undefined,one.routeId,'stale'])await capture({status:{mode:'custody',enrollment:{state:'approved'},hostSlot:'dormant',selectedRouteId},rows});
for(const reach of [undefined,{grants:0},{grants:1}])await capture({status:{mode:'custody',enrollment:{state:'approved'},hostSlot:'dormant'},rows:[],reach});
for(const mode of ['local','custody','unreadable'])await capture({status:{mode,enrollment:{state:'approved'},hostSlot:'dormant',modeProblem:'damaged'},listError:'Bearer secret.token project secret=please-redact'});
await capture({status:{},statusError:'never expose this exception'});
const setups=[{status:{mode:'local',enrollment:{state:'idle'},hostSlot:'empty'}},{status:{mode:'custody',enrollment:{state:'pending'},hostSlot:'dormant'}},{status:{mode:'local',enrollment:{state:'approved'},hostSlot:'login'},rows:[one,two]},{status:{mode:'custody',enrollment:{state:'approved'},hostSlot:'dormant'},rows:[one,{...two,declined:true}]},{status:{mode:'unreadable',enrollment:{state:'blocked',code:'missing_token'},hostSlot:'dormant'}}];
for(const spec of setups)for(const actionId of ['request-access','use-vault','use-local','forget'])for(const confirmed of [false,true])await capture(spec,{command:'antigravity',sectionId:'vault',actionId,confirmed});
for(const error of [undefined,'refused','secret=never-print', 'Claustrum enrollment path has a writable ancestor: /shared/folder. Another user could replace it.']){
 for(const actionId of ['use','decline','accept'])for(const itemIndex of [0,1])await capture({...setups[3],error},{command:'antigravity',sectionId:'vault',actionId,itemIndex});
 for(const [index,actionId] of [[0,'request-access'],[2,'use-vault'],[4,'forget']] as const)await capture({...setups[index],error},{command:'antigravity',sectionId:'vault',actionId,confirmed:true});
}
for(const step of enrollments)await capture({...setups[0],step},{command:'antigravity',sectionId:'vault',actionId:'request-access'});
for(const request of [{command:'wrong',sectionId:'vault',actionId:'forget'},{command:'antigravity',sectionId:'missing',actionId:'forget'},{command:'antigravity',sectionId:'vault',actionId:'use',itemId:'stale'}])await capture(setups[3],request);
for(const answers of [[],[0,0,null],[1,0,null],[2,0,null],[3,0,null],[4,0,null,null]])await capture(setups[0],undefined,answers);
for(const answers of [[4,0,0,3,null],[4,0,1,3,null],[4,1,0,3,null],[4,2,false,3,null],[4,2,true,3,null],[4,0,null,3,null]])await capture(setups[3],undefined,answers);
const redactor=commands.createTextRedactor();const texts:any[]=[];
for(const secret of ['Bearer abc.def','bearer abc.def','sk-test_123','eyJabc.def.ghi','ckh_'+'x'.repeat(20),'api_key="abc"','TOKEN: Bearer abc','authorization=Basic abc','a'.repeat(32),'a'.repeat(31),'secret=abc','passWord : abc','token=abc;done','prefix-password=value','prefixapiKey=value'])for(const prefix of ['','α','_','é','A ','\ufeff'])texts.push({input:prefix+secret,result:redactor(prefix+secret)});
const folders=['enrollment token folder /shared is not private: foreign owner','enrollment token folder /shared is not private: sticky ancestor','enrollment token folder /shared is not private: could not read /etc/group to establish a user-private group','Claustrum enrollment path has a writable ancestor: /shared. Another user could replace it.','enrollment token folder /one is not private: foreign owner is not private: sticky ancestor','enrollment token folder /one is not private: could not read file\ufeffname to establish a user-private group','unrecognized error'].map(input=>({input,result:sharedVaultTokenFolderOutcome(new Error(input))??null}));
await writeFile(resolve(process.env.RPI_REFERENCE_OUTPUT!),JSON.stringify({cases,texts,folders})+'\n');console.log(`${cases.length} menu models/actions, ${texts.length} text redactions, ${folders.length} folder refusals`);
