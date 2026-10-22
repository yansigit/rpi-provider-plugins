// OAuth/project diagnostics reference: unchanged upstream TS through loopback fake
// servers, with an injected clock, deterministic entropy and captured traces.
//
// Run from the package directory:
//   RPI_REFERENCE_DIR=<provider-references/antigravity> \
//   RPI_REFERENCE_INTEGRATED=<provider-references/integrated> \
//   RPI_REFERENCE_OUTPUT=tests/fixtures/ag1-oauth.json \
//   node --experimental-test-module-mocks scripts/ag1-oauth-reference.mjs --write
// Without --write the regenerated document is compared to the committed fixture.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { readFile, writeFile } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { createServer, request as httpRequest } from 'node:http';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { mock } from 'node:test';

const root = resolve(process.env.RPI_REFERENCE_DIR);
const integrated = resolve(process.env.RPI_REFERENCE_INTEGRATED);
const output = resolve(process.env.RPI_REFERENCE_OUTPUT);
const pin = 'a97f9055017f3c1546afbfe61a19d50b428be3d8';
assert.equal(
  execFileSync('git', ['-C', root, 'rev-parse', 'HEAD'], { encoding: 'utf8' }).trim(),
  pin,
);
const write = process.argv.includes('--write');

const crypto = await import('node:crypto');
const entropy = Buffer.from(Array.from({ length: 32 }, (_, i) => i));
mock.module('node:crypto', {
  exports: {
    ...Object.fromEntries(Object.entries(crypto).filter(([key]) => key !== 'default')),
    randomBytes: (length) => {
      assert.ok(length <= 32, 'injected entropy is bounded');
      return entropy.subarray(0, length);
    },
  },
});

const clock = 1784285195116;
let now = clock;
Date.now = () => now;
const originalTimeout = globalThis.setTimeout;
globalThis.setTimeout = (callback, ms, ...args) => {
  if (ms === 5000 || ms === 7) {
    trace.push({ kind: 'wait', milliseconds: ms });
    now += ms;
    queueMicrotask(callback);
    return 0;
  }
  return originalTimeout(callback, ms, ...args);
};

// One loopback server per run serves every scripted response; nothing leaves 127.0.0.1.
let queue = [];
let trace = [];
const server = createServer((incoming, outgoing) => {
  const chunks = [];
  incoming.on('data', (chunk) => chunks.push(chunk));
  incoming.on('end', () => {
    const next = queue.shift();
    assert.ok(next, 'unexpected network request');
    const body = next.raw ?? JSON.stringify(next.json);
    outgoing.writeHead(next.status, next.statusText, { 'content-type': 'application/json' });
    outgoing.end(body);
  });
});
await new Promise((ready) => server.listen(0, '127.0.0.1', ready));
const address = `127.0.0.1:${server.address().port}`;

function roundtrip() {
  return new Promise((ready, reject) => {
    const call = httpRequest(
      { host: '127.0.0.1', port: server.address().port, path: '/', method: 'POST' },
      (response) => {
        const chunks = [];
        response.on('data', (chunk) => chunks.push(chunk));
        response.on('end', () =>
          ready({
            status: response.statusCode,
            statusText: response.statusMessage ?? '',
            body: Buffer.concat(chunks).toString('utf8'),
          }),
        );
      },
    );
    call.on('error', reject);
    call.end();
  });
}

/// Mirrors the Rust `auth::Port` trace and reads, through a real loopback socket.
async function respond(url, init = {}, agy = false, timeoutMs = 15000) {
  trace.push({
    kind: 'send',
    request: {
      url,
      method: init.method ?? 'GET',
      headers: Object.entries(init.headers ?? {}),
      ...(init.body === undefined ? {} : { body: String(init.body) }),
      agy,
      timeoutMs,
    },
  });
  const { status, statusText, body } = await roundtrip();
  return {
    ok: status >= 200 && status < 300,
    status,
    statusText,
    text: async () => {
      trace.push({ kind: 'read' });
      return body;
    },
    json: async () => {
      trace.push({ kind: 'read' });
      return JSON.parse(body);
    },
  };
}

globalThis.fetch = (url, init) => respond(String(url), init, false, 15000);
for (const base of [
  join(root, 'packages/core'),
  join(integrated, 'node_modules/@cortexkit/antigravity-auth-core'),
]) {
  mock.module(pathToFileURL(join(base, 'dist/agy-transport.js')).href, {
    exports: {
      DEFAULT_AGY_RESPONSE_HEADER_TIMEOUT_MS: 180000,
      DEFAULT_AGY_IDLE_TIMEOUT_MS: 180000,
      fetchWithAgyCliTransport: (url, init, options = {}) =>
        respond(url, init, true, options.timeoutMs ?? 180000),
    },
  });
}

const core = await import(pathToFileURL(join(root, 'packages/core/dist/antigravity/oauth.js')).href);
const helpers = await import(pathToFileURL(join(root, 'packages/core/dist/auth.js')).href);
const project = await import(pathToFileURL(join(root, 'packages/core/dist/project.js')).href);
const cache = await import(pathToFileURL(join(root, 'packages/pi/src/credential-cache.ts')).href);
const redaction = await import(
  pathToFileURL(join(root, 'packages/core/src/credential-redaction.ts')).href
);

const host = JSON.parse(
  await readFile(join(integrated, 'node_modules/@earendil-works/pi-ai/package.json'), 'utf8'),
);
assert.equal(host.version, '1.3.0');

const token = { status: 200, json: { access_token: 'access-test', refresh_token: 'refresh-test', expires_in: 1800 } };
const user = { status: 200, json: { email: 'fixture@example.test', name: 'Alice' } };

// ---- OAuth exchange / refresh: malformed bodies and expiry units ----
const exchange = [];
async function exchangeCase(name, responses, project = '', rawState) {
  queue = structuredClone(responses);
  trace = [];
  const state =
    rawState === undefined
      ? new URL((await core.authorizeAntigravity(project)).url).searchParams.get('state')
      : Buffer.from(JSON.stringify(rawState)).toString('base64url');
  const result = await core.exchangeAntigravity('code +😀', state);
  exchange.push({
    name,
    responses,
    code: 'code +😀',
    state,
    project,
    result,
    trace: structuredClone(trace),
    remaining: queue.length,
  });
}
await exchangeCase('malformed token body', [{ status: 200, raw: 'not json at all' }], 'project-one');
await exchangeCase('truncated token body', [{ status: 200, raw: '{"access_token":' }], 'project-one');
await exchangeCase('html token error body', [{ status: 400, raw: '<html>denied</html>' }]);
await exchangeCase('malformed userinfo body', [token, { status: 200, raw: 'nope' }], 'project-one');
await exchangeCase('malformed discovery body', [token, user, { status: 200, raw: 'load{' }, token]);
await exchangeCase('malformed discovery then second endpoint', [token, user, { status: 200, raw: 'load{' }, { status: 200, json: { cloudaicompanionProject: { id: 'second-project' } } }]);
await exchangeCase('empty token body', [{ status: 200, raw: '' }], 'project-one');
await exchangeCase('whitespace token body', [{ status: 200, raw: '   \n\t' }], 'project-one');
for (const seconds of [null, '3600', -1, 0, 0.25]) {
  await exchangeCase(`expiry ${JSON.stringify(seconds)}`, [{ status: 200, json: { ...token.json, expires_in: seconds } }, user], 'project-one');
}

const refresh = [];
async function refreshCase(name, packed, responses) {
  queue = structuredClone(responses);
  trace = [];
  let result;
  try {
    result = { value: await core.refreshAntigravityToken(packed) };
  } catch (error) {
    result = { error: error.message };
  }
  refresh.push({ name, packed, responses, result, trace: structuredClone(trace), remaining: queue.length });
}
await refreshCase('malformed body', 'refresh', [{ status: 200, raw: 'invalid' }]);
await refreshCase('truncated body', 'refresh', [{ status: 200, raw: '{"access_token":' }]);
await refreshCase('empty body', 'refresh', [{ status: 200, raw: '' }]);
await refreshCase('array body', 'refresh', [{ status: 200, raw: '[1,2,3' }]);
await refreshCase('non-ok malformed', 'refresh', [{ status: 500, statusText: 'Oops', raw: 'boom' }]);
for (const seconds of [null, '3600', -1, 0, 0.25]) {
  await refreshCase(`expiry ${JSON.stringify(seconds)}`, 'refresh', [{ status: 200, json: { access_token: 'next', expires_in: seconds } }]);
}

// ---- Project discovery/onboarding: malformed bodies ----
const projectCases = [];
async function projectCase(name, responses, operations) {
  project.invalidateProjectContextCache();
  queue = structuredClone(responses);
  trace = [];
  now = clock;
  const results = [];
  for (const operation of operations) {
    try {
      let value;
      switch (operation.op) {
        case 'ensure':
          value = await project.ensureProjectContext(operation.auth);
          break;
        case 'onboard':
          value = await project.onboardManagedProject(operation.access, operation.tier, operation.project, operation.attempts, operation.delay);
          break;
        case 'advance':
          now += operation.milliseconds;
          break;
        case 'clearFailures':
          project.clearProvisionFailedKeys();
          break;
      }
      results.push({ value: value ?? null });
    } catch (error) {
      results.push({ error: error.message });
    }
  }
  assert.equal(queue.length, 0, `unused scripted responses: ${name}`);
  projectCases.push({ name, responses, operations, results, trace: structuredClone(trace), clock: now });
}
const auth = (refresh = 'refresh|requested', access = 'access') => ({ type: 'oauth', refresh, access, expires: clock + 3600000 });
const ensure = (value = auth()) => ({ op: 'ensure', auth: value });
await projectCase('malformed discovery falls back to next endpoint', [{ status: 200, raw: 'load{' }, { status: 200, json: { cloudaicompanionProject: 'managed' } }], [ensure()]);
await projectCase('malformed discovery then onboarding', [{ status: 200, raw: 'load{' }, { status: 200, json: { allowedTiers: [{ id: 'free', isDefault: true }] } }, { status: 200, json: { done: true, response: { cloudaicompanionProject: { id: 'created' } } } }], [ensure()]);
await projectCase('malformed onboard body', [{ status: 200, raw: 'load{' }, { status: 200, json: { allowedTiers: [{ id: 'free' }] } }, { status: 200, raw: 'onboard{' }, { status: 403 }], [ensure()]);
await projectCase('malformed onboard then second endpoint', [{ status: 200, raw: 'load{' }, { status: 200, json: { allowedTiers: [{ id: 'free' }] } }, { status: 200, raw: 'onboard{' }, { status: 200, json: { done: true, response: { cloudaicompanionProject: { id: 'second' } } } }], [ensure()]);
await projectCase('cache reuse within TTL', [{ status: 200, json: { cloudaicompanionProject: 'managed' } }], [ensure(), ensure()]);
await projectCase('cache eviction at TTL boundary', [{ status: 200, json: { cloudaicompanionProject: 'managed' } }, { status: 200, json: { cloudaicompanionProject: 'managed' } }], [ensure(), { op: 'advance', milliseconds: 1799999 }, ensure(), { op: 'advance', milliseconds: 1 }, ensure()]);
await projectCase('provision failure then cached fallback', [{ status: 403 }, { status: 403 }, { status: 403 }, { status: 403 }], [ensure(), ensure()]);

// ---- Packed-refresh cache eviction (four entries) ----
const cacheTrace = [];
for (const [access, refresh] of [['a', 'one'], ['b', 'two'], ['c', 'three'], ['d', 'four'], ['a', 'replace'], ['e', 'five'], ['', 'ignored']]) {
  cache.rememberPackedRefresh(access, refresh);
  cacheTrace.push({ access, refresh, values: Object.fromEntries(['', 'a', 'b', 'c', 'd', 'e'].map((key) => [key, cache.getPackedRefresh(key) ?? null])) });
}

// ---- Error redaction over the malformed-JSON diagnostics ----
const redactionCases = [];
for (const input of [
  'Antigravity OAuth exchange failed: not json at all',
  'Antigravity OAuth exchange failed: {"error":"invalid_grant"}',
  'Antigravity token refresh failed (400 Bad Request) - Bearer opaque-selected-token',
  'loadCodeAssist error at https://cloudcode-pa.googleapis.com: Unexpected token \'l\', "load{" is not valid JSON',
  'project=private-project-123456 failed',
]) {
  for (const credentials of [[], ['opaque-selected-token']]) {
    for (const projects of [[], ['private-project-123456']]) {
      redactionCases.push({ input, credentials, projects, result: redaction.redactCredentialText(input, credentials, projects) });
    }
  }
}

// ---- Expiry helpers (milliseconds) ----
const expiries = [null, {}, '1', -5, 0, 1, 0.25, 3600].map((seconds) => ({
  seconds,
  result: helpers.calculateTokenExpiry(clock, seconds),
}));

const files = [
  'packages/core/src/antigravity/oauth.ts',
  'packages/core/src/project.ts',
  'packages/core/src/auth.ts',
  'packages/core/src/credential-redaction.ts',
  'packages/pi/src/credential-cache.ts',
];
const sourceHashes = Object.fromEntries(
  await Promise.all(
    files.map(async (file) => [
      file,
      createHash('sha256').update(await readFile(join(root, file))).digest('hex'),
    ]),
  ),
);

const document =
  JSON.stringify(
    {
      reference: pin,
      host: host.version,
      runtime: process.version,
      clock,
      sourceHashes,
      exchange,
      refresh,
      project: projectCases,
      cache: { trace: cacheTrace },
      redaction: redactionCases,
      expiries,
    },
    null,
    2,
  ) + '\n';
server.close();
if (write) {
  await writeFile(output, document);
  console.log(JSON.stringify({ exchange: exchange.length, refresh: refresh.length, project: projectCases.length, cache: cacheTrace.length, redaction: redactionCases.length }));
} else {
  const committed = await readFile(output, 'utf8');
  assert.equal(document, committed, 'fixture differs; run with --write');
  console.log(JSON.stringify({ checked: true, exchange: exchange.length, refresh: refresh.length, project: projectCases.length }));
}
