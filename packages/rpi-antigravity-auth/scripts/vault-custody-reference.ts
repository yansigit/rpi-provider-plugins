// Scripted custody traces use the unchanged pinned implementation, including its outage cache.
import assert from 'node:assert/strict';
import { writeFile } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { pathToFileURL } from 'node:url';
const root = resolve(process.env.RPI_REFERENCE_DIR!);
const { ClaustrumScopedCustody } = await import(pathToFileURL(join(root, 'packages/core/src/common-auth-embedded/claustrum/custody.js')).href);
const { ClaustrumCredentialError } = await import(pathToFileURL(join(root, 'packages/core/node_modules/@cortexkit/claustrum-client/dist/index.js')).href);
const { ClaustrumConsumerError } = await import(pathToFileURL(join(root, 'packages/core/src/common-auth-embedded/claustrum/errors.js')).href);
const identity = { credentialId: 'id', credentialType: 'oauth', accountIdentity: 'account' };
const receipt = { credentialId: 'id', accountId: 'account', material: 'fixture-access', recordVersion: 3, expiresAtMs: 901000, projectId: 'project' };
const inventory = { view: 'view-1', rows: [{ id: 'id', credentialType: 'oauth', categories: ['antigravity-native'], operations: ['read'], refreshAdapter: 'antigravity', state: 'active', accountId: 'account' }] };
const token = { token: 'a'.repeat(64), token_generation: 1 };
type Step = Record<string, any>;
const cases: any[] = [];
const errorValue = (e: any) => ({ name: e.name, message: e.message, ...(e.kind !== undefined && { kind: e.kind }), ...(e.code !== undefined && { code: e.code }), ...(e.class !== undefined && { class: e.class }), ...(e.action !== undefined && { action: e.action }) });
const decodeError = (e: any) => e.type === 'consumer' ? new ClaustrumConsumerError(e.kind, e.message) : e.type === 'other' ? new Error('private fixture detail') : new ClaustrumCredentialError(e.code, e.class ?? 'transient', e.action ?? 'retry');
const flush = async () => { for (let i = 0; i < 12; i++) await Promise.resolve(); };
async function capture(name: string, steps: Step[]) {
  let now = 1000, nextTimer = 0, currentToken: any = token;
  const calls: any[] = [], logs: any[] = [], timers = new Map<number, any>(), timerTrace: any[] = [], results: any[] = [], attempts = new Map<string, any>();
  const responses: Record<string, any[]> = { get: [], list: [], report: [] };
  const held = new Map<string, (value: any) => void>();
  const pending = new Map<string, { cancel: AbortController; promise: Promise<any> }>();
  const call = async (op: string, input: any) => {
    calls.push({ op, input, at: now });
    let response = responses[op].shift();
    if (response?.hold) response = await new Promise(resolve => held.set(response.hold, resolve));
    if (response?.error) throw decodeError(response.error);
    return response?.value ?? (op === 'get' ? structuredClone(receipt) : op === 'list' ? structuredClone(inventory) : undefined);
  };
  const custody = new ClaustrumScopedCustody({
    family: { refreshAdapter: 'antigravity', category: 'antigravity-native', apiKeys: false }, requireAssertion: true,
    now: () => now,
    readToken: async () => { calls.push({ op: 'token', at: now }); if (currentToken.error) throw decodeError(currentToken.error); return currentToken; },
    client: { getScoped: (input: any) => call('get', input), listScoped: (input: any) => call('list', input), reportAuthFailureScoped: (input: any) => call('report', input), close() { calls.push({ op: 'close', at: now }); } },
    logger: { debug(...args: any[]) { logs.push(['debug', ...args]); }, warn(...args: any[]) { logs.push(['warn', ...args]); }, info(...args: any[]) { logs.push(['info', ...args]); } },
    setTimeoutImpl: (fn: () => void, delay: number) => { const timer = { id: ++nextTimer, at: now + delay, fn, unref() {} }; timers.set(timer.id, timer); timerTrace.push(['set', timer.id, delay]); return timer; },
    clearTimeoutImpl: (timer: any) => { timers.delete(timer.id); timerTrace.push(['clear', timer.id]); },
  });
  for (const step of steps) {
    try {
      let value: any;
      switch (step.op) {
        case 'reply': responses[step.method].push(step.response); break;
        case 'start': {
          const cancel = new AbortController();
          const promise = custody.authorize(step.identity ?? identity, cancel.signal, 'route').then((attempt: any) => ({ attempt }), (error: any) => ({ error }));
          pending.set(step.id, { cancel, promise }); break;
        }
        case 'cancel': pending.get(step.id)!.cancel.abort(); break;
        case 'release': assert(held.has(step.id)); held.get(step.id)!(step.response ?? {}); held.delete(step.id); break;
        case 'wait': {
          const result = await pending.get(step.id)!.promise;
          if (result.error) throw result.error;
          attempts.set(step.id, result.attempt);
          value = { metadata: JSON.parse(JSON.stringify(result.attempt)), accessToken: result.attempt.accessToken }; break;
        }
        case 'token': currentToken = step.value; break;
        case 'time': now = step.value; break;
        case 'timer': {
          const timer = [...timers.values()].sort((a, b) => a.at - b.at || a.id - b.id)[0];
          assert(timer, 'expected a scheduled probe'); timers.delete(timer.id); now = timer.at; timerTrace.push(['fire', timer.id]); timer.fn(); await flush(); break;
        }
        case 'authorize': { const attempt = await custody.authorize(step.identity ?? identity, undefined, step.route ?? 'route'); attempts.set(step.id, attempt); value = { metadata: JSON.parse(JSON.stringify(attempt)), accessToken: attempt.accessToken }; break; }
        case 'discover': value = await custody.discover(); break;
        case 'report': await custody.reportFailure(step.forged ? { ...attempts.get(step.id) } : attempts.get(step.id), step.status ?? 401, 'direct'); break;
        case 'reportTogether': await Promise.all([custody.reportFailure(attempts.get(step.id), 401, 'direct'), custody.reportFailure(attempts.get(step.id), 401, 'direct')]); break;
        case 'forget': custody.forget(step.id ?? 'id'); break;
        case 'retain': custody.retain(step.identities); break;
        case 'lost': custody.transportLost(step.error && decodeError(step.error)); break;
        case 'reachable': custody.reachable(); break;
        case 'listFailed': custody.listFailed(decodeError(step.error)); break;
        case 'state': value = custody.keptReceiptState(); break;
        case 'close': custody.close(); break;
        default: throw new Error('invalid fixture operation');
      }
      await flush(); results.push({ ok: true, ...(value !== undefined && { value }) });
    } catch (e) { await flush(); results.push({ error: errorValue(e) }); }
  }
  custody.close(); await flush();
  cases.push({ name, steps, results, calls, logs, timers: timerTrace });
}
const auth = { op: 'authorize', id: 'first' };
const lost = { op: 'lost' };
const outageReply = { op: 'reply', method: 'get', response: { error: { code: 'transport_error' } } };
await capture('fresh-per-send', [auth, { op: 'authorize', id: 'second' }, { op: 'state' }]);
await capture('report-once-per-receipt', [auth, { op: 'reportTogether', id: 'first' }, { op: 'report', id: 'first' }]);
await capture('report-provenance', [auth, { op: 'report', id: 'first', forged: true }]);
for (const status of [200, 400, 402, 403, 429, 500]) await capture(`ignore-report-${status}`, [auth, { op: 'report', id: 'first', status, forged: true }]);
await capture('report-failure-retried', [auth, { op: 'reply', method: 'report', response: { error: { code: 'not_found', class: 'permanent', action: 'gone' } } }, { op: 'report', id: 'first' }, { op: 'report', id: 'first' }]);
for (const code of ['transport_error', 'not_found', 'unknown-new-code']) await capture(`failed-get-${code}`, [auth, { op: 'reply', method: 'get', response: { error: { code } } }, { op: 'authorize', id: 'second' }, { op: 'state' }]);
await capture('outage-without-kept', [outageReply, auth, { op: 'state' }]);
await capture('kept-401-never-reported', [auth, outageReply, { op: 'authorize', id: 'kept' }, { op: 'report', id: 'kept' }, { op: 'state' }]);
for (const value of [840999, 841000, 841001]) await capture(`kept-expiry-${value}`, [auth, lost, { op: 'time', value }, { op: 'reply', method: 'list', response: { error: { code: 'transport_error' } } }, { op: 'authorize', id: 'kept' }, { op: 'state' }]);
for (const field of ['credentialType', 'accountIdentity']) await capture(`identity-changed-${field}`, [auth, lost, { op: 'authorize', id: 'other', identity: { ...identity, [field]: 'other' } }, { op: 'state' }]);
for (const value of [{ token: 'bad', token_generation: 1 }, { token: token.token, token_generation: 0 }, { error: { type: 'consumer', kind: 'not-enrolled', message: 'fixture enrollment missing' } }]) await capture(`enrollment-${cases.length}`, [auth, lost, { op: 'token', value }, { op: 'authorize', id: 'kept' }, { op: 'token', value: token }, { op: 'authorize', id: 'after' }]);
for (const code of ['transport_error', 'not_found']) await capture(`listing-${code}`, [auth, { op: 'reply', method: 'list', response: { error: { code } } }, { op: 'discover' }, { op: 'state' }, lost, { op: 'authorize', id: 'after' }]);
for (const identities of [[], [{ ...identity, state: 'active', enabled: true }], [{ ...identity, state: 'active', enabled: false }], [{ ...identity, state: 'disabled', enabled: true }], [{ ...identity, state: 'active', enabled: true, accountIdentity: 'other' }]]) await capture(`retain-${cases.length}`, [auth, { op: 'retain', identities }, lost, { op: 'authorize', id: 'after' }]);
await capture('reachable-clears-active-state', [auth, outageReply, { op: 'authorize', id: 'kept' }, { op: 'state' }, { op: 'reachable' }, { op: 'state' }]);
await capture('idle-probe-backoff', [lost, ...Array.from({ length: 6 }, () => [{ op: 'reply', method: 'list', response: { error: { code: 'transport_error' } } }, { op: 'timer' }]).flat(), { op: 'timer' }]);
await capture('close-is-idempotent', [auth, lost, { op: 'close' }, { op: 'close' }, { op: 'authorize', id: 'after' }, { op: 'discover' }, { op: 'report', id: 'first' }, { op: 'state' }]);
await capture('cancel-pending-get', [{ op: 'reply', method: 'get', response: { hold: 'get' } }, { op: 'start', id: 'pending' }, { op: 'cancel', id: 'pending' }, { op: 'wait', id: 'pending' }, { op: 'release', id: 'get' }, lost, auth]);
await capture('close-pending-get', [{ op: 'reply', method: 'get', response: { hold: 'get' } }, { op: 'start', id: 'pending' }, { op: 'close' }, { op: 'release', id: 'get' }, { op: 'wait', id: 'pending' }]);
await capture('old-report-retains-new-version', [auth, { op: 'reply', method: 'get', response: { value: { ...receipt, recordVersion: 4 } } }, { op: 'authorize', id: 'new' }, { op: 'report', id: 'first' }, lost, { op: 'reply', method: 'list', response: { error: { code: 'transport_error' } } }, { op: 'authorize', id: 'kept' }]);
await capture('probe-single-flight', [auth, lost, { op: 'reply', method: 'list', response: { hold: 'list' } }, { op: 'authorize', id: 'kept' }, { op: 'authorize', id: 'kept-again' }, { op: 'release', id: 'list', response: { error: { code: 'transport_error' } } }, { op: 'authorize', id: 'too-soon' }, { op: 'time', value: 2000 }, { op: 'authorize', id: 'ready' }]);
await capture('unknown-error-is-redacted', [auth, { op: 'reply', method: 'get', response: { error: { type: 'other' } } }, { op: 'authorize', id: 'second' }, lost, { op: 'authorize', id: 'after' }]);
await capture('report-keeps-issuing-enrollment', [auth, { op: 'token', value: { token: 'b'.repeat(64), token_generation: 2 } }, { op: 'report', id: 'first' }, { op: 'authorize', id: 'second' }]);
await capture('pending-send-keeps-issuing-enrollment', [{ op: 'reply', method: 'get', response: { hold: 'get' } }, { op: 'start', id: 'pending' }, { op: 'token', value: { token: 'b'.repeat(64), token_generation: 2 } }, { op: 'release', id: 'get' }, { op: 'wait', id: 'pending' }, { op: 'report', id: 'pending' }]);
assert(cases.length >= 30);
await writeFile(resolve(process.env.RPI_REFERENCE_OUTPUT!), JSON.stringify({ runtime: process.version, cases }, null, 2) + '\n');
console.log(`${cases.length} original custody state-machine traces captured`);
