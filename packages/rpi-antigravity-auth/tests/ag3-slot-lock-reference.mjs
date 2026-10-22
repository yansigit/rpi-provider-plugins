// AG-3 slot-lock fault reference: the unchanged pinned lease and credential
// slot, driven through corrupt identity, marker corruption, write failures,
// cancellation-before-commit and a real competing child process.
//
// Usage:
//   node tests/ag3-slot-lock-reference.mjs --write   # regenerate the fixture
//   node tests/ag3-slot-lock-reference.mjs           # check the committed fixture
// RPI_REFERENCE_DIR must point at the locked Antigravity checkout.
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {spawn} from 'node:child_process';
import {writeFile,readFile,mkdtemp,mkdir,rm,utimes,symlink,chmod,lstat,readdir} from 'node:fs/promises';
import {join,resolve,dirname} from 'node:path';
import {fileURLToPath} from 'node:url';
import {tmpdir} from 'node:os';
import {setTimeout as sleep} from 'node:timers/promises';
import {pathToFileURL} from 'node:url';

const self = fileURLToPath(import.meta.url);
const fixture = resolve(dirname(self),'ag3-slot-lock.json');
const root = resolve(process.env.RPI_REFERENCE_DIR ?? '');
const scratchRoot = process.env.RPI_PRIVATE_TMP ?? tmpdir();
const load = (path) => import(pathToFileURL(join(root,path)).href);

// --- renewal timers are deterministic ---------------------------------------
const timers = new Map();
const realTimeout = globalThis.setTimeout;
const realClear = globalThis.clearTimeout;
globalThis.setTimeout = (fn, ms, ...args) => {
  if (ms >= 1000) { const id = {unref(){}}; timers.set(id, () => fn(...args)); return id; }
  return realTimeout(fn, ms, ...args);
};
globalThis.clearTimeout = (id) => { if (timers.delete(id)) return; return realClear(id); };

// --- the child half of the competing-process scenario -----------------------
if (process.argv.includes('--child')) {
  const {acquireRefreshFileLockWithClock} = await load('packages/core/src/common-auth-embedded/fs/refresh-file-lock.js');
  const directory = process.argv[process.argv.indexOf('--child') + 1];
  const lock = await acquireRefreshFileLockWithClock({
    path: join(directory,'state.json'), name: 'ceremony', ttlMs: 30_000, renew: false,
    now: () => 100_000, onStep: () => {},
  });
  if (!lock) process.exit(3);
  await writeFile(join(directory,'held.json'), `${JSON.stringify({ownerId: lock.ownerId})}\n`);
  for (;;) {
    try { await readFile(join(directory,'release')); break; }
    catch { await sleep(10); }
  }
  await lock.release();
  process.exit(0);
}

const {acquireRefreshFileLockWithClock} = await load('packages/core/src/common-auth-embedded/fs/refresh-file-lock.js');
const {createPiSlot} = await load('packages/core/src/common-auth-embedded/pi-slot/slot.js');
const marker = 'antigravity-vault-custody';

// --- lease scenarios --------------------------------------------------------
const leaseSpecs = [];
const corruptOwners = [
  ['ownerId number live', {ownerId:7,expiresAt:110000}],
  ['ownerId number stale', {ownerId:7,expiresAt:90000}],
  ['ownerId bool live', {ownerId:true,expiresAt:110000}],
  ['ownerId array live', {ownerId:['x'],expiresAt:110000}],
  ['ownerId object live', {ownerId:{a:1},expiresAt:110000}],
  ['ownerId null live', {ownerId:null,expiresAt:110000}],
  ['expiresAt true', {ownerId:'previous',expiresAt:true}],
  ['expiresAt false', {ownerId:'previous',expiresAt:false}],
  ['expiresAt array empty', {ownerId:'previous',expiresAt:[]}],
  ['expiresAt array one', {ownerId:'previous',expiresAt:[90000]}],
  ['expiresAt array many', {ownerId:'previous',expiresAt:[1,2]}],
  ['expiresAt object', {ownerId:'previous',expiresAt:{}}],
  ['expiresAt hex', {ownerId:'previous',expiresAt:'0x10'}],
  ['expiresAt binary', {ownerId:'previous',expiresAt:'0b100'}],
  ['expiresAt exponent', {ownerId:'previous',expiresAt:'1e5'}],
  ['expiresAt padded', {ownerId:'previous',expiresAt:'  90000  '}],
  ['expiresAt negative', {ownerId:'previous',expiresAt:-1}],
  ['expiresAt zero', {ownerId:'previous',expiresAt:0}],
  ['extra fields', {ownerId:'previous',expiresAt:110000,extra:1}],
];
for (const [name,initial] of corruptOwners)
  leaseSpecs.push({name:`corrupt ${name}`,initial,age:0,steps:['assert','release']});
leaseSpecs.push({name:'symlink to live owner',initial:{ownerId:'previous',expiresAt:110000},age:0,symlink:true,steps:['assert','release']});
leaseSpecs.push({name:'symlink to stale owner',initial:{ownerId:'previous',expiresAt:90000},age:0,symlink:true,steps:['assert','release']});
leaseSpecs.push({name:'legacy directory owner',initial:{ownerId:'previous',expiresAt:90000},legacy:true,steps:['assert','release']});
leaseSpecs.push({name:'legacy directory malformed',legacy:true,malformedOwner:true,steps:['assert','release']});
leaseSpecs.push({name:'marker regular file fresh',initial:{ownerId:'previous',expiresAt:90000},markerKind:'file',markerAge:0,steps:['assert','release']});
leaseSpecs.push({name:'marker regular file stale',initial:{ownerId:'previous',expiresAt:90000},markerKind:'file',markerAge:6000,steps:['assert','release']});
leaseSpecs.push({name:'marker malformed owner fresh',initial:{ownerId:'previous',expiresAt:90000},markerKind:'malformed',markerAge:0,steps:['assert','release']});
leaseSpecs.push({name:'marker wrong owner fresh',initial:{ownerId:'previous',expiresAt:90000},markerKind:'other',markerAge:0,steps:['assert','release']});
leaseSpecs.push({name:'marker wrong owner stale',initial:{ownerId:'previous',expiresAt:90000},markerKind:'other',markerAge:6000,steps:['assert','release']});
// Cancellation before commit: the lock is destroyed while a live lease is held.
leaseSpecs.push({name:'assert malformed while held',steps:['corruptLock','assert','release']});
leaseSpecs.push({name:'assert directory while held',steps:['corruptLockDir','assert','release']});
leaseSpecs.push({name:'assert removed while held',steps:['removeLock','assert','release']});
leaseSpecs.push({name:'renew removed while held',renew:true,steps:['removeLock','renew','assert','release']});
leaseSpecs.push({name:'renew malformed while held',renew:true,steps:['corruptLock','renew','assert','release']});
leaseSpecs.push({name:'renew marker stolen while held',renew:true,race:{step:'renewal-write-ready',mutation:'marker'},steps:['renew','assert','release']});
leaseSpecs.push({name:'release marker stolen while held',renew:true,race:{step:'release-owner-confirmed',mutation:'marker'},steps:['assert','release']});
leaseSpecs.push({name:'release already succeeded',steps:['removeLock','release']});

const leaseCases = [];
for (const spec of leaseSpecs) {
  const directory = await mkdtemp(join(scratchRoot,'ag3-lease-'));
  const path = join(directory,'state.json');
  const lockPath = `${path}.ceremony.lock`;
  const evict = `${lockPath}.evicting`;
  let now = 100000;
  const trace = [];
  let fired = false;
  let lock;
  let owner;
  const save = async (target, value) => writeFile(target, `${JSON.stringify(value)}\n`, {mode:0o600});
  const mutate = async (kind) => {
    if (kind === 'takeover') await save(lockPath, {ownerId:'successor',expiresAt:now + 60000});
    else {
      await mkdir(evict,{recursive:true});
      await save(join(evict,'owner.json'), {ownerId:'other-marker',createdAt:now});
      await utimes(evict, now/1000, now/1000);
    }
  };
  const normalize = (value) => Array.isArray(value) ? value.map(normalize)
    : value && typeof value === 'object' ? Object.fromEntries(Object.entries(value).map(([k,v]) => [k, k === 'ownerId' && v === owner ? '<OWNER>' : normalize(v)]))
    : value;
  const snapshot = async () => {
    let value = null;
    try {
      const bytes = await readFile(lockPath,'utf8');
      try { value = JSON.parse(bytes); } catch { value = 'malformed'; }
    } catch (error) {
      if (error.code === 'EISDIR') {
        try { const bytes = await readFile(join(lockPath,'owner.json'),'utf8'); try { value = JSON.parse(bytes); } catch { value = 'malformed'; } }
        catch { value = null; }
      } else if (error.code !== 'ENOENT') value = 'malformed';
    }
    let markerValue;
    try { markerValue = JSON.parse(await readFile(join(evict,'owner.json'),'utf8')); } catch {}
    let markerKind = null;
    try { const stat = await lstat(evict); markerKind = stat.isDirectory() ? 'directory' : stat.isSymbolicLink() ? 'symlink' : 'file'; } catch {}
    return {lock: normalize(value ?? null), marker: markerValue?.ownerId === 'other-marker' ? 'other-marker' : markerValue ? 'present' : null, markerKind};
  };
  try {
    if (spec.initial !== undefined) {
      if (spec.legacy) {
        await mkdir(lockPath);
        if (!spec.malformedOwner) await save(join(lockPath,'owner.json'), spec.initial);
        else await writeFile(join(lockPath,'owner.json'), 'broken');
      } else {
        await save(lockPath, spec.initial);
        if (spec.symlink) { await rm(lockPath); await save(`${lockPath}.target`, spec.initial); await symlink(`${lockPath}.target`, lockPath); }
      }
      await utimes(lockPath,(now - (spec.age ?? 0))/1000,(now - (spec.age ?? 0))/1000);
    }
    if (spec.markerKind) {
      if (spec.markerKind === 'file') await writeFile(evict, 'marker\n');
      else if (spec.markerKind === 'malformed') { await mkdir(evict,{recursive:true}); await writeFile(join(evict,'owner.json'),'broken'); }
      else if (spec.markerKind === 'other') { await mkdir(evict,{recursive:true}); await save(join(evict,'owner.json'), {ownerId:'other-marker',createdAt:now}); }
      await utimes(evict,(now - (spec.markerAge ?? 0))/1000,(now - (spec.markerAge ?? 0))/1000);
    }
    let finishRenewal;
    lock = await acquireRefreshFileLockWithClock({
      path, name:'ceremony', ttlMs:30000, renew:spec.renew ?? false, now:() => now,
      onStep: async (step) => {
        trace.push(step);
        if (!fired && spec.race?.step === step) { fired = true; await mutate(spec.race.mutation); }
        if (step === 'renewal-finished') finishRenewal?.();
      },
    });
    owner = lock?.ownerId;
    const results = [{operation:'acquire',acquired:!!lock,after:await snapshot()}];
    if (lock) for (const step of spec.steps) {
      let result = {ok:true};
      if (step === 'corruptLock') await writeFile(lockPath,'broken');
      else if (step === 'corruptLockDir') { await rm(lockPath,{force:true}); await mkdir(lockPath); await writeFile(join(lockPath,'owner.json'),'broken'); }
      else if (step === 'removeLock') await rm(lockPath,{recursive:true,force:true});
      else if (step === 'renew') {
        now += 10000;
        const timer = timers.entries().next().value;
        if (timer) { timers.delete(timer[0]); await new Promise((done) => { finishRenewal = done; timer[1](); }); finishRenewal = undefined; }
        else result = {scheduled:false};
      } else {
        try { await lock[step === 'assert' ? 'assertOwned' : 'release'](); }
        catch (error) { result = {error:{name:error.name,message:error.message.replaceAll(directory,'<DIR>')}}; }
      }
      results.push({operation:step,result,lost:lock.hasLost(),after:await snapshot()});
    }
    if (lock && !spec.steps.includes('release')) await lock.release();
    leaseCases.push({spec,trace,results});
  } finally { timers.clear(); await rm(directory,{recursive:true,force:true}); }
}

// --- credential-slot scenarios ----------------------------------------------
const slotSpecs = [];
slotSpecs.push({name:'second instance restores after first enters',raw:'{"google":{"key":"fixture-original"},"other":{}}',steps:[{op:'enter'},{op:'otherInspect'},{op:'otherExit'},{op:'inspect'},{op:'exit'}]});
slotSpecs.push({name:'second instance conflicts after foreign login',raw:'{"google":{"key":"fixture-original"},"other":{}}',steps:[{op:'enter'},{op:'write',target:'auth',raw:'{"google":{"key":"new login"},"other":{}}'},{op:'otherInspect'},{op:'otherExit'},{op:'inspect'},{op:'exit'}]});
slotSpecs.push({name:'interrupted auth rename leaves recoverable stash',raw:'{"google":{"key":"fixture-original"},"other":{}}',hook:'before-rename-auth',steps:[{op:'enter'},{op:'inspect'},{op:'enter'},{op:'inspect'},{op:'exit'}]});
slotSpecs.push({name:'interrupted stash rename leaves original auth',raw:'{"google":{"key":"fixture-original"},"other":{}}',hook:'before-rename-stash',steps:[{op:'enter'},{op:'inspect'},{op:'enter'},{op:'exit'}]});

const slotCases = [];
for (const [index,spec] of slotSpecs.entries()) {
  const directory = await mkdtemp(join(scratchRoot,'ag3-slot-'));
  const dir = join(directory,String(index));
  await mkdir(dir,{mode:0o700});
  const paths = {auth:join(dir,'auth.json'),stash:join(dir,'vault','stash.json'),authDir:dir,stashDir:join(dir,'vault')};
  await mkdir(paths.stashDir,{mode:0o700});
  if (spec.raw !== null) await writeFile(paths.auth,spec.raw,{mode:0o600});
  const hash = createHash('sha256').update(JSON.stringify(paths.auth)).digest('hex');
  let fired = false;
  const events = [];
  const make = () => createPiSlot({authPath:paths.auth,stashPath:paths.stash,provider:'google',placeholderKey:marker},
    (name) => { events.push(name); if (!fired && name === spec.hook) { fired = true; throw Error('injected crash'); } },
    {beforeRename:async (path) => { const name = `before-rename-${path === paths.auth ? 'auth' : 'stash'}`; events.push(name); if (!fired && name === spec.hook) { fired = true; throw Error('injected crash'); } }});
  const slot = make();
  const other = make();
  const snapshot = async (path) => { try { const stat = await lstat(path); if (stat.isDirectory()) return {kind:'directory'}; if (stat.isSymbolicLink()) return {kind:'symlink'}; const bytes = await readFile(path); const text = bytes.toString('utf8'); return {kind:'file',mode:stat.mode & 0o777,...(Buffer.from(text).equals(bytes) ? {text:text.replaceAll(hash,'<auth-identity>')} : {bytes:[...bytes]})}; } catch (error) { if (error.code === 'ENOENT') return null; throw error; } };
  const results = [];
  for (const action of spec.steps) {
    let outcome = {};
    try {
      if (action.op === 'inspect') outcome = {value:await slot.inspect()};
      else if (action.op === 'enter') { await slot.enterVault(); outcome = {value:null}; }
      else if (action.op === 'exit') outcome = {value:await slot.exitVault()};
      else if (action.op === 'otherInspect') outcome = {value:await other.inspect()};
      else if (action.op === 'otherExit') outcome = {value:await other.exitVault()};
      else if (action.op === 'write') await writeFile(paths[action.target],action.raw);
      else throw Error(`unknown op ${action.op}`);
    } catch (error) { outcome = {error:{name:error.name,message:error.message,code:error.code}}; }
    results.push({outcome,events:[...events],auth:await snapshot(paths.auth),stash:await snapshot(paths.stash)});
  }
  slotCases.push({spec,results});
  await rm(directory,{recursive:true,force:true});
}

// --- competing processes ----------------------------------------------------
async function waitFor(path, timeout = 10_000) {
  const deadline = Date.now() + timeout;
  for (;;) {
    try { return await readFile(path); } catch (error) { if (error.code !== 'ENOENT') throw error; }
    if (Date.now() > deadline) throw Error(`timed out waiting for ${path}`);
    await sleep(10);
  }
}
async function processScenario(name) {
  const directory = await mkdtemp(join(scratchRoot,'ag3-process-'));
  const path = join(directory,'state.json');
  const lockPath = `${path}.ceremony.lock`;
  const child = spawn(process.execPath,[self,'--child',directory],{env:process.env,stdio:['ignore','inherit','inherit']});
  const outcome = {name,childExit:null,contender:null,successor:null,leftover:null};
  try {
    await waitFor(join(directory,'held.json'));
    const contender = await acquireRefreshFileLockWithClock({path,name:'ceremony',ttlMs:30_000,renew:false,now:()=>100_000,onStep:()=>{}});
    outcome.contender = contender ? 'acquired' : null;
    if (contender) await contender.release();
    await writeFile(join(directory,'release'),'');
    outcome.childExit = await new Promise((done) => child.on('exit',(code) => done(code)));
    const successor = await acquireRefreshFileLockWithClock({path,name:'ceremony',ttlMs:30_000,renew:false,now:()=>100_000,onStep:()=>{}});
    if (successor) {
      const saved = JSON.parse(await readFile(lockPath,'utf8'));
      outcome.successor = {ownerId:'<OWNER>',expiresAt:saved.expiresAt};
      await successor.release();
    }
    const entries = await readdir(directory);
    entries.sort();
    outcome.leftover = entries;
  } finally {
    if (child.exitCode === null) child.kill();
    await rm(directory,{recursive:true,force:true});
  }
  return outcome;
}
const processCases = [await processScenario('competing child holds then hands off')];

const output = `${JSON.stringify({runtime:process.version,platform:process.platform,lease:leaseCases,slot:slotCases,process:processCases},null,2)}\n`;
if (process.argv.includes('--write')) {
  await writeFile(process.env.RPI_REFERENCE_OUTPUT ?? fixture,output);
  console.log(`${leaseCases.length} lease, ${slotCases.length} slot and ${processCases.length} process scenarios captured`);
} else {
  const committed = await readFile(fixture,'utf8');
  assert.equal(output,committed,'regenerate with --write after reviewing the difference');
  console.log(`check passed: ${leaseCases.length} lease, ${slotCases.length} slot and ${processCases.length} process scenarios`);
}
