// Differential reference for src/register.ts and the announcement.ts service
// surface as exposed over the native service contract (`rpi-multiprovider`
// service version 1).
//
// The upstream TypeScript is imported unchanged and sha256-asserted. Clock and
// crypto are injected. Registration expectations come from the real
// `registerMultiProvider` emits; active-account/auth/notification expectations
// come from the real `createServiceAnnouncement`, exactly as the upstream
// announcement spec drives it. Native-only publications (topic envelopes) are
// derived from those upstream values, never from re-implemented logic.
//
//   node --experimental-transform-types tests/m_native_svc-reference.mjs          check
//   node --experimental-transform-types tests/m_native_svc-reference.mjs --write  regenerate
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFile, writeFile } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const args = process.argv.slice(2);
const writeMode = args.includes('--write');
const rootArg = args.find(arg => !arg.startsWith('--'));
const root = resolve(rootArg ?? join(here, '../../../../provider-references/multiprovider'));
const fixturePath = join(here, 'm_native_svc-reference.json');

const EXPECTED_SHA256 = {
  'src/announcement.ts': 'fc0bf72467dba7b524146ade4a66c420d546a01e7867b8095c8b7d8b1804ae18',
  'src/register.ts': '946932988b5884081d0f173b464e1b2d42740849803463bdb8fd9841fcc87449',
  'src/service.ts': '2eafa61662b3ee75a96a7b07a6ccad62ca600bbb2139a407b81cd9983564a026',
  'src/types.ts': 'aaf5e77ffc5738bfc5ee642a61238793ec0869123ceef12a9650c1384b0d181e',
  'tests/announcement.spec.ts': 'eb7d75aba8b3fd10edd175cb8a9262bd3ed280f1a71a476c44eb356ad55cde1a',
  'tests/extension-resume.spec.ts': 'fb768939799048972fbb4d805fb110a400435c3ce000325cea8df54ced18caac',
};
const hashes = {};
for (const file of Object.keys(EXPECTED_SHA256)) {
  const digest = createHash('sha256').update(await readFile(join(root, file))).digest('hex');
  assert.equal(digest, EXPECTED_SHA256[file], `${file} differs from the pinned upstream`);
  hashes[file] = digest;
}

// Injected clock and identifiers.
const DEFAULT_NOW = 1700000000000;
const now = DEFAULT_NOW;
const RealDate = Date;
class Clock extends RealDate {
  constructor(...values) { super(...(values.length ? values : [now])); }
  static now() { return now; }
}
globalThis.Date = Clock;

const { createServiceAnnouncement } = await import(
  pathToFileURL(join(root, 'src/announcement.ts')).href
);
const { registerMultiProvider } = await import(pathToFileURL(join(root, 'src/register.ts')).href);
const { MultiProviderService } = await import(pathToFileURL(join(root, 'src/service.ts')).href);
const types = await import(pathToFileURL(join(root, 'src/types.ts')).href);
const registerModule = await import(pathToFileURL(join(root, 'src/register.ts')).href);

const plain = value => (value === undefined ? null : JSON.parse(JSON.stringify(value)));

const SERVICE_ID = 'pi-multiprovider';
const SERVICE_VERSION = 1;
const SERVICE_EVENT = 'pi-multiprovider:service';
const SPEC_ACCOUNTS = [
  { id: 'pi:default', label: 'Pi default', authKind: 'custom', credentialRef: 'pi:default' },
  { id: 'a', label: 'Work', authKind: 'oauth', credentialRef: 'ref-a' },
  { id: 'b', label: 'Personal', authKind: 'oauth', credentialRef: 'ref-b' },
];
const SPEC_RESOLVE = {
  a: { value: { auth: { apiKey: 'token-a' }, source: 'Work · Test OAuth' } },
  b: { throw: 'resolve failed' },
};
const ACTIVE_A = { id: 'a', label: 'Work', authKind: 'oauth' };

// ---------------------------------------------------------------------------
// register.ts: announce at registration and on every session start.
// ---------------------------------------------------------------------------

// The serializable projection the native service publishes for an integration
// (ProviderDescriptor). Only fields the upstream integration actually carries.
const project = value => ({
  id: value.id,
  label: value.label,
  ...(value.managementHint === undefined ? {} : { managementHint: value.managementHint }),
  ...(value.selectionBias === undefined ? {} : { selectionBias: value.selectionBias }),
});

function runRegistrationCase(scenario) {
  assert.equal(types.MULTIPROVIDER_REGISTER_EVENT, 'pi-multiprovider:register');
  assert.equal(types.MULTIPROVIDER_SERVICE_EVENT, SERVICE_EVENT);
  assert.equal(typeof registerMultiProvider, 'function');
  assert.equal(registerModule.registerMultiProvider, registerMultiProvider);

  const emissions = [];
  const handlers = [];
  const pi = {
    events: {
      emit(event, value) { emissions.push({ event, value }); },
      on() { return () => {}; },
    },
    on(event, handler) {
      if (event === 'session_start') handlers.push(handler);
      return () => {};
    },
  };
  const integrations = [];
  for (const registration of scenario.registrations) {
    const integration = {
      id: registration.id,
      label: registration.label,
      accounts: () => [],
      resolveAuth: async () => ({ auth: {} }),
      ...(registration.managementHint === undefined
        ? {}
        : { managementHint: registration.managementHint }),
      ...(registration.selectionBias === undefined
        ? {}
        : { selectionBias: registration.selectionBias }),
    };
    integrations.push(integration);
    registerMultiProvider(pi, integration);
  }
  assert.equal(handlers.length, scenario.registrations.length);
  for (let index = 0; index < scenario.sessionStarts; index += 1) {
    for (const handler of handlers) handler();
  }

  // Every emit must be the exact integration object, at the documented event.
  for (const emission of emissions) {
    assert.equal(emission.event, types.MULTIPROVIDER_REGISTER_EVENT);
    assert(integrations.includes(emission.value));
  }
  const publications = emissions.map(emission => ({
    topic: 'providerRegistered',
    value: project(emission.value),
  }));
  // Consumer-visible map: last value per id at the first registration position.
  const inventory = [];
  const positions = new Map();
  for (const integration of integrations) {
    const descriptor = project(integration);
    if (positions.has(descriptor.id)) inventory[positions.get(descriptor.id)] = descriptor;
    else {
      positions.set(descriptor.id, inventory.length);
      inventory.push(descriptor);
    }
  }
  return {
    name: scenario.name,
    registrations: scenario.registrations,
    sessionStarts: scenario.sessionStarts,
    publications,
    inventory,
  };
}

// ---------------------------------------------------------------------------
// Service cases: real scheduler + announcement, driven through the native
// method/topic surface. `call` inputs mirror the host envelope's `input`.
// ---------------------------------------------------------------------------

const registrationCases = [
  {
    name: 'register.ts announces each integration at registration and on every session start',
    registrations: [
      { id: 'alpha', label: 'Alpha' },
      { id: 'beta', label: 'Beta', managementHint: 'Manage beta', selectionBias: 'none' },
    ],
    sessionStarts: 2,
  },
  {
    name: 'a repeated id is announced again and keeps its first inventory position',
    registrations: [
      { id: 'alpha', label: 'One' },
      { id: 'beta', label: 'Beta' },
      { id: 'alpha', label: 'Two' },
    ],
    sessionStarts: 1,
  },
  {
    name: 'no integrations announce nothing',
    registrations: [],
    sessionStarts: 1,
  },
];

const call = (method, input = {}) => ({ op: 'call', method, input });
const pin = (account, key) => ({ op: 'pin', account, ...(key === undefined ? {} : { key }) });
const subscribe = (handle, listener, provider) => ({
  op: 'subscribe',
  handle,
  listener,
  ...(provider === undefined ? {} : { provider }),
});
const unsubscribe = handle => ({ op: 'unsubscribe', handle });
const notify = (account, provider) => ({
  op: 'notify',
  account,
  ...(provider === undefined ? {} : { provider }),
});
const setResolve = (accountId, entry) => ({ op: 'setResolve', accountId, entry });
const scriptedIntegration = (overrides = {}) => ({
  accounts: SPEC_ACCOUNTS,
  resolve: SPEC_RESOLVE,
  ...overrides,
});

const serviceCases = [
  {
    name: 'spec: explicit pin resolves its stored credential',
    affinity: false,
    integration: scriptedIntegration(),
    registrations: [{ id: 'example', label: 'Example' }],
    steps: [
      call('getActiveAccount', { providerId: 'example' }),
      pin('a'),
      call('getActiveAccount', { providerId: 'example' }),
      call('resolveActiveAccountAuth', { providerId: 'example' }),
    ],
  },
  {
    name: 'spec: last scheduler selection is reported while affinity is on',
    selectionBias: 'none',
    integration: scriptedIntegration(),
    registrations: [{ id: 'example', label: 'Example' }],
    steps: [
      { op: 'acquire', key: 'session-1' },
      call('getActiveAccount', { providerId: 'example' }),
    ],
  },
  {
    name: 'spec: upstream and unpinned sessions never resolve credentials',
    integration: scriptedIntegration(),
    registrations: [{ id: 'example', label: 'Example' }],
    steps: [
      call('resolveActiveAccountAuth', { providerId: 'example' }),
      pin('pi:default'),
      call('getActiveAccount', { providerId: 'example' }),
      call('resolveActiveAccountAuth', { providerId: 'example' }),
    ],
  },
  {
    name: 'spec: bearer headers and resolver failures',
    integration: scriptedIntegration({
      resolve: {
        a: { value: { auth: { headers: { Authorization: 'Bearer header-token' } } } },
        b: { throw: 'resolve failed' },
      },
    }),
    registrations: [{ id: 'example', label: 'Example' }],
    steps: [
      pin('a'),
      call('resolveActiveAccountAuth', { providerId: 'example' }),
      pin('b'),
      call('resolveActiveAccountAuth', { providerId: 'example' }),
      setResolve('a', { value: { auth: {} } }),
      call('resolveActiveAccountAuth', { providerId: 'example' }),
    ],
  },
  {
    name: 'spec: listeners are per provider and unsubscribe',
    integration: scriptedIntegration(),
    registrations: [{ id: 'example', label: 'Example' }],
    listeners: { L1: {} },
    steps: [
      subscribe('h1', 'L1'),
      notify(ACTIVE_A),
      notify(undefined),
      notify(ACTIVE_A, 'other'),
      unsubscribe('h1'),
      notify(ACTIVE_A),
    ],
  },
  {
    name: 'spec: missing integration returns nothing',
    integration: scriptedIntegration(),
    registrations: [{ id: 'example', label: 'Example' }],
    steps: [
      call('getActiveAccount', { providerId: 'missing' }),
      call('resolveActiveAccountAuth', { providerId: 'missing' }),
      call('accounts', { providerId: 'missing' }),
    ],
  },
  {
    name: 'a throwing listener does not stop its siblings',
    integration: scriptedIntegration(),
    registrations: [{ id: 'example', label: 'Example' }],
    listeners: { L1: { throws: 'listener boom' }, L2: {} },
    steps: [
      subscribe('h1', 'L1'),
      subscribe('h2', 'L2'),
      notify(ACTIVE_A),
      notify(undefined),
    ],
  },
  {
    name: 'signal state crosses the service call into the integration',
    integration: scriptedIntegration(),
    registrations: [{ id: 'example', label: 'Example' }],
    steps: [
      pin('a'),
      { ...call('resolveActiveAccountAuth', { providerId: 'example' }), signal: 'live' },
      { ...call('resolveActiveAccountAuth', { providerId: 'example' }), signal: 'aborted' },
      call('resolveActiveAccountAuth', { providerId: 'example' }),
    ],
  },
  {
    name: 'account inventory is whitelisted and ordered like the integration',
    integration: scriptedIntegration(),
    registrations: [{ id: 'example', label: 'Example' }],
    steps: [
      call('accounts', { providerId: 'example' }),
      call('providers'),
    ],
  },
  {
    name: 'account inventory failures propagate to the caller',
    integration: scriptedIntegration(),
    registrations: [{ id: 'example', label: 'Example' }],
    steps: [
      { op: 'setAccountsError', message: 'accounts down' },
      call('accounts', { providerId: 'example' }),
    ],
  },
];

async function runServiceCase(scenario) {
  const trace = [];
  const providerSpec = scenario.provider ?? { id: 'example', name: 'Example', models: [{ id: 'model' }] };
  const provider = {
    id: providerSpec.id,
    name: providerSpec.name,
    getModels: () => providerSpec.models.map(model => ({ ...model, provider: providerSpec.id })),
  };
  const scripted = {
    accounts: structuredClone(scenario.integration.accounts),
    resolve: structuredClone(scenario.integration.resolve),
    accountsError: undefined,
  };
  const integration = {
    id: 'example',
    label: 'Example',
    accounts: () => {
      if (scripted.accountsError !== undefined) throw new Error(scripted.accountsError);
      return structuredClone(scripted.accounts);
    },
    resolveAuth: async (account, signal, request) => {
      trace.push({
        integration: 'resolveAuth',
        account: account.id,
        aborted: signal.aborted,
        model: request.model.id,
        context: request.context,
        requestOptions: request.requestOptions,
      });
      if (!Object.hasOwn(scripted.resolve, account.id)) {
        throw new Error(`unscripted resolve ${account.id}`);
      }
      const entry = scripted.resolve[account.id];
      if (entry.throw !== undefined) throw new Error(entry.throw);
      return structuredClone(entry.value);
    },
  };
  const scheduler = new MultiProviderService({
    ...(scenario.affinity === undefined ? {} : { affinity: scenario.affinity }),
    randomInt: () => 1,
    randomId: () => 'lease-1',
  });
  scheduler.registerProvider({
    id: 'example',
    label: 'Example',
    accounts: () => integration.accounts(),
    ...(scenario.selectionBias === undefined ? {} : { selectionBias: scenario.selectionBias }),
  });
  const ctx = {
    model: scenario.ctxModel === undefined ? undefined : { id: scenario.ctxModel },
    sessionManager: { getSessionId: () => 'session-1' },
    modelRegistry: { getProvider: () => provider },
  };
  const announcement = createServiceAnnouncement({
    scheduler,
    getIntegration: providerId => (providerId === 'example' ? integration : undefined),
    getBaseProvider: providerId => (providerId === 'example' ? provider : undefined),
    affinityKeyFor(_integration, _ctx, providerId) {
      trace.push({ affinityKeyFor: providerId });
      if (scenario.affinityKeyError !== undefined) throw new Error(scenario.affinityKeyError);
      return 'session-1';
    },
  });

  // Native service state: registration inventory plus the observed publications.
  const inventory = [];
  const publications = [];
  const publish = (topic, value) => {
    publications.push({ topic, value });
    return publications.length;
  };
  for (const registration of scenario.registrations ?? []) {
    inventory.push(project(registration));
    publish('providerRegistered', project(registration));
  }

  const listeners = new Map();
  const handles = new Map();
  const listenerFor = name => {
    if (!listeners.has(name)) {
      const definition = scenario.listeners?.[name] ?? {};
      listeners.set(name, event => {
        trace.push({
          listener: name,
          providerId: event.providerId,
          account: event.account === undefined ? null : event.account,
          ctxSessionId: event.ctx.sessionManager.getSessionId(),
        });
        for (const action of definition.calls ?? []) {
          if (action.op === 'subscribe') {
            handles.set(
              action.handle,
              announcement.onActiveAccountChanged('example', listenerFor(action.listener)),
            );
          } else {
            handles.get(action.handle)();
          }
        }
        if (definition.throws !== undefined) throw new Error(definition.throws);
      });
    }
    return listeners.get(name);
  };

  // Native method dispatch: same inputs the Rust service receives, same values
  // the real announcement returns. Unknown providers answer null, exactly like
  // the in-process contract.
  const dispatch = async (method, input, signal) => {
    switch (method) {
      case 'providers':
        return { providers: structuredClone(inventory) };
      case 'accounts': {
        const providerId = input.providerId;
        if (providerId !== 'example') return null;
        return {
          providerId,
          accounts: (await integration.accounts()).map(account => ({
            id: account.id,
            label: account.label,
            authKind: account.authKind,
          })),
        };
      }
      case 'getActiveAccount':
        return plain(await announcement.getActiveAccount(input.providerId, ctx));
      case 'resolveActiveAccountAuth':
        return plain(await announcement.resolveActiveAccountAuth(input.providerId, ctx, signal));
      default:
        throw new Error(`unknown service method "${method}"`);
    }
  };
  const signalFor = action => {
    if (action.signal === undefined || action.signal === 'none') return undefined;
    if (action.signal === 'aborted') return AbortSignal.abort();
    return new AbortController().signal;
  };

  const results = [];
  for (const step of scenario.steps) {
    const before = trace.length;
    let outcome;
    let payload;
    try {
      switch (step.op) {
        case 'pin':
          payload = await scheduler.pinAccount('example', step.key ?? 'session-1', step.account);
          break;
        case 'acquire': {
          const lease = await scheduler.acquire({
            providerId: 'example',
            affinityKey: step.key ?? 'session-1',
          });
          payload = lease.accountId;
          lease.release({ status: 'success' });
          break;
        }
        case 'setAccounts':
          scripted.accounts = structuredClone(step.accounts);
          payload = null;
          break;
        case 'setAccountsError':
          scripted.accountsError = step.message === null ? undefined : step.message;
          payload = null;
          break;
        case 'setResolve':
          scripted.resolve[step.accountId] = structuredClone(step.entry);
          payload = null;
          break;
        case 'subscribe':
          handles.set(
            step.handle,
            announcement.onActiveAccountChanged(step.provider ?? 'example', listenerFor(step.listener)),
          );
          payload = null;
          break;
        case 'unsubscribe':
          handles.get(step.handle)();
          payload = null;
          break;
        case 'notify': {
          const providerId = step.provider ?? 'example';
          announcement.notifyActiveAccountChanged(
            providerId,
            ctx,
            step.account === null ? undefined : step.account,
          );
          publish('activeAccountChanged', {
            providerId,
            account: step.account ?? null,
          });
          payload = null;
          break;
        }
        case 'call': {
          const input = step.input ?? {};
          const value = await dispatch(step.method, input, signalFor(step));
          payload = { response: { version: 1, result: value } };
          break;
        }
        default:
          throw new Error(`unknown step ${step.op}`);
      }
      outcome =
        step.op === 'call'
          ? { op: step.op, method: step.method, result: payload.response.result }
          : { op: step.op, result: plain(payload) };
    } catch (error) {
      outcome =
        step.op === 'call'
          ? { op: step.op, method: step.method, error: error.message }
          : { op: step.op, error: error.message };
    }
    outcome.trace = plain(trace.slice(before));
    results.push(outcome);
  }
  return {
    name: scenario.name,
    affinity: scenario.affinity,
    selectionBias: scenario.selectionBias,
    integration: scenario.integration,
    registrations: (scenario.registrations ?? []).map(project),
    steps: scenario.steps,
    results,
    publications,
  };
}

const cases = { registration: [], service: [] };
for (const scenario of registrationCases) cases.registration.push(runRegistrationCase(scenario));
for (const scenario of serviceCases) cases.service.push(await runServiceCase(scenario));

const output = {
  reference: 'bd97d65a36f541935dc91e33ba15d7910a30f9d7',
  service: { id: SERVICE_ID, serviceVersion: SERVICE_VERSION },
  hashes,
  cases,
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
    process.stderr.write(
      `fixture differs at line ${index + 1}:\n  expected: ${expectedLines[index]}\n  actual:   ${actualLines[index]}\n`,
    );
    process.exit(1);
  }
  process.stdout.write(
    `m_native_svc reference ok: ${cases.registration.length} registration, ${cases.service.length} service cases\n`,
  );
}
