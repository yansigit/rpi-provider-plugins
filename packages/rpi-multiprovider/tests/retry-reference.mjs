// Differential reference for the Fabric failover decision layer of
// extensions/multiprovider.ts (upstream bd97d65) and for Pi 1.3.0
// isRetryableAssistantError (dist/utils/retry.js).
//
// The upstream TypeScript and the upstream extension are imported unchanged and
// sha256-asserted. Clock, crypto.randomUUID and the 50 ms idle poll timer are
// injected, so the whole run is deterministic and never touches real time,
// credentials or the network. The extension is driven through a scripted
// ExtensionAPI/ExtensionContext exactly like tests/extension-failover.spec.ts,
// which is why the fixture records the genuine attempts/events the upstream
// lift produces. MultiProviderService.prototype.snapshot is wrapped (not
// modified on disk) so the reference can observe the compaction task entering
// the pool lookup and can script a throwing snapshot.
//
//   node --experimental-transform-types tests/retry-reference.mjs          check
//   node --experimental-transform-types tests/retry-reference.mjs --write  regenerate
import assert from 'node:assert/strict';
import crypto, { createHash } from 'node:crypto';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
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
const fixturePath = join(here, 'retry-reference.json');

const EXPECTED_SHA256 = {
  'extensions/multiprovider.ts': '086a0d9c4fbc8431e81026e5d689747d1c31dfcdf89c9d180c0bbd7e37c0a267',
  'extensions/pool-manager.ts': '67873d40ae349d13d0293c1b92fabaeb9f3b531a7242a0424c721ff3e323cbe8',
  'extensions/settings-ui.ts': 'bbbf4fcd97d6c9e0e62180b15ea20adbd15129a63f243f4431ad192ae4a0f782',
  'src/announcement.ts': 'fc0bf72467dba7b524146ade4a66c420d546a01e7867b8095c8b7d8b1804ae18',
  'src/auth-store.ts': '29916553aa160b3c676ab843891da2b202fadf536fa220980d564b4ed50d48c6',
  'src/errors.ts': '1e4f189fc4aac4488c84912b29bfc7e78629600ead41ce2924c8d8c3a3bac4b7',
  'src/index.ts': '90b3b79432e4d556f75b7b8e25fc3fb512cb56293f5263e6b2090465f5cfaa66',
  'src/lift.ts': '887b91aab533565504449b6094c1a2b076832b0fdc73bfc352e2c46a53ac3695',
  'src/managed.ts': '5a139118f805ed4825259df761dd5e2fbec515da19d3bc10b9719c9bf30bdeef',
  'src/multilogin.ts': 'f6ce501e18b0fb8550fcfbbe77ace3d06cde9bd5b2ad25b29bc8f2e079c590dc',
  'src/register.ts': '946932988b5884081d0f173b464e1b2d42740849803463bdb8fd9841fcc87449',
  'src/service.ts': '2eafa61662b3ee75a96a7b07a6ccad62ca600bbb2139a407b81cd9983564a026',
  'src/session-pins.ts': '45b1ff1fbfcc6853a717b917aaede404f32fe96a64208c75283a53e954bdc005',
  'src/types.ts': 'aaf5e77ffc5738bfc5ee642a61238793ec0869123ceef12a9650c1384b0d181e',
  'src/virtual.ts': '6c38efe1d8bcd180e3287b182df592914a1af0a7091f872cd6dc32042575f808',
  'tests/extension-failover.spec.ts': '66099b172f13b9e8bc8e81ad6c486ab4e6a8922289a8a3519139a92d26aea680',
};
const PI_RETRY_SHA256 = 'cab8af3e38101abc2a7c1c51caa871481c805b3cbe83b9d7267e24857736fc60';

const hashes = {};
for (const file of Object.keys(EXPECTED_SHA256)) {
  const digest = createHash('sha256').update(await readFile(join(root, file))).digest('hex');
  assert.equal(digest, EXPECTED_SHA256[file], `sha256 mismatch for ${file}`);
  hashes[file] = digest;
}
const piRetryPath = join(piAi, 'dist/utils/retry.js');
{
  const digest = createHash('sha256').update(await readFile(piRetryPath)).digest('hex');
  assert.equal(digest, PI_RETRY_SHA256, `sha256 mismatch for ${piRetryPath}`);
  hashes['@earendil-works/pi-ai/dist/utils/retry.js'] = digest;
}

// --- injected clock / ids / idle poll timer ------------------------------
let now = 1_700_000_000_000;
let sequence = 0;
let polls = 0;
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
  polls += 1;
  now += 50;
  queueMicrotask(() => fn(...rest));
  return 0;
};

// --- upstream imports (unchanged, sha256-asserted above) -----------------
const { isRetryableAssistantError } = await import(pathToFileURL(piRetryPath));
const pi = await import(pathToFileURL(join(piAi, 'dist/index.js')));
const { createAssistantMessageEventStream, createProvider, normalizeContext } = pi;
const { MULTIPROVIDER_REGISTER_EVENT, MultiAuthStore, MultiProviderService } = await import(
  pathToFileURL(join(root, 'src/index.ts'))
);
const { default: multiprovider } = await import(pathToFileURL(join(root, 'extensions/multiprovider.ts')));

// --- snapshot instrumentation (observe the pool lookup, script a throw) ---
let snapshotError = null;
const snapshotCalls = { count: 0 };
const originalSnapshot = MultiProviderService.prototype.snapshot;
MultiProviderService.prototype.snapshot = function (...rest) {
  snapshotCalls.count += 1;
  if (snapshotError !== null) return Promise.reject(new Error(snapshotError));
  return originalSnapshot.apply(this, rest);
};

// --- classifier corpus ---------------------------------------------------
const NON_RETRYABLE_SAMPLES = [
  'GoUsageLimitError: usage limit',
  'FreeUsageLimitError',
  'Monthly usage limit reached',
  'please add available balance to continue',
  'insufficient_quota',
  'you are out of budget',
  'quota exceeded',
  'billing hard limit reached',
  'subscription_sharing_usage_limit_exceeded',
];
const RETRYABLE_SAMPLES = [
  'overloaded',
  'server_busy',
  'the servers are currently busy',
  'currently experiencing high demand',
  'model is at capacity',
  'rate limit hit',
  'too many requests',
  'HTTP 429',
  'HTTP 500 upstream',
  'HTTP 502 bad gateway',
  'HTTP 503 upstream',
  'HTTP 504 gateway timeout',
  'HTTP 520',
  'HTTP 524',
  'The service is temporarily unavailable',
  'server error',
  'internal error',
  'Provider returned error',
  'The upstream provider returned an error',
  'exceeded request buffer limit while retrying upstream',
  'unable to forward upstream',
  'network error',
  'connection error',
  'connection refused',
  'connection lost',
  'other side closed',
  'fetch failed',
  'getaddrinfo ENOTFOUND',
  'EAI_AGAIN',
  'upstream connect error',
  'reset before headers',
  'socket hang up',
  'socket connection was closed',
  'request timed out',
  'timeout',
  'terminated',
  'websocket closed',
  'websocket error',
  'stream ended without a terminal event',
  'stream ended before message_stop',
  'stream ended before a terminal response event',
  'http2 request did not get a response',
  'pending stream has been canceled',
  'retry delay exceeded',
  'you can retry your request',
  'try your request again',
  'please retry your request',
  'ResourceExhausted',
  'subscription_sharing_usage_unavailable',
  'subscription_sharing_user_unavailable',
];
const classifierCases = [];
function classifierCase(name, message) {
  classifierCases.push({ name, message, result: isRetryableAssistantError(message) });
}
for (const errorMessage of NON_RETRYABLE_SAMPLES) {
  classifierCase(`non-retryable: ${errorMessage}`, { stopReason: 'error', errorMessage });
}
for (const errorMessage of RETRYABLE_SAMPLES) {
  classifierCase(`retryable: ${errorMessage}`, { stopReason: 'error', errorMessage });
}
classifierCase('success message', { stopReason: 'stop' });
classifierCase('aborted message', { stopReason: 'aborted', errorMessage: 'HTTP 429' });
classifierCase('error without errorMessage', { stopReason: 'error' });
classifierCase('error with empty errorMessage', { stopReason: 'error', errorMessage: '' });
classifierCase('non-error stopReason with retryable text', { stopReason: 'length', errorMessage: 'HTTP 503' });
classifierCase('quota text beats 429', { stopReason: 'error', errorMessage: '429 insufficient_quota' });
classifierCase('quota text beats 503', { stopReason: 'error', errorMessage: '503 quota exceeded' });
classifierCase('billing beats retryable', { stopReason: 'error', errorMessage: 'billing: too many requests' });
classifierCase('uppercase retryable', { stopReason: 'error', errorMessage: 'TOO MANY REQUESTS' });
classifierCase('mixed case retryable', { stopReason: 'error', errorMessage: 'RaTe LiMiT' });
classifierCase('rate with dot filler', { stopReason: 'error', errorMessage: 'rateXlimit' });
classifierCase('rate with no filler', { stopReason: 'error', errorMessage: 'ratelimit' });
classifierCase('rate across a newline does not match the dot', { stopReason: 'error', errorMessage: 'rate\nlimit' });
classifierCase('rate across a carriage return does not match the dot', { stopReason: 'error', errorMessage: 'rate\rlimit' });
classifierCase('rate across a line separator does not match the dot', { stopReason: 'error', errorMessage: 'rate\u2028limit' });
classifierCase('rate across a paragraph separator does not match the dot', { stopReason: 'error', errorMessage: 'rate\u2029limit' });
classifierCase('rate across one non-ASCII character matches the dot', { stopReason: 'error', errorMessage: 'rate\u20aclimit' });
classifierCase('service unavailable variants', { stopReason: 'error', errorMessage: 'service is temporarily unavailable' });
classifierCase('service unavailable short', { stopReason: 'error', errorMessage: 'service unavailable' });
classifierCase('provider returned error with an', { stopReason: 'error', errorMessage: 'provider returned an error' });
classifierCase('internal server error', { stopReason: 'error', errorMessage: 'internal server error' });
classifierCase('400 is not retryable', { stopReason: 'error', errorMessage: 'HTTP 400: bad request' });
classifierCase('401 is not retryable', { stopReason: 'error', errorMessage: 'HTTP 401: invalid token' });
classifierCase('403 is not retryable', { stopReason: 'error', errorMessage: 'HTTP 403: forbidden' });
classifierCase('user cancellation is not retryable', { stopReason: 'error', errorMessage: 'Request cancelled by user' });
classifierCase('context overflow is not retryable', { stopReason: 'error', errorMessage: 'context length exceeded' });
classifierCase('codex usage limit', { stopReason: 'error', errorMessage: 'Codex error: The usage limit has been reached' });
classifierCase('leading and trailing noise', { stopReason: 'error', errorMessage: '  !!HTTP 503!!  ' });
classifierCase('multiline retryable', { stopReason: 'error', errorMessage: 'failed:\nHTTP 503 service unavailable\n' });
classifierCase('number embedded in text', { stopReason: 'error', errorMessage: 'code 15003' });
classifierCase('empty-ish whitespace error', { stopReason: 'error', errorMessage: '   ' });
classifierCase('500 with word boundaries absent', { stopReason: 'error', errorMessage: 'x5000x' });

// --- failover scenarios --------------------------------------------------
const FABRIC_NOTICE = 'multiprovider: compacted session context before account failover';
const accounts = (count, disabled = []) =>
  Array.from({ length: count }, (_, index) => ({
    label: `a${index + 1}`,
    priority: index + 1,
    enabled: !disabled.includes(index + 1),
  }));
const retryable = { stopReason: 'error', errorMessage: 'HTTP 429: too many requests' };
const quota = { stopReason: 'error', errorMessage: 'Codex error: The usage limit has been reached' };
const step = values => ({
  fabric: 'available',
  handled: false,
  failure: retryable,
  accounts: accounts(3),
  exhausted: null,
  visible: false,
  aborted: false,
  idle: { values: [], default: true },
  poolError: null,
  stepGapMs: 0,
  ...values,
});
const idleFor = count => ({ values: Array.from({ length: count }, () => false), default: true });
const scenarios = [
  {
    name: 'quota error rotates inline when fabric is available',
    steps: [step({ failure: quota, exhausted: ['a1', 'a2'], expect: { attempts: ['a1', 'a2', 'a3'], events: ['start', 'done'] } })],
  },
  {
    name: 'quota error surfaces after every account is exhausted',
    steps: [step({ failure: quota, expect: { attempts: ['a1', 'a2', 'a3'], events: ['start', 'error'] } })],
  },
  {
    name: 'authentication failure rotates inline',
    steps: [step({ failure: { stopReason: 'error', errorMessage: 'HTTP 401: invalid token' }, exhausted: ['a1', 'a2'], expect: { attempts: ['a1', 'a2', 'a3'], events: ['start', 'done'] } })],
  },
  {
    name: 'retryable error rotates inline when fabric is absent',
    steps: [step({ fabric: 'absent', exhausted: ['a1', 'a2'], expect: { attempts: ['a1', 'a2', 'a3'], events: ['start', 'done'] } })],
  },
  {
    name: 'retryable error surfaces and compacts when fabric is available',
    steps: [step({ expect: { attempts: ['a1'], events: ['start', 'error'] } })],
  },
  {
    name: 'visible output closes the fabric retry window',
    steps: [step({ visible: true, expect: { attempts: ['a1'], events: ['start', 'text_delta', 'error'] } })],
  },
  {
    name: 'aborted run never compacts',
    steps: [step({ aborted: true, expect: { attempts: ['a1'], events: ['start', 'error'] } })],
  },
  {
    name: 'a throwing tool listing counts as fabric absent',
    steps: [step({ fabric: 'error', exhausted: ['a1', 'a2'], expect: { attempts: ['a1', 'a2', 'a3'], events: ['start', 'done'] } })],
  },
  {
    name: 'a single enabled account skips compaction',
    steps: [step({ accounts: accounts(1), expect: { attempts: ['a1'], events: ['start', 'error'] } })],
  },
  {
    name: 'one disabled account leaves fewer than two enabled',
    steps: [step({ accounts: accounts(2, [2]), expect: { attempts: ['a1'], events: ['start', 'error'] } })],
  },
  {
    name: 'exactly two enabled accounts compacts',
    steps: [step({ accounts: accounts(2), expect: { attempts: ['a1'], events: ['start', 'error'] } })],
  },
  {
    name: 'idle wait ends before compacting',
    steps: [step({ accounts: accounts(2), idle: idleFor(2), expect: { attempts: ['a1'], events: ['start', 'error'] } })],
  },
  {
    name: 'a run that never idles times out after fifteen seconds',
    steps: [step({ accounts: accounts(2), idle: { values: [], default: false }, expect: { attempts: ['a1'], events: ['start', 'error'] } })],
  },
  {
    name: 'debounce suppresses a second compaction inside thirty seconds',
    steps: [
      step({ accounts: accounts(2), expect: { attempts: ['a1'], events: ['start', 'error'] } }),
      step({ accounts: accounts(2), stepGapMs: 10_000, expect: { attempts: ['a1'], events: ['start', 'error'] } }),
    ],
  },
  {
    name: 'debounce suppresses a second compaction at twenty-nine seconds',
    steps: [
      step({ accounts: accounts(2), expect: { attempts: ['a1'], events: ['start', 'error'] } }),
      step({ accounts: accounts(2), stepGapMs: 29_999, expect: { attempts: ['a1'], events: ['start', 'error'] } }),
    ],
  },
  {
    name: 'the debounce window ends at exactly thirty seconds',
    steps: [
      step({ accounts: accounts(2), expect: { attempts: ['a1'], events: ['start', 'error'] } }),
      step({ accounts: accounts(2), stepGapMs: 30_000, expect: { attempts: ['a1'], events: ['start', 'error'] } }),
    ],
  },
  {
    name: 'the idle wait runs after the debounce window has passed',
    steps: [
      step({ accounts: accounts(2), expect: { attempts: ['a1'], events: ['start', 'error'] } }),
      step({ accounts: accounts(2), stepGapMs: 30_000, idle: idleFor(250), expect: { attempts: ['a1'], events: ['start', 'error'] } }),
    ],
  },
  {
    name: 'an empty error message is not retryable',
    steps: [step({ failure: { stopReason: 'error', errorMessage: '' }, expect: { attempts: ['a1'], events: ['start', 'error'] } })],
  },
  {
    name: 'an error message without text is not retryable',
    steps: [step({ failure: { stopReason: 'error', errorMessage: undefined }, expect: { attempts: ['a1'], events: ['start', 'error'] } })],
  },
  {
    name: 'a throwing pool snapshot skips compaction',
    steps: [step({ accounts: accounts(2), poolError: 'snapshot failed', expect: { attempts: ['a1'], events: ['start', 'error'] } })],
  },
  {
    name: 'a compaction error is silent',
    steps: [step({ accounts: accounts(2), compactOutcome: 'error', expect: { attempts: ['a1'], events: ['start', 'error'] } })],
  },
  {
    name: 'an external failover handler claims the transition',
    steps: [step({ external: true, handled: true, failure: quota, expect: { attempts: ['a1'], events: ['start', 'error'] } })],
  },
  {
    name: 'an external failover handler declines and the pool rotates',
    steps: [step({ external: true, handled: false, failure: quota, exhausted: ['a1', 'a2'], expect: { attempts: ['a1', 'a2', 'a3'], events: ['start', 'done'] } })],
  },
  {
    name: 'an external failover handler still wins without fabric',
    steps: [step({ external: true, handled: true, fabric: 'absent', expect: { attempts: ['a1'], events: ['start', 'error'] } })],
  },
  {
    name: 'an external failover handler cannot stop fabric compaction',
    steps: [step({ external: true, handled: true, accounts: accounts(2), expect: { attempts: ['a1'], events: ['start', 'error'] } })],
  },
];

// --- extension harness ---------------------------------------------------
const agentDir = await mkdtemp(join(tmpdir(), 'pi-multiprovider-retry-'));
process.env.PI_CODING_AGENT_DIR = agentDir;
const store = new MultiAuthStore();

let current = null;
const providers = new Map();
const bases = new Map();
const handlers = new Map();
const bus = new Map();
const compactCalls = [];
const notifies = [];
let attempts = [];
let fabricMode = 'available';
let idleScript = { values: [], default: true };

const modelFor = providerId => ({
  id: 'm', name: 'M', api: 'probe-api', provider: providerId,
  baseUrl: 'https://probe.invalid', reasoning: false, input: ['text'],
  cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 }, contextWindow: 1_000, maxTokens: 100,
});
const message = failure => ({
  role: 'assistant', api: 'probe-api', provider: 'probe', model: 'm', content: [],
  timestamp: now, stopReason: failure?.stopReason ?? 'stop',
  ...(failure?.errorMessage === undefined ? {} : { errorMessage: failure.errorMessage }),
  usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0,
    cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } },
});
const stream = (_model, _context, options) => {
  const account = options?.apiKey ?? 'upstream';
  attempts.push(account);
  const exhausted = current.exhausted === null || current.exhausted.includes(account);
  const failure = exhausted ? current.failure : undefined;
  const result = message(failure);
  const events = createAssistantMessageEventStream();
  events.push({ type: 'start', partial: result });
  if (current.visible && exhausted) {
    events.push({ type: 'text_delta', contentIndex: 0, delta: 'visible', partial: result });
  }
  if (current.aborted) {
    result.stopReason = 'aborted';
    events.push({ type: 'error', reason: 'aborted', error: result });
  } else if (exhausted) {
    events.push({ type: 'error', reason: 'error', error: result });
  } else {
    events.push({ type: 'done', reason: 'stop', message: result });
  }
  events.end(result);
  return events;
};
const baseProvider = providerId => createProvider({
  id: providerId, name: `Base ${providerId}`, models: [modelFor(providerId)],
  auth: { apiKey: { name: 'Probe key', async resolve({ credential }) {
    return credential?.key !== undefined ? { auth: { apiKey: credential.key }, source: 'test' } : undefined;
  } } },
  api: { stream, streamSimple: stream },
});
const ensureBase = providerId => {
  if (!bases.has(providerId)) bases.set(providerId, baseProvider(providerId));
  return bases.get(providerId);
};

const ctx = {
  cwd: agentDir, model: modelFor('probe'), mode: 'tui', hasUI: true,
  isIdle: () => idleScript.values.length > 0 ? idleScript.values.shift() : idleScript.default,
  compact: options => {
    compactCalls.push({ at: now });
    if (current?.compactOutcome === 'error') options.onError?.(new Error('compaction failed'));
    else options.onComplete?.();
  },
  ui: { notify: (...values) => notifies.push(values) },
  sessionManager: { getSessionId: () => 'probe-session', getEntries: () => [] },
  modelRegistry: {
    getProvider: providerId => bases.get(providerId),
    getAll: () => [...bases.values()].map(base => modelFor(base.id)),
    getApiKeyAndHeaders: async () => ({ ok: true }),
  },
};
const extensionApi = {
  events: {
    emit(name, value) { for (const listener of bus.get(name) ?? []) listener(value); },
    on(name, listener) {
      const listeners = bus.get(name) ?? new Set();
      listeners.add(listener); bus.set(name, listeners);
      return () => listeners.delete(listener);
    },
  },
  on(name, handler) { handlers.set(name, handler); },
  registerProvider(provider) { providers.set(provider.id, provider); },
  unregisterProvider() {}, registerCommand() {}, appendEntry() {},
  getAllTools: () => {
    if (fabricMode === 'error') throw new Error('tool registry unavailable');
    return fabricMode === 'available' ? [{ name: 'fabric_exec' }] : [];
  },
};
const settle = async () => {
  // The store's snapshot path resolves through real file I/O and its lock
  // pause timer, so drain both macrotask turns and short real timers.
  for (let index = 0; index < 50; index += 1) {
    await new Promise(resolve => setImmediate(resolve));
    await new Promise(resolve => originalTimeout(resolve, 2));
  }
};

// Register every managed pool before the session starts; the extension installs
// a lifted provider per stored pool during session_start.
for (const [index, scenario] of scenarios.entries()) {
  for (const [stepIndex, entry] of scenario.steps.entries()) {
    if (entry.external) continue;
    entry.providerId = `p-${index}-${stepIndex}`;
    for (const account of entry.accounts) {
      await store.addAccount(entry.providerId, {
        label: account.label, credential: { type: 'api_key', key: account.label },
        priority: account.priority, enabled: account.enabled,
        pool: { policy: 'priority', includeUpstream: false },
      });
    }
    ensureBase(entry.providerId);
    const pool = await store.getPool(entry.providerId);
    entry.pool = { enabled: pool.accounts.filter(account => account.enabled).length };
  }
}
await store.updateSchedulerSettings({ errorsBeforeSwitch: 1 });
await multiprovider(extensionApi);
await handlers.get('session_start')({}, ctx);

// External integrations are announced after the session exists; the extension
// installs them synchronously off the event.
for (const [index, scenario] of scenarios.entries()) {
  for (const [stepIndex, entry] of scenario.steps.entries()) {
    if (!entry.external) continue;
    entry.providerId = `ext-${index}-${stepIndex}`;
    ensureBase(entry.providerId);
    const integration = {      id: entry.providerId,
      label: `Ext ${entry.providerId}`,
      accounts: async () => entry.accounts.map(account => ({
        id: account.label, label: account.label, authKind: 'api-key', credentialRef: account.label,
        enabled: account.enabled, priority: account.priority,
      })),
      resolveAuth: async account => ({ auth: { apiKey: account.id } }),
      onFailover: () => entry.handled,
    };
    extensionApi.events.emit(MULTIPROVIDER_REGISTER_EVENT, integration);
    entry.pool = { enabled: entry.accounts.filter(account => account.enabled).length };
  }
}
await settle();

const failoverCases = [];
for (const [scenarioIndex, scenario] of scenarios.entries()) {
  let previousCompactAt = null;
  for (const [stepIndex, entry] of scenario.steps.entries()) {
    // The debounce timestamp is shared by the whole extension, so every
    // scenario after the first starts more than thirty seconds later.
    entry.gapMs = entry.stepGapMs + (stepIndex === 0 && scenarioIndex > 0 ? 30_000 : 0);
    now += entry.gapMs;
    await settle();
    current = entry;
    fabricMode = entry.fabric;
    idleScript = { values: [...entry.idle.values], default: entry.idle.default };
    snapshotError = entry.poolError;
    attempts = [];
    const pollsBefore = polls;
    const snapshotsBefore = snapshotCalls.count;
    const compactsBefore = compactCalls.length;
    const notifiesBefore = notifies.length;
    const events = [];
    const provider = providers.get(entry.providerId);
    assert.ok(provider !== undefined, `provider ${entry.providerId} was installed`);
    for await (const event of provider.streamSimple(modelFor(entry.providerId), normalizeContext({ messages: [] }))) {
      events.push(event.type);
    }
    await settle();
    const observed = {
      attempts,
      events,
      compactCalls: compactCalls.length - compactsBefore,
      notify: notifies.slice(notifiesBefore).filter(value => value[0] === FABRIC_NOTICE),
      accountsQueried: snapshotCalls.count > snapshotsBefore,
      polls: polls - pollsBefore,
    };
    // The decision is only observable through its effects upstream: a claimed
    // transition surfaces the failing account's error after a single attempt.
    // A run that already produced output, or that was aborted, never reaches
    // the failover hook, so the fabric layer claims nothing.
    const failoverReached = !entry.visible && !entry.aborted;
    const expectedHandled = failoverReached
      && (entry.handled || (entry.fabric === 'available' && isRetryableAssistantError(message(entry.failure))));
    assert.deepEqual(observed.attempts, entry.expect.attempts, `${scenario.name}: attempts`);
    assert.deepEqual(observed.events, entry.expect.events, `${scenario.name}: events`);
    if (expectedHandled) {
      assert.equal(observed.attempts.length, 1, `${scenario.name}: claimed failover surfaces after one attempt`);
      assert.equal(observed.events.at(-1), 'error', `${scenario.name}: claimed failover surfaces the buffered error`);
    }
    const compacted = observed.compactCalls > 0;
    if (compacted) {
      if (entry.compactOutcome === 'error') {
        assert.equal(observed.notify.length, 0, `${scenario.name}: a failed compaction is silent`);
      } else {
        assert.equal(observed.notify.length, 1, `${scenario.name}: completion notice`);
        assert.deepEqual(observed.notify[0], [FABRIC_NOTICE, 'info']);
      }
      assert.ok(previousCompactAt === null || now - previousCompactAt >= 30_000,
        `${scenario.name}: compaction respects the debounce`);
      previousCompactAt = now;
    }
    failoverCases.push({
      name: `${scenario.name}${scenario.steps.length > 1 ? ` (step ${stepIndex + 1})` : ''}`,
      input: {
        handled: entry.handled,
        fabric: entry.fabric,
        failure: entry.failure === undefined
          ? null
          : { stopReason: entry.failure.stopReason, ...(entry.failure.errorMessage === undefined ? {} : { errorMessage: entry.failure.errorMessage }) },
        accounts: entry.accounts.map(account => ({ label: account.label, enabled: account.enabled, priority: account.priority })),
        pool: entry.poolError === null ? entry.pool : { enabled: entry.pool.enabled, error: entry.poolError },
        idle: entry.idle,
        gapMs: entry.gapMs,
        exhausted: entry.exhausted,
        visible: entry.visible,
        aborted: entry.aborted,
        compactOutcome: entry.compactOutcome ?? null,
      },
      expected: {
        handled: expectedHandled,
        attempts: observed.attempts,
        events: observed.events,
        compactCalls: observed.compactCalls,
        notify: observed.notify,
        accountsQueried: observed.accountsQueried,
        polls: observed.polls,
      },
    });
  }
}

const output = {
  reference: 'bd97d65a36f541935dc91e33ba15d7910a30f9d7',
  referenceNode: '24.22.0',
  hashes,
  classifier: classifierCases,
  failover: failoverCases,
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
    process.stderr.write(`retry-reference.json differs at line ${index + 1}\n`);
    process.exit(1);
  }
  process.stdout.write('retry-reference.json matches the regenerated reference\n');
}
await rm(agentDir, { recursive: true, force: true });
