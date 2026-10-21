import {readFile,writeFile} from 'node:fs/promises';
import {resolve,join} from 'node:path';
import {pathToFileURL} from 'node:url';
const root=resolve(process.env.RPI_REFERENCE_DIR!);
const {createPiVaultSend,createPiVaultAccountsPort}=await import(pathToFileURL(join(root,'packages/pi/src/vault-source.ts')).href);
const {piNoVaultAccountsGuidance}=await import(pathToFileURL(join(root,'packages/pi/src/vault-setup.ts')).href);
const rosters=JSON.parse(await readFile(new URL('../tests/fixtures/vault-roster.json',import.meta.url),'utf8'));
const routes=[{routeId:'one',credentialId:'credential',accountIdentity:'account',label:'One'},{routeId:'two',credentialId:'second',accountIdentity:'other',label:'Two'}];
const cases:any[]=[],choices:any[]=[];
for(const read of [{ok:false,reason:'damaged'},{ok:false,reason:''},{ok:true,record:{mode:'local'}},{ok:true,record:{mode:'custody'}},...[null,'','one','two','missing'].map(routeId=>({ok:true,record:{mode:'custody',routeId}}))]) {
 for(const count of [0,1,2])for(const cause of ['unknown','no-grant','grant-reaches-nothing']) {
  const rows=routes.slice(0,count),source={routes:()=>rows,refresh:async()=>undefined,emptyRosterCause:()=>cause,send:async(ref:any)=>ref};
  let result;try{result={value:await createPiVaultSend({readMode:async()=>read,source:async()=>source})(()=>{},new AbortController().signal)};}catch(e:any){result={error:e.message};}
  cases.push({read,routes:rows,cause,result});
 }
}
for(const spec of rosters.cases){
 const roster=spec.result;
 const calls:any[]=[];const source={refresh:async()=>roster,grantReach:()=>({grants:3}),decline:async(ref:any)=>calls.push(['decline',ref]),accept:async(ref:any)=>calls.push(['accept',ref])};
 const port=createPiVaultAccountsPort({readMode:async()=>({ok:true,record:{mode:'custody'}}),source:async()=>source});
 const result=await port.list();for(const row of result)await port.decline(row.routeId);
 choices.push({roster,result,references:calls.map(v=>v[1])});
}
const guidance=[undefined,{grants:0},{grants:1}].map(reach=>({reach:reach??null,result:piNoVaultAccountsGuidance(reach)}));
await writeFile(resolve(process.env.RPI_REFERENCE_OUTPUT!),JSON.stringify({cases,choices,guidance})+'\n');
console.log(`${cases.length} request selections, ${choices.length} account lists, ${guidance.length} guidance cases`);
