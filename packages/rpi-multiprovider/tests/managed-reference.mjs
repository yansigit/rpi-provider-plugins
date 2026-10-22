// Differential reference for src/managed.ts, src/announcement.ts and the
// resolveAccount decision logic in src/auth-store.ts (upstream bd97d65).
//
// The upstream TypeScript is imported unchanged and sha256-asserted. Clock and
// crypto are injected. Store file/lock events are observed through the real
// node:fs/promises calls the upstream store makes, so the trace records the
// genuine lock, read, write and unlock order.
//
//   node --experimental-transform-types tests/managed-reference.mjs          check
//   node --experimental-transform-types tests/managed-reference.mjs --write  regenerate
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import crypto from 'node:crypto';
import { createRequire, syncBuiltinESMExports } from 'node:module';
import { existsSync } from 'node:fs';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const args = process.argv.slice(2);
const writeMode = args.includes('--write');
const rootArg = args.find(arg => !arg.startsWith('--'));
const root = resolve(rootArg ?? join(here, '../../../../provider-references/multiprovider'));
const fixturePath = join(here, 'managed-reference.json');

const EXPECTED_SHA256 = {
  'src/managed.ts': '5a139118f805ed4825259df761dd5e2fbec515da19d3bc10b9719c9bf30bdeef',
  'src/announcement.ts': 'fc0bf72467dba7b524146ade4a66c420d546a01e7867b8095c8b7d8b1804ae18',
  'src/auth-store.ts': '29916553aa160b3c676ab843891da2b202fadf536fa220980d564b4ed50d48c6',
  'src/service.ts': '2eafa61662b3ee75a96a7b07a6ccad62ca600bbb2139a407b81cd9983564a026',
  'src/errors.ts': '1e4f189fc4aac4488c84912b29bfc7e78629600ead41ce2924c8d8c3a3bac4b7',
  'src/types.ts': 'aaf5e77ffc5738bfc5ee642a61238793ec0869123ceef12a9650c1384b0d181e',
  'tests/managed.spec.ts': '26d01957484d177282c22ae942158cc4c7e7c5574f7386a34ea51e4730439307',
  'tests/announcement.spec.ts': 'eb7d75aba8b3fd10edd175cb8a9262bd3ed280f1a71a476c44eb356ad55cde1a',
};
const hashes = {};
for (const file of Object.keys(EXPECTED_SHA256)) {
  const digest = createHash('sha256').update(await readFile(join(root, file))).digest('hex');
  assert.equal(digest, EXPECTED_SHA256[file], `${file} differs from the pinned upstream`);
  hashes[file] = digest;
}

// Injected clock and identifiers.
const DEFAULT_NOW = 1700000000000;
let now = DEFAULT_NOW;
const RealDate = Date;
class Clock extends RealDate {
  constructor(...args) { super(...(args.length ? args : [now])); }
  static now() { return now; }
}
globalThis.Date = Clock;
crypto.randomUUID = () => 'fixture-uuid';

// Observe the store file and its lock through the upstream fs calls.
let probe = null;
const emit = event => { if (probe !== null) probe.trace.push(event); };
const fsp = createRequire(import.meta.url)('node:fs/promises');
const original = { open: fsp.open, readFile: fsp.readFile, rename: fsp.rename, unlink: fsp.unlink };
fsp.open = async function open(path, flags, ...rest) {
  const handle = await original.open.call(fsp, path, flags, ...rest);
  if (probe !== null && path === probe.lockPath && flags === 'wx') emit({ store: 'lock' });
  return handle;
};
fsp.readFile = async function readStore(path, ...rest) {
  if (probe !== null && path === probe.storePath) emit({ store: 'read' });
  return original.readFile.call(fsp, path, ...rest);
};
fsp.rename = async function renameStore(from, to) {
  const result = await original.rename.call(fsp, from, to);
  if (probe !== null && to === probe.storePath) emit({ store: 'write' });
  return result;
};
fsp.unlink = async function unlinkLock(path) {
  const result = await original.unlink.call(fsp, path);
  if (probe !== null && path === probe.lockPath) emit({ store: 'unlock' });
  return result;
};
syncBuiltinESMExports();

const managed = await import(pathToFileURL(join(root, 'src/managed.ts')).href);
const { MultiAuthStore } = await import(pathToFileURL(join(root, 'src/auth-store.ts')).href);
const { createServiceAnnouncement } = await import(pathToFileURL(join(root, 'src/announcement.ts')).href);
const { MultiProviderService } = await import(pathToFileURL(join(root, 'src/service.ts')).href);

const plain = value => (value === undefined ? null : JSON.parse(JSON.stringify(value)));

// Native provider scripted from the scenario. Each response is keyed by the
// credential field the native method would receive, so fixtures stay readable.
function respond(responses, kind, key) {
  const table = responses?.[kind] ?? {};
  if (!Object.hasOwn(table, key)) throw new Error(`unscripted ${kind} ${key}`);
  const entry = table[key];
  if (entry.throw !== undefined) throw new Error(entry.throw);
  if (entry.none === true) return undefined;
  return structuredClone(entry.value);
}
const keyOf = input => input.credential?.key ?? '<none>';
const credentialEvent = input => ({
  ...(input.credential === undefined ? {} : { credential: plain(input.credential) }),
  aborted: input.signal.aborted,
});
function makeProvider(spec, trace) {
  const models = (spec.models ?? []).map(model => ({
    id: model.id,
    name: model.id,
    provider: spec.id,
    api: 'test-api',
    ...(model.baseUrl === undefined ? {} : { baseUrl: model.baseUrl }),
  }));
  const auth = {};
  if (spec.apiKey !== undefined) {
    const native = spec.apiKey;
    auth.apiKey = {
      name: native.name,
      ...(native.login === true ? { login: async () => { throw new Error('login not scripted'); } } : {}),
      ...(native.check === true ? {
        check: async input => {
          trace.push({ native: 'apiKey.check', ...credentialEvent(input) });
          return respond(native.responses, 'check', keyOf(input));
        },
      } : {}),
      resolve: async input => {
        trace.push({ native: 'apiKey.resolve', ...credentialEvent(input) });
        return respond(native.responses, 'resolve', keyOf(input));
      },
    };
  }
  if (spec.oauth !== undefined) {
    const native = spec.oauth;
    auth.oauth = {
      name: native.name,
      ...(native.isSubscription === undefined ? {} : { isSubscription: native.isSubscription }),
      ...(native.loginLabel === undefined ? {} : { loginLabel: native.loginLabel }),
      login: async () => { throw new Error('login not scripted'); },
      refresh: async (credential, signal) => {
        trace.push({ native: 'oauth.refresh', credential: plain(credential), aborted: signal.aborted });
        return respond(native.responses, 'refresh', credential.refresh);
      },
      toAuth: async credential => {
        trace.push({ native: 'oauth.toAuth', credential: plain(credential) });
        return respond(native.responses, 'toAuth', credential.access);
      },
    };
  }
  return {
    id: spec.id,
    name: spec.name,
    ...(spec.baseUrl === undefined ? {} : { baseUrl: spec.baseUrl }),
    auth,
    getModels: () => models,
  };
}

// Fixture helpers. Times are fixed so updatedAt is deterministic.
const T = '2023-11-14T22:13:20.000Z';
const storedAccount = (id, label, credential, extra = {}) => ({
  id,
  label,
  authKind: credential.type === 'oauth' ? 'oauth' : 'api-key',
  enabled: true,
  weight: 1,
  priority: 0,
  createdAt: T,
  updatedAt: T,
  credential,
  ...extra,
});
const storedState = (accounts, pool = {}) => ({
  version: 1,
  providers: { example: { policy: 'round-robin', affinity: false, includeUpstream: true, ...pool, accounts } },
});
const apiKeyCredential = (key, extra = {}) => ({ type: 'api_key', key, ...extra });
const oauthCredential = (access, refresh, expires, extra = {}) => ({ type: 'oauth', access, refresh, expires, ...extra });
const SECRETS = ['upstream-key', 'extra-key', 'refresh-old', 'access-old', 'sk-secret-extra'];

const RESOLVE = {
  'upstream-key': { value: { auth: { apiKey: 'upstream-key', baseUrl: 'https://upstream.invalid', headers: { Authorization: 'Bearer upstream-key', 'x-upstream-only': 'upstream' } }, env: { UPSTREAM_ONLY: 'upstream' }, source: 'Pi credential' } },
  'extra-key': { value: { auth: { apiKey: 'extra-key', headers: { 'x-extra': 'extra' } }, env: { EXTRA_ONLY: 'extra' }, source: 'Stored extra credential' } },
  'sk-secret-extra': { value: { auth: { apiKey: 'sk-secret-extra' } } },
  'header-key': { value: { auth: { headers: { Authorization: 'Bearer header-token' } }, source: 'Header' } },
  '<none>': { none: true },
  'missing-key': { none: true },
  'custom-base-key': { value: { auth: { apiKey: 'custom', baseUrl: 'https://own.invalid' }, source: 'Own base' } },
  'expired-upstream': { throw: 'upstream credential expired' },
  'bare-key': { value: { auth: { apiKey: 'bare' } } },
  'no-base-key': { value: { auth: { apiKey: 'no-base' } } },
};
const CHECK = {
  'upstream-key': { value: { type: 'api_key', source: 'Checked upstream' } },
  '<none>': { none: true },
  'missing-key': { none: true },
  'check-throw': { throw: 'check failed' },
};
const OAUTH = {
  refresh: {
    'refresh-old': { value: oauthCredential('access-new', 'refresh-new', 1700003600000, { account: 'rotated' }) },
    'refresh-fail': { throw: 'invalid_grant' },
    'refresh-api': { value: { type: 'api_key', key: 'not-oauth' } },
    'refresh-malformed': { value: 'not-an-object' },
    'refresh-no-type': { value: { access: 'missing-type' } },
    'refresh-bad-type': { value: { type: 'token' } },
    'refresh-boundary': { value: oauthCredential('access-boundary', 'refresh-boundary', 1700003600000) },
  },
  toAuth: {
    'access-new': { value: { apiKey: 'access-new', baseUrl: 'https://oauth.invalid', headers: { 'x-account': 'rotated' } } },
    'access-fresh': { value: { headers: { Authorization: 'Bearer access-fresh' } } },
    'access-above': { value: { apiKey: 'access-above' } },
    'access-boundary': { value: { apiKey: 'access-boundary' } },
    'access-throw': { throw: 'toAuth unavailable' },
    'access-work': { value: { apiKey: 'access-work' } },
    'access-old-after': { value: { apiKey: 'access-old-after' } },
  },
};
const PROVIDER = {
  id: 'example',
  name: 'Example Provider',
  baseUrl: 'https://provider.invalid',
  models: [{ id: 'same-model', baseUrl: 'https://model.invalid' }, { id: 'plain-model' }],
  apiKey: { name: 'Example API key', login: true, check: true, responses: { resolve: RESOLVE, check: CHECK } },
  oauth: { name: 'Example OAuth', isSubscription: true, loginLabel: 'Sign in', responses: OAUTH },
};
const withProvider = (overrides = {}) => ({ ...PROVIDER, ...overrides });
const apiKeyOnly = { ...PROVIDER, oauth: undefined };
const oauthOnly = { ...PROVIDER, apiKey: undefined };

const lookupAccount = accountId => ({ op: 'lookup', accountId });
const merged = (name, extra = {}) => ({ op: 'merged', name, ...extra });
const resolveAuth = (accountId, extra = {}) => ({ op: 'resolveAuth', accountId, ...extra });
const sanitize = (accountId, extra = {}) => ({ op: 'sanitize', accountId, ...extra });
const excludeIds = requestOptions => ({ op: 'excludeAccountIds', requestOptions });
const marker = (index, path = ['auth', 'headers', 'x-pi-multiprovider-auth-source']) => ({ $ref: index, path });
const MARKER = 'x-pi-multiprovider-auth-source';
const cred = (credential) => ({ credential });

const managedCases = [
  {
    name: 'pi default listed before stored accounts',
    state: storedState([storedAccount('acct-extra', 'Extra', apiKeyCredential('extra-key'))]),
    provider: PROVIDER,
    actions: [{ op: 'accounts' }, { op: 'hasAccounts' }, { op: 'pool' }, { op: 'shape' }],
    secrets: SECRETS,
  },
  {
    name: 'stored upstream preferences apply to the listing',
    state: storedState([storedAccount('acct-extra', 'Extra', apiKeyCredential('extra-key'))], { upstream: { label: 'Team account', weight: 5, priority: 3 } }),
    provider: PROVIDER,
    actions: [{ op: 'accounts' }],
  },
  {
    name: 'empty upstream preferences fall back to defaults',
    state: storedState([storedAccount('acct-extra', 'Extra', apiKeyCredential('extra-key'))], { upstream: {} }),
    provider: PROVIDER,
    actions: [{ op: 'accounts' }],
  },
  {
    name: 'raw upstream values are used without normalisation',
    state: storedState([], { upstream: { label: '', weight: 0, priority: -2 } }),
    provider: PROVIDER,
    actions: [{ op: 'accounts' }, { op: 'hasAccounts' }],
  },
  {
    name: 'upstream account hidden when includeUpstream is off',
    state: storedState([storedAccount('acct-extra', 'Extra', apiKeyCredential('extra-key'))], { includeUpstream: false }),
    provider: PROVIDER,
    actions: [{ op: 'accounts' }],
  },
  {
    name: 'missing includeUpstream field hides the upstream account',
    state: { version: 1, providers: { example: { policy: 'round-robin', affinity: false, accounts: [] } } },
    provider: PROVIDER,
    actions: [{ op: 'accounts' }, { op: 'hasAccounts' }],
  },
  {
    name: 'missing pool lists nothing',
    state: { version: 1, providers: {} },
    provider: PROVIDER,
    actions: [{ op: 'accounts' }, { op: 'hasAccounts' }, { op: 'pool' }, { op: 'merged', name: 'apiKey.resolve', credential: undefined }],
  },
  {
    name: 'disabled stored account is listed but unavailable',
    state: storedState([
      storedAccount('acct-extra', 'Extra', apiKeyCredential('extra-key')),
      storedAccount('acct-off', 'Off', apiKeyCredential('extra-key'), { enabled: false }),
    ]),
    provider: PROVIDER,
    actions: [
      { op: 'accounts' },
      { op: 'hasAccounts' },
      { op: 'resolveAccount', accountId: 'acct-off' },
      { op: 'resolveAccount', accountId: 'acct-extra' },
    ],
  },
  {
    name: 'only disabled accounts do not count as stored accounts',
    state: storedState([storedAccount('acct-off', 'Off', apiKeyCredential('extra-key'), { enabled: false })]),
    provider: PROVIDER,
    actions: [
      { op: 'hasAccounts' },
      merged('apiKey.check'),
      merged('apiKey.resolve'),
      { op: 'resolveAccount', accountId: 'acct-off' },
    ],
  },
  {
    name: 'duplicate labels stay distinct accounts',
    state: storedState([
      storedAccount('acct-one', 'Work', apiKeyCredential('extra-key')),
      storedAccount('acct-two', 'Work', apiKeyCredential('sk-secret-extra')),
    ]),
    provider: PROVIDER,
    actions: [
      { op: 'accounts' },
      { op: 'resolveAccount', accountId: 'acct-one' },
      { op: 'resolveAccount', accountId: 'acct-two' },
    ],
    secrets: SECRETS,
  },
  {
    name: 'public snapshots omit stored credentials',
    state: storedState([
      storedAccount('acct-oauth', 'OAuth', oauthCredential('access-old', 'refresh-old', 1700003600000)),
    ], { upstream: { label: 'Pool upstream' } }),
    provider: PROVIDER,
    actions: [{ op: 'pool' }, { op: 'accounts' }],
    secrets: SECRETS,
  },
  {
    name: 'upstream api key resolves and is marked available',
    state: storedState([]),
    provider: PROVIDER,
    actions: [merged('apiKey.resolve', cred(apiKeyCredential('upstream-key')))],
  },
  {
    name: 'upstream resolve failure falls back to the pool when accounts exist',
    state: storedState([storedAccount('acct-extra', 'Extra', apiKeyCredential('extra-key'))]),
    provider: PROVIDER,
    actions: [merged('apiKey.resolve', cred(apiKeyCredential('expired-upstream')))],
  },
  {
    name: 'upstream resolve failure rethrows without stored accounts',
    state: storedState([]),
    provider: PROVIDER,
    actions: [merged('apiKey.resolve', cred(apiKeyCredential('expired-upstream')))],
  },
  {
    name: 'upstream unavailable without stored accounts yields nothing',
    state: storedState([]),
    provider: PROVIDER,
    actions: [merged('apiKey.resolve', cred(apiKeyCredential('missing-key'))), merged('apiKey.check', cred(apiKeyCredential('missing-key')))],
  },
  {
    name: 'upstream unavailable with stored accounts yields an unavailable pool marker',
    state: storedState([storedAccount('acct-extra', 'Extra', apiKeyCredential('extra-key'))]),
    provider: PROVIDER,
    actions: [
      merged('apiKey.resolve', cred(apiKeyCredential('missing-key'))),
      excludeIds({ headers: { [MARKER]: marker(0, ['auth', 'headers', MARKER]) } }),
    ],
  },
  {
    name: 'native check result is returned unchanged',
    state: storedState([storedAccount('acct-extra', 'Extra', apiKeyCredential('extra-key'))]),
    provider: PROVIDER,
    actions: [merged('apiKey.check', cred(apiKeyCredential('upstream-key')))],
  },
  {
    name: 'undefined native check falls back to the pool',
    state: storedState([storedAccount('acct-extra', 'Extra', apiKeyCredential('extra-key'))]),
    provider: PROVIDER,
    actions: [merged('apiKey.check', cred(apiKeyCredential('missing-key')))],
  },
  {
    name: 'undefined native check without stored accounts is unconfigured',
    state: storedState([]),
    provider: PROVIDER,
    actions: [merged('apiKey.check', cred(apiKeyCredential('missing-key')))],
  },
  {
    name: 'native check errors propagate even with stored accounts',
    state: storedState([storedAccount('acct-extra', 'Extra', apiKeyCredential('extra-key'))]),
    provider: PROVIDER,
    actions: [merged('apiKey.check', cred(apiKeyCredential('check-throw')))],
  },
  {
    name: 'check without a native check uses resolve',
    state: storedState([]),
    provider: withProvider({ apiKey: { name: 'Resolve only API key', responses: { resolve: { 'bare-key': { value: { auth: { apiKey: 'bare' }, source: 'Env' } }, 'no-base-key': { value: { auth: { apiKey: 'no-base' } } }, '<none>': { none: true } } } } }),
    actions: [
      merged('apiKey.check', cred(apiKeyCredential('bare-key'))),
      merged('apiKey.check', cred(apiKeyCredential('no-base-key'))),
      merged('apiKey.check', cred(apiKeyCredential('missing-key'))),
    ],
  },
  {
    name: 'resolve only check still reports stored accounts when native is absent',
    state: storedState([storedAccount('acct-extra', 'Extra', apiKeyCredential('extra-key'))]),
    provider: withProvider({ apiKey: { name: 'Resolve only API key', responses: { resolve: { '<none>': { none: true }, 'expired-upstream': { throw: 'upstream credential expired' } } } } }),
    actions: [
      merged('apiKey.check', cred(apiKeyCredential('expired-upstream'))),
      merged('apiKey.resolve', cred(apiKeyCredential('expired-upstream'))),
    ],
  },
  {
    name: 'absent api key method still exposes the pool',
    state: storedState([storedAccount('acct-extra', 'Extra', apiKeyCredential('extra-key'))]),
    provider: oauthOnly,
    actions: [
      { op: 'shape' },
      merged('apiKey.check', cred(apiKeyCredential('x'))),
      merged('apiKey.resolve', cred(apiKeyCredential('x'))),
    ],
  },
  {
    name: 'absent api key method without pool is unconfigured',
    state: storedState([]),
    provider: oauthOnly,
    actions: [merged('apiKey.check'), merged('apiKey.resolve')],
  },
  {
    name: 'oauth wrapper marks toAuth and forwards refresh',
    state: storedState([]),
    provider: PROVIDER,
    actions: [
      merged('oauth.toAuth', cred(oauthCredential('access-fresh', 'refresh-fresh', 1700003600000))),
      merged('oauth.refresh', cred(oauthCredential('access-old', 'refresh-old', 1700000000000))),
      merged('oauth.refresh', cred(oauthCredential('access-old', 'refresh-fail', 1700000000000))),
      { op: 'shape' },
    ],
  },
  {
    name: 'oauth absent omits the oauth surface',
    state: storedState([]),
    provider: apiKeyOnly,
    actions: [{ op: 'shape' }, { op: 'hasAccounts' }],
  },
  {
    name: 'minimal provider auth exposes only required keys',
    state: storedState([]),
    provider: withProvider({ apiKey: { name: 'Plain', responses: { resolve: {} } }, oauth: { name: 'Plain OAuth', responses: OAUTH } }),
    actions: [{ op: 'shape' }],
  },
  {
    name: 'marker round trip from an upstream resolution',
    state: storedState([storedAccount('acct-extra', 'Extra', apiKeyCredential('extra-key'))]),
    provider: PROVIDER,
    actions: [
      merged('apiKey.resolve', cred(apiKeyCredential('upstream-key'))),
      excludeIds({ headers: { [MARKER]: marker(0, ['auth', 'headers', MARKER]) } }),
      { op: 'resolveAuth', accountId: 'acct-extra', requestOptions: { headers: { Authorization: 'Bearer upstream-key', 'x-upstream-only': 'upstream', [MARKER]: marker(0, ['auth', 'headers', MARKER]) }, env: { UPSTREAM_ONLY: 'upstream' } } },
      sanitize('acct-extra', {
        requestOptions: { headers: { Authorization: 'Bearer upstream-key', 'x-upstream-only': 'upstream', [MARKER]: marker(0, ['auth', 'headers', MARKER]) }, env: { UPSTREAM_ONLY: 'upstream' } },
        resolution: { $ref: 2, path: ['result'] },
      }),
    ],
  },
  {
    name: 'spec: upstream credential is attempted first and pool-only requests spill to the stored account',
    state: storedState([storedAccount('acct-extra', 'Extra', apiKeyCredential('extra-key'))]),
    provider: PROVIDER,
    actions: [
      merged('apiKey.resolve', cred(apiKeyCredential('upstream-key'))),
      excludeIds({ headers: { [MARKER]: marker(0, ['auth', 'headers', MARKER]) } }),
      resolveAuth('acct-extra', { requestOptions: { apiKey: 'upstream-key', headers: { Authorization: 'Bearer upstream-key', 'x-upstream-only': 'upstream', [MARKER]: marker(0, ['auth', 'headers', MARKER]) }, env: { UPSTREAM_ONLY: 'upstream' } } }),
      merged('apiKey.resolve', cred(apiKeyCredential('missing-key'))),
      excludeIds({ headers: { [MARKER]: marker(3, ['auth', 'headers', MARKER]) } }),
      sanitize('acct-extra', { requestOptions: { apiKey: 'upstream-key', headers: { Authorization: 'Bearer upstream-key', 'x-upstream-only': 'upstream', [MARKER]: marker(0, ['auth', 'headers', MARKER]) }, env: { UPSTREAM_ONLY: 'upstream' } }, resolution: { $ref: 2, path: ['result'] } }),
    ],
  },
  {
    name: 'marker parsing variants',
    state: storedState([]),
    provider: PROVIDER,
    actions: [
      excludeIds({ headers: { [MARKER]: '{"available":true,"headerNames":[],"envNames":[],"customBaseUrl":false}' } }),
      excludeIds({ headers: { [MARKER]: '{"available":false,"headerNames":[],"envNames":[],"customBaseUrl":false}' } }),
      excludeIds({ headers: { [MARKER]: '{"available":"yes"}' } }),
      excludeIds({ headers: { [MARKER]: '{"available":1}' } }),
      excludeIds({ headers: { [MARKER]: '[true]' } }),
      excludeIds({ headers: { [MARKER]: 'null' } }),
      excludeIds({ headers: { [MARKER]: '{not json' } }),
      excludeIds({ headers: { [MARKER]: 42 } }),
      excludeIds({ headers: { 'X-PI-MULTIPROVIDER-AUTH-SOURCE': '{"available":true}' } }),
      excludeIds({ headers: { 'x-pi-multiprovider-auth-source': '{"available":false}', 'X-Pi-Multiprovider-Auth-Source': '{"available":true}' } }),
      excludeIds({ headers: { 'X-Pi-Multiprovider-Auth-Source': '{"available":true}', 'x-pi-multiprovider-auth-source': '{"available":false}' } }),
      excludeIds({ headers: {} }),
      excludeIds({}),
    ],
  },
  {
    name: 'sanitize leaves the pi default account untouched except the marker',
    state: storedState([]),
    provider: PROVIDER,
    actions: [
      sanitize('pi:default', {
        requestOptions: { apiKey: 'pi-key', headers: { Authorization: 'Bearer pi', 'x-custom': 'keep', [MARKER]: '{"available":true,"headerNames":["x-custom"],"envNames":["PI_ONLY"],"customBaseUrl":false}' }, env: { PI_ONLY: 'yes' } },
        resolution: { auth: {} },
      }),
    ],
  },
  {
    name: 'sanitize removes unselected marker headers and environment',
    state: storedState([storedAccount('acct-extra', 'Extra', apiKeyCredential('extra-key'))]),
    provider: PROVIDER,
    actions: [
      sanitize('acct-extra', {
        requestOptions: {
          apiKey: 'extra-key',
          headers: { Authorization: 'Bearer upstream-key', 'x-upstream-only': 'upstream', 'X-Extra': 'kept', [MARKER]: '{"available":true,"headerNames":["authorization","x-upstream-only","x-extra"],"envNames":["UPSTREAM_ONLY","EXTRA_ONLY"],"customBaseUrl":true}' },
          env: { UPSTREAM_ONLY: 'upstream', EXTRA_ONLY: 'extra', UNMARKED: 'stays' },
        },
        resolution: { auth: { apiKey: 'extra-key', headers: { 'x-extra': 'extra' } }, env: { EXTRA_ONLY: 'extra' }, source: 'Extra · Stored' },
      }),
    ],
  },
  {
    name: 'sanitize drops emptied environment and headers',
    state: storedState([storedAccount('acct-extra', 'Extra', apiKeyCredential('extra-key'))]),
    provider: PROVIDER,
    actions: [
      sanitize('acct-extra', {
        requestOptions: {
          headers: { Authorization: 'Bearer old', [MARKER]: '{"available":true,"headerNames":["Authorization"],"envNames":["OLD"],"customBaseUrl":false}' },
          env: { OLD: 'stale' },
        },
        resolution: { auth: {} },
      }),
      sanitize('acct-extra', {
        requestOptions: { headers: {}, env: {} },
        resolution: { auth: {} },
      }),
    ],
  },
  {
    name: 'sanitize without a marker only strips the marker header',
    state: storedState([storedAccount('acct-extra', 'Extra', apiKeyCredential('extra-key'))]),
    provider: PROVIDER,
    actions: [
      sanitize('acct-extra', {
        requestOptions: { apiKey: 'raw', env: { KEEP: 'yes' }, headers: { Authorization: 'Bearer raw', [MARKER]: 'not-json' } },
        resolution: { auth: { apiKey: 'extra-key' } },
      }),
    ],
  },
  {
    name: 'sanitize matches header names case-insensitively against the selected resolution',
    state: storedState([storedAccount('acct-extra', 'Extra', apiKeyCredential('extra-key'))]),
    provider: PROVIDER,
    actions: [
      sanitize('acct-extra', {
        requestOptions: { headers: { AUTHORIZATION: 'Bearer kept', 'x-drop': 'gone', [MARKER]: '{"available":true,"headerNames":["Authorization","X-Drop"],"envNames":[],"customBaseUrl":false}' } },
        resolution: { auth: { headers: { authorization: 'Bearer selected' } } },
      }),
    ],
  },
  {
    name: 'resolveAuth for pi default copies api key and environment',
    state: storedState([]),
    provider: PROVIDER,
    actions: [
      { op: 'resolveAuth', account: { id: 'pi:default', label: 'Pi default', authKind: 'custom', credentialRef: 'pi:default' }, requestOptions: { apiKey: 'pi-key', env: { A: '1' }, headers: { X: 'y' } } },
      { op: 'resolveAuth', account: { id: 'pi:default', label: 'Pi default', authKind: 'custom', credentialRef: 'pi:default' }, requestOptions: { env: null } },
      { op: 'resolveAuth', account: { id: 'pi:default', label: 'Pi default', authKind: 'custom', credentialRef: 'pi:default' }, requestOptions: {} },
    ],
  },
  {
    name: 'resolveAuth for a stored api key account',
    state: storedState([storedAccount('acct-extra', 'Extra', apiKeyCredential('extra-key'))]),
    provider: PROVIDER,
    actions: [resolveAuth('acct-extra', { requestOptions: {} })],
  },
  {
    name: 'resolveAuth fails for disabled and unknown accounts',
    state: storedState([storedAccount('acct-off', 'Off', apiKeyCredential('extra-key'), { enabled: false })]),
    provider: PROVIDER,
    actions: [
      resolveAuth('acct-off'),
      { op: 'resolveAuth', account: { id: 'ghost', label: 'Ghost', authKind: 'oauth', credentialRef: 'ghost' }, requestOptions: {} },
    ],
  },
  {
    name: 'resolveAuth refreshes an expiring oauth token under the lock',
    state: storedState([storedAccount('acct-oauth', 'Work', oauthCredential('access-old', 'refresh-old', 1700000000000), { authKind: 'oauth' })]),
    provider: PROVIDER,
    actions: [resolveAuth('acct-oauth', { requestOptions: {} })],
  },
  {
    name: 'resolveAuth reuses a fresh oauth token without refresh',
    state: storedState([storedAccount('acct-oauth', 'Work', oauthCredential('access-work', 'refresh-work', 1700003600000))]),
    provider: PROVIDER,
    actions: [resolveAuth('acct-oauth', { requestOptions: {} })],
  },
  {
    name: 'refresh threshold uses inclusive five minute skew',
    state: storedState([
      storedAccount('acct-edge', 'Edge', oauthCredential('access-old', 'refresh-boundary', 1700000300000)),
      storedAccount('acct-above', 'Above', oauthCredential('access-above', 'refresh-above', 1700000300001)),
    ]),
    provider: PROVIDER,
    actions: [resolveAuth('acct-edge'), resolveAuth('acct-above')],
  },
  {
    name: 'missing expiry is treated as not expiring',
    state: storedState([storedAccount('acct-noexp', 'NoExp', { type: 'oauth', access: 'access-work', refresh: 'refresh-old' })]),
    provider: PROVIDER,
    actions: [resolveAuth('acct-noexp')],
  },
  {
    name: 'refresh failure releases the lock without persisting',
    state: storedState([storedAccount('acct-oauth', 'Work', oauthCredential('access-old', 'refresh-fail', 1700000000000))]),
    provider: PROVIDER,
    actions: [resolveAuth('acct-oauth'), { op: 'accounts' }],
  },
  {
    name: 'refresh returning an api key credential is rejected',
    state: storedState([storedAccount('acct-oauth', 'Work', oauthCredential('access-old', 'refresh-api', 1700000000000))]),
    provider: PROVIDER,
    actions: [resolveAuth('acct-oauth')],
  },
  {
    name: 'malformed refreshed credentials are rejected',
    state: storedState([
      storedAccount('acct-a', 'A', oauthCredential('access-old', 'refresh-malformed', 1700000000000)),
      storedAccount('acct-b', 'B', oauthCredential('access-old', 'refresh-no-type', 1700000000000)),
      storedAccount('acct-c', 'C', oauthCredential('access-old', 'refresh-bad-type', 1700000000000)),
    ]),
    provider: PROVIDER,
    actions: [resolveAuth('acct-a'), resolveAuth('acct-b'), resolveAuth('acct-c')],
  },
  {
    name: 'rotated credential is persisted with its refresh timestamp',
    state: storedState([storedAccount('acct-oauth', 'Work', oauthCredential('access-old', 'refresh-old', 1700000000000, { account: 'before' }))]),
    provider: PROVIDER,
    actions: [resolveAuth('acct-oauth'), { op: 'pool' }],
  },
  {
    name: 'oauth toAuth failure happens after the lock is released',
    state: storedState([storedAccount('acct-oauth', 'Work', oauthCredential('access-throw', 'refresh-throw', 1700003600000))]),
    provider: PROVIDER,
    actions: [resolveAuth('acct-oauth')],
  },
  {
    name: 'account type mismatches are refused before native calls',
    state: storedState([storedAccount('acct-key', 'Key', apiKeyCredential('extra-key'))]),
    provider: oauthOnly,
    actions: [resolveAuth('acct-key')],
  },
  {
    name: 'oauth account without an oauth method is refused',
    state: storedState([storedAccount('acct-oauth', 'Work', oauthCredential('access-work', 'refresh-work', 1700003600000))]),
    provider: apiKeyOnly,
    actions: [resolveAuth('acct-oauth')],
  },
  {
    name: 'api key account without an api key method is refused',
    state: storedState([storedAccount('acct-key', 'Key', apiKeyCredential('extra-key'))]),
    provider: { ...PROVIDER, apiKey: undefined },
    actions: [resolveAuth('acct-key')],
  },
  {
    name: 'api key account that does not resolve is refused',
    state: storedState([storedAccount('acct-key', 'Key', apiKeyCredential('missing-key'))]),
    provider: PROVIDER,
    actions: [resolveAuth('acct-key')],
  },
  {
    name: 'api key source falls back to the method name',
    state: storedState([storedAccount('acct-key', 'Key', apiKeyCredential('bare-key'))]),
    provider: PROVIDER,
    actions: [resolveAuth('acct-key')],
  },
  {
    name: 'aborted resolution never touches the store',
    state: storedState([storedAccount('acct-oauth', 'Work', oauthCredential('access-old', 'refresh-old', 1700000000000))]),
    provider: PROVIDER,
    actions: [{ op: 'resolveAccount', accountId: 'acct-oauth', aborted: true }, resolveAuth('acct-oauth', { aborted: true })],
  },
  {
    name: 'restores the base url from the requested model',
    state: storedState([storedAccount('acct-key', 'Key', apiKeyCredential('no-base-key'))]),
    provider: PROVIDER,
    actions: [
      resolveAuth('acct-key', { model: 'same-model', requestOptions: { headers: { [MARKER]: '{"available":true,"headerNames":[],"envNames":[],"customBaseUrl":true}' } } }),
    ],
  },
  {
    name: 'base url falls back to the provider for unknown or unset models',
    state: storedState([storedAccount('acct-key', 'Key', apiKeyCredential('no-base-key'))]),
    provider: PROVIDER,
    actions: [
      resolveAuth('acct-key', { model: 'plain-model', requestOptions: { headers: { [MARKER]: '{"available":true,"headerNames":[],"envNames":[],"customBaseUrl":true}' } } }),
      resolveAuth('acct-key', { model: 'unknown-model', requestOptions: { headers: { [MARKER]: '{"available":true,"headerNames":[],"envNames":[],"customBaseUrl":true}' } } }),
    ],
  },
  {
    name: 'base url is not restored without a custom base url marker or with one present',
    state: storedState([storedAccount('acct-key', 'Key', apiKeyCredential('no-base-key'))]),
    provider: PROVIDER,
    actions: [
      resolveAuth('acct-key', { model: 'same-model', requestOptions: { headers: { [MARKER]: '{"available":true,"headerNames":[],"envNames":[],"customBaseUrl":false}' } } }),
      resolveAuth('acct-key', { model: 'same-model', requestOptions: {} }),
      resolveAuth('acct-key', { model: 'same-model', requestOptions: { headers: { [MARKER]: '{"available":true,"headerNames":[],"envNames":[],"customBaseUrl":true}' } } }),
    ],
  },
  {
    name: 'restoration does not run when the provider reports its own base url',
    state: storedState([storedAccount('acct-key', 'Key', apiKeyCredential('custom-base-key'))]),
    provider: PROVIDER,
    actions: [
      resolveAuth('acct-key', { model: 'same-model', requestOptions: { headers: { [MARKER]: '{"available":true,"headerNames":[],"envNames":[],"customBaseUrl":true}' } } }),
    ],
  },
  {
    name: 'absent provider base url leaves auth without a base url',
    state: storedState([storedAccount('acct-key', 'Key', apiKeyCredential('no-base-key'))]),
    provider: withProvider({ baseUrl: undefined, models: [{ id: 'plain-model' }] }),
    actions: [
      resolveAuth('acct-key', { model: 'plain-model', requestOptions: { headers: { [MARKER]: '{"available":true,"headerNames":[],"envNames":[],"customBaseUrl":true}' } } }),
    ],
  },
];

function signalFor(action) {
  return action.aborted === true ? AbortSignal.abort() : new AbortController().signal;
}

async function runManagedAction(action, env) {
  const { provider, store, integration, merged: auth } = env;
  const requestFor = (signal) => ({
    provider,
    model: { id: action.model ?? 'same-model' },
    context: { messages: [] },
    requestOptions: action.requestOptions ?? {},
    signal,
  });
  const accountFor = async () => {
    if (action.account !== undefined) return action.account;
    const found = (await integration.accounts()).find(candidate => candidate.id === action.accountId);
    if (found === undefined) throw new Error(`unknown account ${action.accountId}`);
    return found;
  };
  const credentialInput = () => (action.credential === undefined ? {} : { credential: action.credential });
  switch (action.op) {
    case 'accounts': return integration.accounts();
    case 'pool': return store.getPool(provider.id);
    case 'hasAccounts': return store.hasAccounts(provider.id);
    case 'lookup': return accountFor();
    case 'resolveAccount': return store.resolveAccount(provider, action.accountId, signalFor(action));
    case 'shape': return {
      topLevel: Object.keys(auth),
      apiKey: Object.keys(auth.apiKey),
      oauth: auth.oauth === undefined ? null : Object.keys(auth.oauth),
      integration: Object.keys(integration),
      hint: integration.managementHint,
    };
    case 'merged': {
      switch (action.name) {
        case 'apiKey.check': return auth.apiKey.check({ ctx: {}, ...credentialInput(), signal: signalFor(action) });
        case 'apiKey.resolve': return auth.apiKey.resolve({ ctx: {}, ...credentialInput(), signal: signalFor(action) });
        case 'oauth.toAuth': return auth.oauth.toAuth(action.credential);
        case 'oauth.refresh': return auth.oauth.refresh(action.credential, signalFor(action));
        default: throw new Error(`unknown merged ${action.name}`);
      }
    }
    case 'excludeAccountIds': return integration.excludeAccountIds(requestFor(new AbortController().signal));
    case 'resolveAuth': {
      const signal = signalFor(action);
      return integration.resolveAuth(await accountFor(), signal, requestFor(signal));
    }
    case 'sanitize': return integration.sanitizeRequestOptions({
      ...requestFor(new AbortController().signal),
      account: await accountFor(),
      resolution: action.resolution,
    });
    default: throw new Error(`unknown action ${action.op}`);
  }
}

// Replaces {"$ref": n, "path": [...]} with a value recorded by an earlier action.
function resolveRefs(value, results) {
  if (Array.isArray(value)) return value.map(entry => resolveRefs(entry, results));
  if (value !== null && typeof value === 'object') {
    if (Object.hasOwn(value, '$ref')) {
      let current = results[value.$ref];
      assert(current !== undefined && Object.hasOwn(current, 'result'), `ref ${value.$ref} has no result`);
      if (value.path === undefined) return structuredClone(current.result);
      if (value.path.length === 1 && value.path[0] === 'result') return structuredClone(current.result);
      current = current.result;
      for (const key of value.path) {
        assert(current !== null && typeof current === 'object' && Object.hasOwn(current, key), `ref path ${key} missing`);
        current = current[key];
      }
      return structuredClone(current);
    }
    return Object.fromEntries(Object.entries(value).map(([key, entry]) => [key, resolveRefs(entry, results)]));
  }
  return value;
}

async function withStore(state, run) {
  const directory = await mkdtemp(join(tmpdir(), 'rpi-managed-'));
  const storePath = join(directory, 'multiprovider-auth.json');
  const lockPath = `${storePath}.lock`;
  try {
    if (state !== undefined && state !== null) await writeFile(storePath, `${JSON.stringify(state, null, 2)}\n`);
    return await run({ storePath, lockPath });
  } finally {
    probe = null;
    await rm(directory, { recursive: true, force: true });
  }
}

async function finish(scenario, results, storePath, lockPath) {
  probe = null;
  const finalState = existsSync(storePath) ? JSON.parse(await readFile(storePath, 'utf8')) : null;
  assert(!existsSync(lockPath), `${scenario.name} left the lock file behind`);
  for (const secret of scenario.secrets ?? []) {
    for (const result of results.filter(entry => entry.op === 'accounts' || entry.op === 'pool')) {
      assert(!JSON.stringify(result).includes(secret), `${scenario.name} exposed ${secret}`);
    }
  }
  return { ...scenario, now: scenario.now ?? DEFAULT_NOW, results, finalState };
}

async function runManagedCase(scenario) {
  now = scenario.now ?? DEFAULT_NOW;
  return withStore(scenario.state, async ({ storePath, lockPath }) => {
    const trace = [];
    probe = { storePath, lockPath, trace };
    const provider = makeProvider(scenario.provider, trace);
    const store = new MultiAuthStore(storePath);
    const integration = managed.createManagedIntegration(provider, store);
    const env = { provider, store, integration, merged: integration.auth };
    const results = [];
    for (const action of scenario.actions) {
      const input = resolveRefs(action, results);
      const before = trace.length;
      let outcome;
      try {
        outcome = { result: plain(await runManagedAction(input, env)) };
      } catch (error) {
        outcome = { error: error.message };
      }
      results.push({ op: action.op, ...outcome, trace: plain(trace.slice(before)) });
    }
    return finish({ ...scenario, actions: scenario.actions }, results, storePath, lockPath);
  });
}

// Announcement scenarios: the real upstream scheduler and announcement, with
// either a scripted integration or the real managed integration.
const SPEC_ACCOUNTS = [
  { id: 'pi:default', label: 'Pi default', authKind: 'custom', credentialRef: 'pi:default' },
  { id: 'a', label: 'Work', authKind: 'oauth', credentialRef: 'ref-a' },
  { id: 'b', label: 'Personal', authKind: 'oauth', credentialRef: 'ref-b' },
];
const SPEC_RESOLVE = { a: { value: { auth: { apiKey: 'token-a' }, source: 'Work · Test OAuth' } }, b: { throw: 'resolve failed' } };
const pin = (account, key) => ({ op: 'pin', account, ...(key === undefined ? {} : { key }) });
const subscribe = (handle, listener, provider) => ({ op: 'subscribe', handle, listener, ...(provider === undefined ? {} : { provider }) });
const unsubscribe = handle => ({ op: 'unsubscribe', handle });
const notify = (account, provider) => ({ op: 'notify', account, ...(provider === undefined ? {} : { provider }) });
const setResolve = (accountId, entry) => ({ op: 'setResolve', accountId, entry });
const ACTIVE_A = { id: 'a', label: 'Work', authKind: 'oauth' };
const scriptedIntegration = (overrides = {}) => ({ kind: 'scripted', accounts: SPEC_ACCOUNTS, resolve: SPEC_RESOLVE, ...overrides });
const managedState = storedState([storedAccount('work', 'Work', oauthCredential('access-old', 'refresh-old', 1700000000000))]);
const managedKeyState = storedState([storedAccount('key-acct', 'Keyed', apiKeyCredential('header-key'))]);

const announcementCases = [
  {
    name: 'spec: explicit pin resolves its stored credential',
    affinity: false,
    integration: scriptedIntegration(),
    steps: [{ op: 'active' }, pin('a'), { op: 'active' }, { op: 'auth' }],
  },
  {
    name: 'spec: last scheduler selection is reported while affinity is on',
    selectionBias: 'none',
    integration: scriptedIntegration(),
    steps: [{ op: 'acquire', key: 'session-1' }, { op: 'active' }],
  },
  {
    name: 'spec: upstream and unpinned sessions never resolve credentials',
    integration: scriptedIntegration(),
    steps: [{ op: 'auth' }, pin('pi:default'), { op: 'active' }, { op: 'auth' }],
  },
  {
    name: 'spec: bearer headers and resolver failures',
    integration: scriptedIntegration({ resolve: { a: { value: { auth: { headers: { Authorization: 'Bearer header-token' } } } }, b: { throw: 'resolve failed' } } }),
    steps: [pin('a'), { op: 'auth' }, pin('b'), { op: 'auth' }, setResolve('a', { value: { auth: {} } }), { op: 'auth' }],
  },
  {
    name: 'spec: listeners are per provider and unsubscribe',
    integration: scriptedIntegration(),
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
    steps: [{ op: 'active', provider: 'missing' }, { op: 'auth', provider: 'missing' }],
  },
  {
    name: 'affinity off ignores implicit scheduler selections',
    affinity: false,
    integration: scriptedIntegration(),
    steps: [{ op: 'acquire', key: 'session-1' }, { op: 'active' }, pin('b'), { op: 'active' }],
  },
  {
    name: 'stale pin to a removed account is silent',
    integration: scriptedIntegration(),
    steps: [pin('a'), { op: 'setAccounts', accounts: [SPEC_ACCOUNTS[0], SPEC_ACCOUNTS[2]] }, { op: 'active' }, { op: 'auth' }],
  },
  {
    name: 'integration accounts failure after a pin rejects',
    integration: scriptedIntegration(),
    steps: [pin('a'), { op: 'setAccountsError', message: 'accounts down' }, { op: 'active' }, { op: 'auth' }, { op: 'setAccountsError', message: null }, { op: 'active' }],
  },
  {
    name: 'affinity key failure rejects before any pin lookup',
    affinityKeyError: 'key lookup failed',
    integration: scriptedIntegration(),
    steps: [pin('a'), { op: 'active' }, { op: 'auth' }],
  },
  {
    name: 'unregistered scheduler provider is silent',
    unregistered: true,
    integration: scriptedIntegration(),
    steps: [{ op: 'active' }, { op: 'auth' }, pin('a')],
  },
  {
    name: 'missing base provider suppresses auth resolution',
    noBaseProvider: true,
    integration: scriptedIntegration(),
    steps: [pin('a'), { op: 'active' }, { op: 'auth' }],
  },
  {
    name: 'provider without models suppresses auth resolution',
    provider: { id: 'example', name: 'Example', models: [] },
    integration: scriptedIntegration(),
    steps: [pin('a'), { op: 'auth' }],
  },
  {
    name: 'context model overrides the base provider model',
    ctxModel: 'ctx-model',
    provider: { id: 'example', name: 'Example', models: [{ id: 'base-model' }] },
    integration: scriptedIntegration(),
    steps: [pin('a'), { op: 'auth' }],
  },
  {
    name: 'access tokens are trimmed and blank tokens are dropped',
    integration: scriptedIntegration(),
    steps: [
      pin('a'),
      setResolve('a', { value: { auth: { apiKey: '  tok  ' }, source: 'Padded' } }),
      { op: 'auth' },
      setResolve('a', { value: { auth: { apiKey: '   ' }, source: 'Blank' } }),
      { op: 'auth' },
      setResolve('a', { value: { auth: { apiKey: '', headers: { Authorization: 'Bearer from-header' } } } }),
      { op: 'auth' },
    ],
  },
  {
    name: 'bearer header parsing follows the upstream pattern',
    integration: scriptedIntegration(),
    steps: [
      pin('a'),
      setResolve('a', { value: { auth: { headers: { Authorization: 'bearer   xyz ' } } } }), { op: 'auth' },
      setResolve('a', { value: { auth: { headers: { Authorization: 'Basic abc' } } } }), { op: 'auth' },
      setResolve('a', { value: { auth: { headers: { Authorization: 'BEARER a\nb' } } } }), { op: 'auth' },
      setResolve('a', { value: { auth: { headers: { AUTHORIZATION: 'Bearer up' } } } }), { op: 'auth' },
      setResolve('a', { value: { auth: { headers: { Authorization: null, 'x-other': 'Bearer nope', authorization: 'Bearer second' } } } }), { op: 'auth' },
      setResolve('a', { value: { auth: { headers: { Authorization: 'Bearer first', authorization: 'Bearer second' } } } }), { op: 'auth' },
      setResolve('a', { value: { auth: { headers: { Authorization: 'Bearer ' } } } }), { op: 'auth' },
      setResolve('a', { value: { auth: { headers: { Authorization: 'Bearer\nspaced' } } } }), { op: 'auth' },
      setResolve('a', { value: { auth: { headers: { Authorization: 'Bearer nbsp' } } } }), { op: 'auth' },
    ],
  },
  {
    name: 'signal state is forwarded to the integration',
    integration: scriptedIntegration(),
    steps: [pin('a'), { op: 'auth', signal: 'live' }, { op: 'auth', signal: 'aborted' }, { op: 'auth', signal: 'none' }],
  },
  {
    name: 'a throwing listener does not stop its siblings',
    integration: scriptedIntegration(),
    listeners: { L1: { throws: 'listener boom' }, L2: {} },
    steps: [subscribe('h1', 'L1'), subscribe('h2', 'L2'), notify(ACTIVE_A), notify(undefined)],
  },
  {
    name: 'a listener unsubscribing a later sibling skips it in the same notification',
    integration: scriptedIntegration(),
    listeners: { L1: { calls: [{ op: 'unsubscribe', handle: 'h2' }] }, L2: {} },
    steps: [subscribe('h1', 'L1'), subscribe('h2', 'L2'), notify(ACTIVE_A), notify(ACTIVE_A)],
  },
  {
    name: 'a listener subscribed during notification is visited in that notification',
    integration: scriptedIntegration(),
    listeners: { L1: { calls: [{ op: 'subscribe', handle: 'h3', listener: 'L3' }] }, L3: {} },
    steps: [subscribe('h1', 'L1'), notify(ACTIVE_A), notify(undefined)],
  },
  {
    name: 'duplicate subscriptions form one listener set',
    integration: scriptedIntegration(),
    listeners: { L1: {} },
    steps: [subscribe('h1', 'L1'), subscribe('h2', 'L1'), notify(ACTIVE_A), unsubscribe('h1'), notify(ACTIVE_A)],
  },
  {
    name: 'a stale unsubscribe removes a later subscription of the same listener',
    integration: scriptedIntegration(),
    listeners: { L1: {} },
    steps: [subscribe('h1', 'L1'), unsubscribe('h1'), subscribe('h2', 'L1'), unsubscribe('h1'), notify(ACTIVE_A)],
  },
  {
    name: 'listeners are isolated by provider and see the session context',
    integration: scriptedIntegration(),
    listeners: { L1: {} },
    steps: [subscribe('h1', 'L1', 'example'), notify(ACTIVE_A, 'other'), notify(null), notify(ACTIVE_A)],
  },
  {
    name: 'managed oauth account resolves through a pin and the lock',
    state: managedState,
    integration: { kind: 'managed' },
    provider: PROVIDER,
    steps: [pin('work'), { op: 'active' }, { op: 'auth' }],
  },
  {
    name: 'managed pi default pin never touches the store',
    state: managedState,
    integration: { kind: 'managed' },
    provider: PROVIDER,
    steps: [pin('pi:default'), { op: 'active' }, { op: 'auth' }],
  },
  {
    name: 'managed api key account yields a bearer token from its headers',
    state: managedKeyState,
    integration: { kind: 'managed' },
    provider: PROVIDER,
    steps: [pin('key-acct'), { op: 'auth' }],
  },
  {
    name: 'managed refresh failure is silent for the announcement',
    state: storedState([storedAccount('work', 'Work', oauthCredential('access-old', 'refresh-fail', 1700000000000))]),
    integration: { kind: 'managed' },
    provider: PROVIDER,
    steps: [pin('work'), { op: 'auth' }],
  },
];

async function runAnnouncementCase(scenario) {
  now = scenario.now ?? DEFAULT_NOW;
  return withStore(scenario.state, async ({ storePath, lockPath }) => {
    const trace = [];
    probe = { storePath, lockPath, trace };
    const providerSpec = scenario.provider ?? { id: 'example', name: 'Example', models: [{ id: 'model' }] };
    const provider = makeProvider(providerSpec, trace);
    const store = new MultiAuthStore(storePath);
    const cfg = scenario.integration;
    const scripted = { accounts: structuredClone(cfg.accounts ?? []), resolve: structuredClone(cfg.resolve ?? {}), accountsError: undefined };
    const integration = cfg.kind === 'managed'
      ? managed.createManagedIntegration(provider, store)
      : {
        id: 'example',
        label: 'Example',
        accounts: () => {
          if (scripted.accountsError !== undefined) throw new Error(scripted.accountsError);
          return structuredClone(scripted.accounts);
        },
        resolveAuth: async (account, signal, request) => {
          trace.push({ integration: 'resolveAuth', account: account.id, aborted: signal.aborted, model: request.model.id, context: request.context, requestOptions: request.requestOptions });
          if (!Object.hasOwn(scripted.resolve, account.id)) throw new Error(`unscripted resolve ${account.id}`);
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
    if (scenario.unregistered !== true) {
      scheduler.registerProvider({
        id: 'example',
        label: 'Example',
        accounts: () => integration.accounts(),
        ...(scenario.selectionBias === undefined ? {} : { selectionBias: scenario.selectionBias }),
      });
    }
    const ctx = {
      model: scenario.ctxModel === undefined ? undefined : { id: scenario.ctxModel },
      sessionManager: { getSessionId: () => 'session-1' },
      modelRegistry: { getProvider: () => provider },
    };
    const announcement = createServiceAnnouncement({
      scheduler,
      getIntegration: providerId => (providerId === 'example' ? integration : undefined),
      getBaseProvider: providerId => (providerId === 'example' && scenario.noBaseProvider !== true ? provider : undefined),
      affinityKeyFor(_integration, _ctx, providerId) {
        trace.push({ affinityKeyFor: providerId });
        if (scenario.affinityKeyError !== undefined) throw new Error(scenario.affinityKeyError);
        return 'session-1';
      },
    });
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
          for (const step of definition.calls ?? []) {
            if (step.op === 'subscribe') {
              handles.set(step.handle, announcement.onActiveAccountChanged('example', listenerFor(step.listener)));
            } else {
              handles.get(step.handle)();
            }
          }
          if (definition.throws !== undefined) throw new Error(definition.throws);
        });
      }
      return listeners.get(name);
    };
    const results = [];
    for (const step of scenario.steps) {
      const input = resolveRefs(step, results);
      const before = trace.length;
      let outcome;
      try {
        outcome = { result: plain(await runAnnouncementStep(input, { scheduler, announcement, ctx, scripted, handles, listenerFor, trace })) };
      } catch (error) {
        outcome = { error: error.message };
      }
      results.push({ op: step.op, ...outcome, trace: plain(trace.slice(before)) });
    }
    return finish(scenario, results, storePath, lockPath);
  });
}

async function runAnnouncementStep(step, env) {
  const { scheduler, announcement, ctx, scripted, handles, listenerFor } = env;
  const provider = step.provider ?? 'example';
  switch (step.op) {
    case 'pin': return scheduler.pinAccount('example', step.key ?? 'session-1', step.account);
    case 'acquire': {
      const lease = await scheduler.acquire({ providerId: 'example', affinityKey: step.key ?? 'session-1' });
      const accountId = lease.accountId;
      lease.release({ status: 'success' });
      return accountId;
    }
    case 'updatePool': await scheduler.updatePool('example', { affinity: step.affinity }); return null;
    case 'setAccounts': scripted.accounts = structuredClone(step.accounts); return null;
    case 'setAccountsError': scripted.accountsError = step.message === null ? undefined : step.message; return null;
    case 'setResolve': scripted.resolve[step.accountId] = structuredClone(step.entry); return null;
    case 'active': return announcement.getActiveAccount(provider, ctx);
    case 'auth': {
      const signal = step.signal === undefined || step.signal === 'none'
        ? undefined
        : step.signal === 'aborted' ? AbortSignal.abort() : new AbortController().signal;
      return announcement.resolveActiveAccountAuth(provider, ctx, signal);
    }
    case 'subscribe': handles.set(step.handle, announcement.onActiveAccountChanged(provider, listenerFor(step.listener))); return null;
    case 'unsubscribe': handles.get(step.handle)(); return null;
    case 'notify': announcement.notifyActiveAccountChanged(provider, ctx, step.account); return null;
    default: throw new Error(`unknown step ${step.op}`);
  }
}

const cases = { managed: [], announcement: [] };
for (const scenario of managedCases) cases.managed.push(await runManagedCase(scenario));
for (const scenario of announcementCases) cases.announcement.push(await runAnnouncementCase(scenario));
probe = null;

const output = {
  reference: 'bd97d65a36f541935dc91e33ba15d7910a30f9d7',
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
    process.stderr.write(`managed-reference.json differs at line ${index + 1}\n`);
    process.exit(1);
  }
  process.stdout.write('managed-reference.json matches the regenerated reference\n');
}
