import {mkdtemp,writeFile,rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {resolve,join} from 'node:path';
import {pathToFileURL} from 'node:url';
const root=resolve(process.env.RPI_REFERENCE_DIR!);
const {readClaustrumEnrollmentStatus,enrollmentAuthority,enrollmentName,classifyEnrollmentError}=await import(pathToFileURL(join(root,'packages/core/src/common-auth-embedded/claustrum/enrollment.js')).href);
const {ClaustrumCredentialError}=await import(pathToFileURL(join(root,'packages/core/node_modules/@cortexkit/claustrum-client/dist/index.js')).href);
const directory=await mkdtemp(join(process.env.RPI_PRIVATE_TMP??tmpdir(),'vault-enrollment-'));
const paths={statePath:join(directory,'state.json'),tokenPath:join(directory,'token.json')};
const proposedName='antigravity-auth-pi';
const token={token:'a'.repeat(64),token_generation:1};
const pending={version:1,phase:'pending',proposedName,requestSecret:'b'.repeat(64),createdAt:1000,updatedAt:1000};
const approved={version:1,phase:'approved',proposedName,approvedName:proposedName,tokenGeneration:1,updatedAt:1000};
const blocked={version:1,phase:'blocked',proposedName,errorCode:'missing_token',updatedAt:1000};
const denied={version:1,phase:'denied',proposedName,updatedAt:1000};
const cases:any[]=[],names:any[]=[],failures:any[]=[];
async function capture(state:any,credential:any=undefined){
 for(const [path,value] of [[paths.statePath,state],[paths.tokenPath,credential]]) {
  if(value===undefined)await rm(path,{force:true});else await writeFile(path,JSON.stringify(value),{mode:0o600});
 }
 let result;try{result={value:await readClaustrumEnrollmentStatus(paths,proposedName)};}catch(e:any){result={error:{name:e.name,kind:e.kind,message:e.message}};}
 cases.push({state,token:credential,result,authority:await enrollmentAuthority(paths,proposedName)});
}
try {
 for(const state of [undefined,null,{},[],pending,approved,blocked,denied])for(const credential of [undefined,null,{},token,{...token,token_generation:2}])await capture(state,credential);
 for(const state of [pending,approved,blocked,denied])for(const field of Object.keys(state))for(const value of [undefined,null,false,0,-1,1,1.5,9007199254740992,'','other',[],[token.token],{},token.token]) {
  const input=structuredClone(state);if(value===undefined)delete input[field];else input[field]=value;
  await capture(input);await capture(input,token);
 }
 for(const field of ['token','token_generation'])for(const value of [undefined,null,false,0,1,-1,1.5,9007199254740992,'','A'.repeat(64),'a'.repeat(63),[token.token],token.token]) {
  const input=structuredClone(token);if(value===undefined)delete input[field];else input[field]=value;
  for(const state of [undefined,pending,approved,{...approved,proposedName:'different'}])await capture(state,input);
 }
 for(const plugin of ['antigravity-auth','x','1','', '-x','A','x_y','x\n'])for(const host of ['pi','rpi','-x','', 'Pi','x\n']) {
  let result;try{result={value:enrollmentName(plugin,host)};}catch(e:any){result={error:{name:e.name,kind:e.kind,message:e.message}};}names.push({plugin,host,result});
 }
 for(const code of ['invalid_params','pending_exists','not_found','already_consumed','superseded','stale_generation','pending_queue_full','new_code',''])for(const action of ['retry','gone','reauth','reduce_and_retry'])failures.push({code,action,result:classifyEnrollmentError(new ClaustrumCredentialError(code,'transient',action))});
 await writeFile(resolve(process.env.RPI_REFERENCE_OUTPUT!),JSON.stringify({runtime:process.version,proposedName,cases,names,failures},null,2)+'\n');
 console.log(`${cases.length} original enrollment state/authority cases, ${names.length} names and ${failures.length} refusals captured`);
} finally { await rm(directory,{recursive:true,force:true}); }
