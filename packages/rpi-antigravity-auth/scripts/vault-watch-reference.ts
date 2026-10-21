import {writeFile} from 'node:fs/promises';import {join,resolve} from 'node:path';import {pathToFileURL} from 'node:url';
const root=resolve(process.env.RPI_REFERENCE_DIR!);
const {createPiVaultSection}=await import(pathToFileURL(join(root,'packages/pi/src/vault-setup.ts')).href);
const cases:any[]=[];
for(const steps of [
 [{state:'approved'}],[{state:'denied'}],[{state:'blocked',code:'pending_exists'}],[{state:'pending'},{state:'approved'}],[{state:'busy'},{state:'pending'},{state:'approved'}],[{state:'pending'}],
 [{error:'connection failed'}],[{state:'pending'},{error:'poll failed'}],[{error:'enrollment token folder /shared is not private: foreign owner'}],
])for(const idle of [false,true]){
 const spec={steps,idle};let now=1000,first=true,index=0;const trace:any[]=[],notifications:any[]=[];
 const setup={status:async()=>({mode:'local',hostSlot:'empty',enrollment:{state:'idle'}}),sourceStatus:async()=>{trace.push(['sourceStatus']);return {enrollment:{state:idle?'idle':'pending'}};},connectStep:async()=>{if(first){first=false;return {state:'pending'};}trace.push(['connectStep']);const step=steps[Math.min(index++,steps.length-1)];if(step.error)throw Error(step.error);return step;}};
 const dateNow=Date.now;Date.now=()=>now;
 try{
  const section=createPiVaultSection({setup:async()=>setup,wait:{sleep:async(ms:number)=>{trace.push(['sleep',ms]);now+=600000;}}});
  const built=await section.build({});const action=built.actions.find((v:any)=>v.id==='request-access');
  const outcome=await action.run({invocation:{notify:(text:string,kind:string)=>notifications.push([text,kind])}});
  for(let i=0;i<6;i++)await new Promise(resolve=>setImmediate(resolve));
  cases.push({spec,outcome,trace,notifications});
 }finally{Date.now=dateNow;}
}
await writeFile(resolve(process.env.RPI_REFERENCE_OUTPUT!),JSON.stringify({cases})+'\n');console.log(`${cases.length} approval-watch traces`);
