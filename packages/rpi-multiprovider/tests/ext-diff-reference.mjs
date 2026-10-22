// Differential reference for the extension composition/resume layer of
// multiprovider (upstream bd97d65): the session-pin journal across resume,
// inherited env pins onto child sessions, disabled/removed account warnings,
// branch prefixes and reloads, the process-wide live-provider registry that
// hands the provider registry back when sessions end, and Pi's composed
// provider wrapper.
//
// The upstream TypeScript and extension are imported unchanged and
// sha256-asserted. Clock, crypto.randomUUID and the 50 ms poll timer are
// injected, so the whole run is deterministic and never touches real time,
// credentials or the network. Every scenario drives the real extension through
// a scripted ExtensionAPI/ExtensionContext exactly like
// tests/extension-resume.spec.ts, tests/extension-composition.spec.ts and the
// extension-level parts of tests/session-pins.spec.ts.
//
//   node --experimental-transform-types tests/ext-diff-reference.mjs          check
//   node --experimental-transform-types tests/ext-diff-reference.mjs --write  regenerate
import assert from 'node:assert/strict';
import crypto, { createHash } from 'node:crypto';
import { mkdtemp, readFile, writeFile } from 'node:fs/promises';
import { syncBuiltinESMExports } from 'node:module';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const args = process.argv.slice(2);
const writeMode = args.includes('--write');
const rootArg = args.find(arg => !arg.startsWith('--'));
const root = resolve(rootArg ?? join(here, '../../../../provider-references/multiprovider'));
const piAi = resolve(root, '../integrated/node_modules/@earendil-works/pi-ai');
const fixturePath = join(here, 'ext-diff-reference.json');

const EXPECTED_SHA256 = {
  'extensions/multiprovider.ts': '086a0d9c4fbc8431e81026e5d689747d1c31dfcdf89c9d180c0bbd7e37c0a267',
  'src/announcement.ts': 'fc0bf72467dba7b524146ade4a66c420d546a01e7867b8095c8b7d8b1804ae18',
  'src/auth-store.ts': '29916553aa160b3c676ab843891da2b202fadf536fa220980d564b4ed50d48c6',
  'src/index.ts': '90b3b79432e4d556f75b7b8e25fc3fb512cb56293f5263e6b2090465f5cfaa66',
  'src/lift.ts': '887b91aab533565504449b6094c1a2b076832b0fdc73bfc352e2c46a53ac3695',
  'src/managed.ts': '5a139118f805ed4825259df761dd5e2fbec515da19d3bc10b9719c9bf30bdeef',
  'src/service.ts': '2eafa61662b3ee75a96a7b07a6ccad62ca600bbb2139a407b81cd9983564a026',
  'src/session-pins.ts': '45b1ff1fbfcc6853a717b917aaede404f32fe96a64208c75283a53e954bdc005',
  'src/types.ts': 'aaf5e77ffc5738bfc5ee642a61238793ec0869123ceef12a9650c1384b0d181e',
  'src/virtual.ts': '6c38efe1d8bcd180e3287b182df592914a1af0a7091f872cd6dc32042575f808',
  'tests/extension-resume.spec.ts': 'fb768939799048972fbb4d805fb110a400435c3ce000325cea8df54ced18caac',
  'tests/extension-composition.spec.ts': '4a65b884b4c99ff12e68d58570748c7dfb11cb402af214d0ec8b7d1a6a4872f0',
  'tests/session-pins.spec.ts': 'b76859b11511fe615ca0b5769152aa049e4d55677cfefb25ae287d23df8f7173',
};

const hashes = {};
for (const file of Object.keys(EXPECTED_SHA256)) {
  const digest = createHash('sha256').update(await readFile(join(root, file))).digest('hex');
  assert.equal(digest, EXPECTED_SHA256[file], `sha256 mismatch for ${file}`);
  hashes[file] = digest;
}

// --- injected clock / ids / poll timer -----------------------------------
let now = 1_700_000_000_000;
let sequence = 0;
const originalDate = Date;
class Clock extends originalDate {
  constructor(...values) { super(...(values.length ? values : [now])); }
  static now() { return now; }
}
globalThis.Date = Clock;
Object.defineProperty(globalThis.performance, 'now', { value: () => now - 1_700_000_000_000 });
crypto.randomUUID = () => `account-${++sequence}`;
syncBuiltinESMExports();
const originalTimeout = globalThis.setTimeout;
globalThis.setTimeout = (fn, ms, ...rest) => {
  if (ms !== 50) return originalTimeout(fn, ms, ...rest);
  now += 50;
  queueMicrotask(() => fn(...rest));
  return 0;
};

// --- upstream imports (unchanged, sha256-asserted above) -----------------
const pi = await import(pathToFileURL(join(piAi, 'dist/index.js')));
const { createAssistantMessageEventStream, normalizeContext } = pi;
const { MULTIPROVIDER_REGISTER_EVENT, MULTIPROVIDER_SERVICE_EVENT, MultiAuthStore, SESSION_PIN_ENTRY_TYPE } =
  await import(pathToFileURL(join(root, 'src/index.ts')));
const { default: multiprovider } = await import(pathToFileURL(join(root, 'extensions/multiprovider.ts')));

const LIFTED_BASE = Symbol.for('pi-multiprovider.lifted-base');
const liveProviders = (globalThis[Symbol.for('pi-multiprovider.live-providers')] ??= new Map());

const model = {
  id: 'probe-model', name: 'Probe Model', api: 'probe-api', provider: 'example',
  baseUrl: 'https://example.invalid', reasoning: false, input: ['text'],
  cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 }, contextWindow: 1_000, maxTokens: 100,
};
const done = {
  role: 'assistant', content: [], api: model.api, provider: model.provider, model: model.id,
  usage: {
    input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0,
    cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
  },
  stopReason: 'stop', timestamp: 0,
};

// The API keys the real base provider was called with, in order.
let served = [];

// A plain provider (not createProvider) so the stream sees exactly the API key
// the lift chose instead of re-resolving its own.
const base = {
  id: 'example', name: 'Example',
  auth: {
    apiKey: {
      name: 'Example API key',
      async resolve(input) { return { auth: { apiKey: input?.credential?.key ?? 'k-default' } }; },
    },
  },
  getModels: () => [model],
  stream(_model, _context, options) {
    served.push(options?.apiKey);
    const events = createAssistantMessageEventStream();
    queueMicrotask(() => { events.push({ type: 'done', reason: 'stop', message: done }); events.end(done); });
    return events;
  },
  streamSimple(m, c, o) { return base.stream(m, c, o); },
};

const settle = async () => {
  for (let index = 0; index < 40; index += 1) {
    await new Promise(resolve => setImmediate(resolve));
    await new Promise(resolve => originalTimeout(resolve, 2));
  }
};

function classify(provider) {
  if (provider === undefined) return null;
  return {
    id: provider.id,
    isLift: LIFTED_BASE in provider,
    liftedBaseId: LIFTED_BASE in provider ? provider[LIFTED_BASE].id : null,
    hasHeaders: provider.headers !== undefined,
  };
}

// `plain` is Pi's registry as the resume spec scripts it. `composed` mirrors
// Pi's model runtime when models.json (or another extension) overlays the
// provider: registerProvider stores the native provider, and getProvider
// returns a freshly composed object that delegates to it and shares only its
// headers by reference.
function makeRegistry(mode) {
  const native = new Map([[base.id, base]]);
  const view = new Map();
  const compose = provider => ({
    id: provider.id,
    name: provider.name,
    ...(provider.headers === undefined ? {} : { headers: provider.headers }),
    auth: provider.auth,
    getModels: () => provider.getModels(),
    stream: (m, c, o) => provider.stream(m, c, o),
    streamSimple: (m, c, o) => provider.streamSimple(m, c, o),
  });
  for (const [id, provider] of native) view.set(id, mode === 'composed' ? compose(provider) : provider);
  return {
    mode,
    register(provider) {
      native.set(provider.id, provider);
      view.set(provider.id, mode === 'composed' ? compose(provider) : provider);
    },
    unregister(id) { native.delete(id); view.delete(id); },
    get: id => view.get(id),
    has: id => view.has(id),
    nativeOf: id => native.get(id),
  };
}

// Resolves auth the way Pi does and streams through the registry's provider.
async function request(registry) {
  served = [];
  const provider = registry.get(base.id);
  assert.ok(provider !== undefined, 'the registry has the provider');
  const resolved = await provider.auth.apiKey.resolve({
    ctx: { env: async () => undefined, fileExists: async () => false },
    signal: new AbortController().signal,
  });
  await provider.streamSimple(model, normalizeContext({ messages: [] }), {
    apiKey: resolved?.auth?.apiKey,
    ...(resolved?.auth?.headers === undefined ? {} : { headers: resolved.auth.headers }),
  }).result();
  return [...served];
}

// Boots one real extension instance against duck-typed Pi APIs. `registry`
// stands in for Pi's provider registry; sessions of one process share it.
async function launch(options = {}) {
  const {
    entries = [], sessionId = 'session-1', registry, trace = null, notifySink = null,
  } = options;
  const localEntries = structuredClone(entries);
  const notifications = [];
  const providers = [];
  const handlers = new Map();
  const bus = new Map();
  const commands = new Map();
  const accountChanges = [];
  let announcement;
  let unsubscribeAccountChanges;

  const ctx = {
    ui: {
      notify(message, level = 'info') {
        notifications.push([message, level]);
        if (notifySink !== null) notifySink.push([message, level]);
      },
      async select() { return undefined; },
      async input() { return undefined; },
      async confirm() { return false; },
      async custom() { return undefined; },
    },
    mode: 'tui',
    hasUI: true,
    cwd: process.cwd(),
    sessionManager: { getSessionId: () => sessionId, getEntries: () => localEntries },
    modelRegistry: {
      getProvider: id => registry.get(id),
      getAll: () => [model],
      getApiKeyAndHeaders: async () => ({ ok: true }),
      getProviderAuthStatus: () => ({ configured: true }),
    },
    model: undefined,
    isIdle: () => true,
    isProjectTrusted: () => true,
    signal: undefined,
    abort() {},
    hasPendingMessages: () => false,
    shutdown() {},
    getContextUsage: () => undefined,
    compact() {},
    getSystemPrompt: () => '',
  };

  const piApi = {
    events: {
      emit(name, value) { for (const callback of bus.get(name) ?? []) callback(value); },
      on(name, callback) {
        const listeners = bus.get(name) ?? new Set();
        listeners.add(callback); bus.set(name, listeners);
        return () => listeners.delete(callback);
      },
    },
    on(name, handler) {
      const list = handlers.get(name) ?? [];
      list.push(handler);
      handlers.set(name, list);
    },
    registerProvider(provider) {
      providers.push(provider);
      registry.register(provider);
      if (trace !== null) trace.push({ op: 'register', ...classify(provider) });
    },
    unregisterProvider(id) {
      registry.unregister(id);
      if (trace !== null) trace.push({ op: 'unregister', id });
    },
    getAllTools: () => [],
    registerCommand(name, definition) { commands.set(name, definition); },
    appendEntry(customType, data) {
      localEntries.push({
        type: 'custom', customType, data,
        id: 'entry-' + (localEntries.length + 1), parentId: null,
        timestamp: new Date().toISOString(),
      });
    },
  };

  // Mirrors pi-better-openai: the service event re-fires with the same stable
  // object, so the identity check keeps the change subscription attached once.
  bus.set(MULTIPROVIDER_SERVICE_EVENT, new Set([(value) => {
    if (value === announcement || typeof value?.onActiveAccountChanged !== 'function') return;
    unsubscribeAccountChanges?.();
    announcement = value;
    unsubscribeAccountChanges = value.onActiveAccountChanged('example', event => {
      accountChanges.push({
        providerId: event.providerId,
        account: event.account === undefined
          ? null
          : { id: event.account.id, label: event.account.label, authKind: event.account.authKind },
      });
    });
  }]));

  await multiprovider(piApi);
  assert.ok(announcement !== undefined, 'extension did not announce its service');

  const emit = async (...names) => {
    for (const name of names) {
      for (const handler of handlers.get(name) ?? []) await handler({ type: name }, ctx);
    }
  };

  return {
    entries: localEntries,
    providers,
    notifications,
    accountChanges,
    ctx,
    emit,
    start: async () => { ctx.model = model; await emit('session_start'); },
    stop: async () => { await emit('session_shutdown'); },
    beforeAgentStart: async () => { await emit('before_agent_start'); },
    active: poolId => announcement.getActiveAccount(poolId, ctx),
    switchAccount: async (value) => {
      const command = commands.get('switch-account');
      assert.ok(command !== undefined, '/switch-account is registered');
      await command.handler(value, ctx);
    },
    announceIntegration: integration => piApi.events.emit(MULTIPROVIDER_REGISTER_EVENT, integration),
  };
}

function journal(entries) {
  return entries
    .filter(entry => entry && typeof entry === 'object' && entry.customType === SESSION_PIN_ENTRY_TYPE)
    .map(entry => entry.data);
}

async function seedAccounts(accounts, providerId = 'example') {
  const store = new MultiAuthStore();
  for (const account of accounts) {
    await store.addAccount(providerId, {
      label: account.label,
      credential: { type: 'api_key', key: account.key },
      ...(account.enabled === undefined ? {} : { enabled: account.enabled }),
      ...(account.priority === undefined ? {} : { priority: account.priority }),
    });
  }
  await store.updateSchedulerSettings({ errorsBeforeSwitch: 1 });
  const pool = await store.getPool(providerId);
  return pool.accounts.map(account => ({
    id: account.id, label: account.label, authKind: account.authKind,
    enabled: account.enabled, weight: account.weight, priority: account.priority,
  }));
}

// --- resume scenarios ----------------------------------------------------
const resumeCases = [];

async function resumeCase(name, spec) {
  process.env.PI_CODING_AGENT_DIR = await mkdtemp(join(tmpdir(), 'pi-multiprovider-extdiff-'));
  liveProviders.clear();
  const accounts = await seedAccounts(spec.accounts);

  const previousEnv = process.env.PI_MULTIPROVIDER_SESSION_PINS;
  try {
    let entries = typeof spec.entries === 'function' ? spec.entries(accounts) : structuredClone(spec.entries ?? []);
    const env = typeof spec.env === 'function' ? spec.env(accounts) : spec.env;
    if (spec.live !== undefined) {
      const live = await launch({ registry: makeRegistry('plain') });
      await live.start();
      for (const op of spec.live.ops) await live.switchAccount(op);
      const full = structuredClone(live.entries);
      entries = spec.branch === undefined ? full : full.slice(0, spec.branch);
      await live.stop();
      liveProviders.clear();
    }
    if (env !== undefined) process.env.PI_MULTIPROVIDER_SESSION_PINS = env;
    else delete process.env.PI_MULTIPROVIDER_SESSION_PINS;

    const registry = makeRegistry('plain');
    if (spec.pending === true) registry.unregister(base.id);
    const resumed = await launch({ entries, sessionId: spec.sessionId ?? 'session-1', registry });
    await resumed.start();
    await settle();
    if (spec.pending === true) {
      registry.register(base);
      await resumed.beforeAgentStart();
      await settle();
    }
    if (spec.reload === true) {
      await resumed.stop();
      await resumed.start();
      await settle();
    }
    const active = {};
    for (const poolId of spec.activePools ?? ['example']) {
      active[poolId] = (await resumed.active(poolId)) ?? null;
    }
    const observation = {
      journal: journal(resumed.entries),
      notifications: resumed.notifications,
      accountChanges: resumed.accountChanges,
      active,
    };
    await resumed.stop();
    liveProviders.clear();

    resumeCases.push({
      name,
      input: {
        sessionId: spec.sessionId ?? 'session-1',
        entries,
        env: env === undefined ? {} : { PI_MULTIPROVIDER_SESSION_PINS: env },
        reload: spec.reload === true,
        pending: spec.pending === true,
        pools: [{ id: 'example', affinity: spec.affinity ?? true, accounts }],
      },
      expected: observation,
    });
  } finally {
    if (previousEnv === undefined) delete process.env.PI_MULTIPROVIDER_SESSION_PINS;
    else process.env.PI_MULTIPROVIDER_SESSION_PINS = previousEnv;
  }
}

const ACCOUNTS = [
  { label: 'Work', key: 'k-work' },
  { label: 'Personal', key: 'k-personal' },
];
const accountId = (accounts, label) => accounts.find(account => account.label === label).id;
const personal = accounts => accountId(accounts, 'Personal');
const work = accounts => accountId(accounts, 'Work');

await resumeCase('resume restores the last switch and notifies followers', {
  accounts: ACCOUNTS,
  live: { ops: ['personal'] },
});
await resumeCase('an explicit return to automatic is not resurrected', {
  accounts: ACCOUNTS,
  live: { ops: ['personal', 'auto'] },
});
await resumeCase('a fresh session without a journal stays automatic', {
  accounts: ACCOUNTS,
});
await resumeCase('a removed pinned account warns and falls back to automatic', {
  accounts: ACCOUNTS,
  entries: [{
    type: 'custom', customType: SESSION_PIN_ENTRY_TYPE,
    data: { pool: 'example', key: 'session-1', accountId: 'deleted-account', label: 'Deleted' },
    id: 'entry-1', parentId: null, timestamp: '2023-11-14T22:13:20.000Z',
  }],
});
await resumeCase('a disabled pinned account warns and falls back to automatic', {
  accounts: [{ label: 'Work', key: 'k-work' }, { label: 'Personal', key: 'k-personal', enabled: false }],
  entries: accounts => [{
    type: 'custom', customType: SESSION_PIN_ENTRY_TYPE,
    data: { pool: 'example', key: 'session-1', accountId: personal(accounts), label: 'Personal' },
    id: 'entry-1', parentId: null, timestamp: '2023-11-14T22:13:20.000Z',
  }],
});
await resumeCase('an inherited env pin binds onto the child session', {
  accounts: ACCOUNTS,
  sessionId: 'child-session',
  env: accounts => JSON.stringify([{ pool: 'example', accountId: personal(accounts), label: 'Personal' }]),
});
await resumeCase('the child journal wins over inherited env pins', {
  accounts: ACCOUNTS,
  sessionId: 'child-session',
  entries: [{
    type: 'custom', customType: SESSION_PIN_ENTRY_TYPE,
    data: { pool: 'example', key: 'child-session' },
    id: 'entry-1', parentId: null, timestamp: '2023-11-14T22:13:20.000Z',
  }],
  env: accounts => JSON.stringify([{ pool: 'example', accountId: personal(accounts), label: 'Personal' }]),
});
await resumeCase('a stale inherited pin warns and is not journalled', {
  accounts: ACCOUNTS,
  env: () => JSON.stringify([{ pool: 'example', accountId: 'deleted-account', label: 'Deleted' }]),
});
await resumeCase('an inherited pin waits for its pool and applies at the next reconcile', {
  accounts: ACCOUNTS,
  sessionId: 'child-session',
  pending: true,
  env: accounts => JSON.stringify([{ pool: 'example', accountId: personal(accounts), label: 'Personal' }]),
});
await resumeCase('a branch prefix keeps the decision that was current then', {
  accounts: ACCOUNTS,
  live: { ops: ['personal', 'auto'] },
  branch: 1,
});
await resumeCase('a branch after the later decision keeps the later decision', {
  accounts: ACCOUNTS,
  live: { ops: ['personal', 'auto'] },
  branch: 2,
});
await resumeCase('a reload replays the journal on the second start', {
  accounts: ACCOUNTS,
  live: { ops: ['personal'] },
  reload: true,
});
await resumeCase('a reload does not re-journal an inherited pin', {
  accounts: ACCOUNTS,
  sessionId: 'child-session',
  reload: true,
  env: accounts => JSON.stringify([{ pool: 'example', accountId: personal(accounts), label: 'Personal' }]),
});
await resumeCase('an inherited pin outranks disabled affinity', {
  accounts: ACCOUNTS,
  affinity: false,
  sessionId: 'child-session',
  env: accounts => JSON.stringify([{ pool: 'example', accountId: personal(accounts), label: 'Personal' }]),
});
await resumeCase('two accounts in the journal: the later decision wins', {
  accounts: ACCOUNTS,
  entries: accounts => [
    {
      type: 'custom', customType: SESSION_PIN_ENTRY_TYPE,
      data: { pool: 'example', key: 'session-1', accountId: work(accounts), label: 'Work' },
      id: 'entry-1', parentId: null, timestamp: '2023-11-14T22:13:20.000Z',
    },
    {
      type: 'custom', customType: SESSION_PIN_ENTRY_TYPE,
      data: { pool: 'example', key: 'session-1', accountId: personal(accounts), label: 'Personal' },
      id: 'entry-2', parentId: 'entry-1', timestamp: '2023-11-14T22:13:21.000Z',
    },
  ],
});

// --- composition scenarios -----------------------------------------------
const compositionCases = [];
const POOLED = {
  id: 'pooled', label: 'Pooled',
  models: [{ id: 'ultra', backends: [{ providerId: 'example', modelId: 'probe-model' }] }],
};

async function compositionCase(name, spec) {
  process.env.PI_CODING_AGENT_DIR = await mkdtemp(join(tmpdir(), 'pi-multiprovider-extdiff-'));
  liveProviders.clear();
  const store = new MultiAuthStore();
  for (const providerId of spec.managed ?? ['example']) {
    await store.addAccount(providerId, { label: 'Work', credential: { type: 'api_key', key: 'k-work' } });
  }
  for (const config of spec.virtual ?? []) await store.saveVirtualProvider(config);
  await store.updateSchedulerSettings({ errorsBeforeSwitch: 1 });

  const registry = makeRegistry(spec.registry);
  const trace = [];
  const notifications = [];
  const requests = [];
  const sessions = [];
  try {
    for (const op of spec.ops) {
      trace.push({ op: 'step', name: `${op.session}:${op.op}` });
      if (op.op === 'load') {
        sessions[op.session] = await launch({
          registry, trace, notifySink: notifications, sessionId: `session-${op.session}`,
        });
      } else if (op.op === 'start') {
        await sessions[op.session].start();
      } else if (op.op === 'before_agent_start') {
        await sessions[op.session].beforeAgentStart();
      } else if (op.op === 'shutdown') {
        await sessions[op.session].stop();
      } else if (op.op === 'request') {
        const keys = await request(registry);
        assert.deepEqual(keys, ['k-default'], `${name}: the base credential serves the request`);
        requests.push(classify(registry.get('example')));
      }
      await settle();
    }
    const registryAfter = {};
    for (const id of spec.ids) {
      registryAfter[id] = registry.has(id)
        ? { view: classify(registry.get(id)), native: classify(registry.nativeOf(id)) }
        : null;
    }
    compositionCases.push({
      name,
      input: {
        registry: spec.registry,
        managed: spec.managed ?? ['example'],
        bases: spec.bases ?? ['example'],
        models: [model],
        virtual: spec.virtual ?? [],
        ids: spec.ids,
        ops: spec.ops,
      },
      expected: { trace, registry: registryAfter, requests, notify: notifications },
    });
  } finally {
    liveProviders.clear();
    for (const config of spec.virtual ?? []) await store.removeVirtualProvider(config.id).catch(() => {});
  }
}

await compositionCase('a composed registry lifts the base once', {
  registry: 'composed',
  ids: ['example'],
  ops: [
    { session: 0, op: 'load' },
    { session: 0, op: 'start' },
    { session: 0, op: 'before_agent_start' },
    { session: 0, op: 'before_agent_start' },
    { session: 0, op: 'request' },
  ],
});
await compositionCase('a child session lifts the base, not the parent lift', {
  registry: 'composed',
  ids: ['example'],
  ops: [
    { session: 0, op: 'load' },
    { session: 0, op: 'start' },
    { session: 1, op: 'load' },
    { session: 1, op: 'start' },
    { session: 0, op: 'request' },
    { session: 0, op: 'before_agent_start' },
    { session: 0, op: 'request' },
    { session: 1, op: 'shutdown' },
    { session: 0, op: 'request' },
  ],
});
await compositionCase('sessions hand the registry back in whatever order they end', {
  registry: 'plain',
  ids: ['example'],
  ops: [
    { session: 0, op: 'load' },
    { session: 0, op: 'start' },
    { session: 1, op: 'load' },
    { session: 1, op: 'start' },
    { session: 2, op: 'load' },
    { session: 2, op: 'start' },
    { session: 1, op: 'shutdown' },
    { session: 0, op: 'request' },
    { session: 2, op: 'shutdown' },
    { session: 0, op: 'request' },
  ],
});
await compositionCase('a session shutdown restores the original base', {
  registry: 'plain',
  ids: ['example'],
  ops: [
    { session: 0, op: 'load' },
    { session: 0, op: 'start' },
    { session: 0, op: 'shutdown' },
  ],
});
await compositionCase('a subagent ending keeps the parent virtual provider registered', {
  registry: 'plain',
  ids: ['example', 'pooled'],
  virtual: [POOLED],
  ops: [
    { session: 0, op: 'load' },
    { session: 0, op: 'start' },
    { session: 1, op: 'load' },
    { session: 1, op: 'start' },
    { session: 1, op: 'shutdown' },
    { session: 0, op: 'request' },
    { session: 0, op: 'shutdown' },
  ],
});
await compositionCase('a parent ending removes the last virtual provider', {
  registry: 'plain',
  ids: ['example', 'pooled'],
  virtual: [POOLED],
  ops: [
    { session: 0, op: 'load' },
    { session: 0, op: 'start' },
    { session: 0, op: 'shutdown' },
  ],
});

const output = {
  reference: 'bd97d65a36f541935dc91e33ba15d7910a30f9d7',
  referenceNode: '24.22.0',
  hashes,
  resume: resumeCases,
  composition: compositionCases,
};
const text = `${JSON.stringify(output, null, 2)}\n`;
if (writeMode) {
  await writeFile(fixturePath, text);
  process.stdout.write(`wrote ${fixturePath}\n`);
} else {
  const expected = await readFile(fixturePath, 'utf8');
  if (expected !== text) {
    const expectedLines = expected.split('\n');
    const actualLines = text.split('\n');
    const index = actualLines.findIndex((line, i) => line !== expectedLines[i]);
    process.stderr.write(`ext-diff-reference.json differs at line ${index + 1}\n`);
    process.exit(1);
  }
  process.stdout.write('ext-diff-reference.json matches the regenerated reference\n');
}
