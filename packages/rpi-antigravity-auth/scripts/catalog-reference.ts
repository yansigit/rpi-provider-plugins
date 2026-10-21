// Run with the reference's locked Bun 1.4.2. Never uses real credentials/network.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

const reference = resolve(process.argv[2]);
const integrated = resolve(process.argv[3]);
assert.equal(execFileSync('git',['-C',reference,'rev-parse','HEAD'],{encoding:'utf8'}).trim(),'a97f9055017f3c1546afbfe61a19d50b428be3d8');
const output = resolve(process.argv[4]);
const temporary = await mkdtemp(join(process.env.TMPDIR ?? '/tmp', 'rpi-agy-catalog-'));
process.env.PI_CODING_AGENT_DIR = temporary;
globalThis.fetch = async () => { throw new Error('unexpected network request in catalog reference'); };
try {
  const source = (path: string) => pathToFileURL(join(reference, path)).href;
  const registry = await import(source('packages/core/src/model-registry.ts'));
  const stream = await import(source('packages/pi/src/stream.ts'));
  const installed = join(integrated, 'node_modules/@cortexkit/pi-antigravity-auth');
  const manifest = JSON.parse(await readFile(join(installed, 'package.json'), 'utf8'));
  assert.equal(manifest.version, '3.0.9');
  const host = JSON.parse(await readFile(join(integrated, 'node_modules/@earendil-works/pi-ai/package.json'), 'utf8'));
  assert.equal(host.version, '1.3.0');
  const { registerCortexKitPiAntigravity } = await import(pathToFileURL(join(installed, manifest.exports['.'].import)).href);
  const registrations: any[] = [];
  await registerCortexKitPiAntigravity({
    registerProvider: (id: string, config: unknown) => registrations.push({id,config}),
    registerCommand: () => {}, on: () => {},
  }, {
    vaultDir: join(temporary, 'vault'), modeFile: join(temporary, 'mode.json'), rosterFile: join(temporary, 'roster.json'),
    nativeAuthPath: join(temporary, 'auth.json'), nativeStashPath: join(temporary, 'stash.json'),
    readHostSlot: () => undefined,
    connect: async () => { throw new Error('unexpected vault connection'); },
    connectEnrollment: async () => { throw new Error('unexpected enrollment connection'); },
    loadPiSlot: async () => ({ openPiSlot: () => ({ exitVault: async () => 'absent', enterVault: async () => { throw new Error('unexpected custody'); } }) }),
  });
  assert.equal(registrations.length, 1);
  const {id:provider, config} = registrations[0];
  assert.equal(provider, 'google');
  assert.equal(typeof config.oauth.login, 'function');
  const catalog = config.models;
  const native = registry.AGY_NATIVE_MODEL_DEFINITIONS;
  const aliases = registry.getResolverAliasMap();
  const names = new Set<string>([...catalog.map((model: any) => model.id), ...Object.keys(native), ...Object.keys(aliases),
    'gemini-pro-agent','gemini-3.1-pro','gemini-3.6-flash','gemini-3.7-flash','gemini-3.8-flash-tiered',
    'gemini-3.1-flash-image','claude-sonnet-4-6-thinking','unknown','gemini-2.5-flash','gemini-2.5-pro']);
  for (const name of [...names]) {
    for (const prefix of ['', 'antigravity-']) {
      for (const suffix of ['', '-minimal','-low','-medium','-high','-xhigh','-preview','-preview-customtools']) {
        const value = prefix + name + suffix;
        names.add(value); names.add(value.toUpperCase());
      }
    }
  }
  const cases: any[] = [];
  for (const id of names) for (const reasoning of [false,true]) for (const effort of [undefined,'minimal','low','medium','high','xhigh']) {
    cases.push({id,reasoning,...(effort ? {effort} : {}),retired:registry.isRetiredAgyModel(id),
      resolved:stream.resolvePiAntigravityModel({id,reasoning},effort)});
  }
  const hashes = {} as Record<string,string>;
  for (const path of ['packages/pi/src/index.ts','packages/pi/src/stream.ts','packages/core/src/model-registry.ts','packages/core/src/transform/model-resolver.ts']) {
    hashes[path] = createHash('sha256').update(await readFile(join(reference,path))).digest('hex');
  }
  const staticData = {provider, name:config.name, api:config.api, baseUrl:config.baseUrl, catalog,native,aliases,retiredMessage:registry.RETIRED_AGY_MODEL_MESSAGE};
  if (process.argv[5]) await writeFile(resolve(process.argv[5]),JSON.stringify(staticData,null,2)+'\n');
  await writeFile(output, JSON.stringify({reference:'a97f9055017f3c1546afbfe61a19d50b428be3d8', catalogHost:host.version,
    sourceHashes:hashes, provider, name:config.name, api:config.api, baseUrl:config.baseUrl,
    catalog, native, aliases, retiredMessage:registry.RETIRED_AGY_MODEL_MESSAGE, cases})+'\n');
  console.log(JSON.stringify({models:catalog.length,cases:cases.length,output}));
} finally { await rm(temporary,{recursive:true,force:true}); }
