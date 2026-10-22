// AG-3 complete /antigravity menu reference: the unchanged pinned Pi sections
// and command-menu renderer, walked through a scripted UI at every level so the
// candidate's notification strings and cancellation behaviour can be compared.
//
// Usage:
//   node --experimental-transform-types tests/ag3-menu-reference.ts --write
//   node --experimental-transform-types tests/ag3-menu-reference.ts
// RPI_REFERENCE_DIR must point at the locked Antigravity checkout.
import assert from 'node:assert/strict';
import crypto from 'node:crypto';
import {syncBuiltinESMExports} from 'node:module';
import {readFile,writeFile} from 'node:fs/promises';
import {dirname,join,resolve} from 'node:path';
import {fileURLToPath,pathToFileURL} from 'node:url';

const self = fileURLToPath(import.meta.url);
const fixture = resolve(dirname(self),'ag3-menu.json');
const originalRandom = crypto.randomBytes;
(crypto as any).randomBytes = (size:number) => size === 16 ? Buffer.alloc(16,7) : originalRandom(size);
syncBuiltinESMExports();
const root = resolve(process.env.RPI_REFERENCE_DIR!);
const load = (path:string) => import(pathToFileURL(join(root,path)).href);
const {createPiVaultSection,PiVaultSetupError} = await load('packages/pi/src/vault-setup.ts');
const {createPiAntigravityAccountSections} = await load('packages/pi/src/commands.ts');
const {createAntigravityCommandMenu} = await load('packages/core/src/antigravity-command-menu.ts');
const commands = await load('packages/core/src/common-auth-embedded/commands/index.js');
const logger = {warn:()=>{},debug:()=>{},info:()=>{},error:()=>{}};
const refused = 'Choose a vault account to use.';

function session(spec:any) {
  const calls:any[] = [], notifications:any[] = [];
  let connections = 0;
  const setup = {
    status: async () => { calls.push(['status']); if (spec.statusError) throw Error(spec.statusError); return structuredClone(spec.status); },
    useVault: async (route?:string) => perform('useVault',route ?? null),
    useLocal: async () => perform('useLocal'),
    disconnect: async () => perform('disconnect'),
    connectStep: async () => { if (connections++) { calls.push(['watch']); return new Promise(() => {}); } await perform('connectStep'); return spec.step ?? {state:'approved'}; },
  };
  async function perform(op:string,...args:any[]) {
    calls.push([op,...args]);
    if (spec.error) { if (spec.error === 'refused') throw new PiVaultSetupError(refused); throw Error(spec.error); }
  }
  const port = {
    list: async () => { calls.push(['list']); if (spec.listError) throw Error(spec.listError); return structuredClone(spec.rows ?? []); },
    grantReach: () => spec.reach,
    accept: async (route:string) => perform('accept',route),
    decline: async (route:string) => perform('decline',route),
  };
  const accounts = () => spec.missingPort ? undefined : port;
  const menu = createAntigravityCommandMenu({commands,source:'sections',sections:createPiAntigravityAccountSections({setup:async()=>setup,accounts}),extras:[createPiVaultSection({setup:async()=>setup,accounts})],logger});
  const invocation = {sessionId:'session-fixture',notify:(...args:any[]) => notifications.push(args)};
  return {menu,invocation,calls,notifications};
}

/** Every deterministic walk: one section, one entry, then back out twice. */
function plan(menu:any) {
  const plans:any[] = [];
  for (const [index,section] of menu.sections.entries()) {
    plans.push([index,null]);
    const entries = [
      ...section.items.filter((item:any) => item.actions.length > 0).map((item:any) => ({kind:'item',item})),
      ...section.actions.map((action:any) => ({kind:'action',action})),
    ];
    for (const [position,entry] of entries.entries()) {
      if (entry.kind === 'item') {
        for (const [choice,action] of entry.item.actions.entries()) {
          if (action.confirm) { plans.push([index,position,choice,false,null,null]); plans.push([index,position,choice,true,null,null]); }
          else plans.push([index,position,choice,null,null]);
        }
        plans.push([index,position,null,null,null]);
      } else if (entry.action.confirm) {
        plans.push([index,position,false,null,null]);
        plans.push([index,position,true,null,null]);
      } else {
        plans.push([index,position,null,null]);
      }
    }
  }
  return plans;
}

const cases:any[] = [];
async function walk(spec:any,answers:any[]) {
  const s = session(spec);
  const open = await s.menu.open(s.invocation);
  s.calls.length = 0;
  const uiTrace:any[] = [];
  const pending = [...answers];
  await commands.runPiCommandMenu(s.menu,{
    select: async (title:string,labels:string[]) => { uiTrace.push(['select',title,labels]); const selected = pending.shift(); return selected == null ? undefined : labels[selected]; },
    confirm: async (title:string,message:string) => { uiTrace.push(['confirm',title,message]); return pending.shift() === true; },
    notify: (text:string,kind?:string) => { uiTrace.push(['notify',text,kind ?? 'info']); },
  },{sessionId:'session-fixture'});
  assert.equal(pending.length,0,'unconsumed UI answers');
  cases.push({spec,answers,uiTrace,calls:s.calls,notifications:s.notifications});
}

const one = {routeId:'route/private:owner@example.test',label:'private-label',email:'owner@example.test',declined:false};
const two = {...one,routeId:'another-private-route',label:'other-private-label',email:'other@example.test'};
const setups = [
  {status:{mode:'local',enrollment:{state:'idle'},hostSlot:'empty'}},
  {status:{mode:'custody',enrollment:{state:'pending'},hostSlot:'dormant'}},
  {status:{mode:'local',enrollment:{state:'approved'},hostSlot:'login'},rows:[one,two]},
  {status:{mode:'custody',enrollment:{state:'approved'},hostSlot:'dormant'},rows:[one,{...two,declined:true}]},
  {status:{mode:'unreadable',enrollment:{state:'blocked',code:'missing_token'},hostSlot:'dormant'}},
  {status:{mode:'custody',enrollment:{state:'approved'},hostSlot:'dormant'},rows:[],reach:{grants:0}},
];
for (const setup of setups) {
  const s = session(setup);
  const open = await s.menu.open(s.invocation);
  for (const answers of plan(open.menu)) await walk(setup,answers);
}
for (const error of ['refused','secret=never-print','Claustrum enrollment path has a writable ancestor: /shared/folder. Another user could replace it.']) {
  const setup = {...setups[3],error};
  const s = session(setup);
  const open = await s.menu.open(s.invocation);
  for (const answers of plan(open.menu)) await walk(setup,answers);
}

const output = `${JSON.stringify({cases},null,2)}\n`;
if (process.argv.includes('--write')) {
  await writeFile(process.env.RPI_REFERENCE_OUTPUT ?? fixture,output);
  console.log(`${cases.length} scripted menu walks captured`);
} else {
  const committed = await readFile(fixture,'utf8');
  assert.equal(output,committed,'regenerate with --write after reviewing the difference');
  console.log(`check passed: ${cases.length} scripted menu walks`);
}
