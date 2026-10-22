// Differential reference for the Claustrum custody response wrapper: the opaque,
// frozen receipt object `authorize()` returns. Its enumerable surface is the
// receipt metadata, its access token is non-enumerable, and provenance is held
// outside the object so a copy cannot report a failure.
//
//   node scripts/vault-custody-wrapper-reference.mjs            # check mode
//   node scripts/vault-custody-wrapper-reference.mjs --write    # write the fixture
//
// This is not the credential-slot transition (backup/marker/restore) and not
// the source mode synchronization; those are separate evidence entries.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { readFileSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const root = resolve(process.env.RPI_REFERENCE_DIR);
const pin = 'a97f9055017f3c1546afbfe61a19d50b428be3d8';
const custodyFile = 'packages/core/src/common-auth-embedded/claustrum/custody.js';
const clientFile = 'packages/core/node_modules/@cortexkit/claustrum-client/dist/index.js';

const head = execFileSync('git', ['-C', root, 'rev-parse', 'HEAD'], { encoding: 'utf8' }).trim();
if (head !== pin) throw new Error(`reference ${head} != ${pin}`);
if (process.version !== 'v24.22.0') throw new Error(`runtime ${process.version} != v24.22.0`);

const { ClaustrumScopedCustody } = await import(pathToFileURL(join(root, custodyFile)).href);

const identity = { credentialId: 'id', credentialType: 'oauth', accountIdentity: 'account' };
const tokenA = 'a'.repeat(64);
const tokenB = 'b'.repeat(64);
const receipt = { credentialId: 'id', accountId: 'account', material: 'fixture-access', recordVersion: 3, expiresAtMs: 901000, projectId: 'project' };
const withoutProject = { ...receipt };
delete withoutProject.projectId;

const flush = async () => {
  for (let i = 0; i < 12; i++) await Promise.resolve();
};

/** Capture one wrapper through the unchanged upstream implementation. */
async function capture(name, { served, kept = false, changedToken = false }) {
  let now = 1000;
  let currentToken = tokenA;
  const calls = [];
  const logs = [];
  const custody = new ClaustrumScopedCustody({
    family: { refreshAdapter: 'antigravity', category: 'antigravity-native', apiKeys: false },
    requireAssertion: true,
    now: () => now,
    readToken: async () => {
      calls.push({ op: 'token', at: now });
      return { token: currentToken, token_generation: currentToken === tokenA ? 1 : 2 };
    },
    client: {
      getScoped: async (input) => {
        calls.push({ op: 'get', input, at: now });
        return structuredClone(served);
      },
      listScoped: async (input) => {
        calls.push({ op: 'list', input, at: now });
        return { view: 'view-1', rows: [], skipped: [] };
      },
      reportAuthFailureScoped: async (input) => {
        calls.push({ op: 'report', input, at: now });
        return {};
      },
      close() {
        calls.push({ op: 'close', at: now });
      },
    },
    logger: {
      debug(...args) {
        logs.push(['debug', ...args]);
      },
      warn(...args) {
        logs.push(['warn', ...args]);
      },
      info(...args) {
        logs.push(['info', ...args]);
      },
    },
    setTimeoutImpl: () => ({ unref() {} }),
    clearTimeoutImpl: () => {},
  });

  const fresh = await custody.authorize(identity, undefined, 'route');
  let attempt = fresh;
  if (kept) {
    if (changedToken) currentToken = tokenB;
    custody.transportLost();
    attempt = await custody.authorize(identity, undefined, 'route');
  }
  await flush();

  const accessToken = attempt.accessToken;
  const json = JSON.stringify(attempt);
  const spread = JSON.parse(JSON.stringify({ ...attempt }));

  // A copied wrapper has no provenance: the custody refuses to report it.
  let provenanceErrorKind = null;
  try {
    await custody.reportFailure({ ...attempt }, 401, 'direct');
  } catch (error) {
    provenanceErrorKind = error.kind ?? error.name;
  }
  await flush();

  // Reporting the original is the observable provenance: a fresh wrapper sends
  // the issuing enrollment token; a kept wrapper is never reported.
  const before = calls.filter(call => call.op === 'report').length;
  await custody.reportFailure(attempt, 401, 'direct');
  await flush();
  const reports = calls.filter(call => call.op === 'report');
  const reportToken = reports.length > before ? reports[before].input.enrollmentToken : null;

  assert.equal(Object.isFrozen(attempt), true, `${name} wrapper frozen`);
  assert.equal(Object.isExtensible(attempt), false, `${name} wrapper non-extensible`);

  custody.close();
  await flush();
  return {
    name,
    served,
    kept,
    changedToken,
    metadata: JSON.parse(json),
    keys: Object.keys(attempt),
    accessToken,
    spread,
    tokenInJson: json.includes(accessToken),
    fromKeptReceipt: attempt.fromKeptReceipt === true,
    provenanceErrorKind,
    reportToken,
    reportCalls: reports.length - before,
  };
}

const cases = [
  await capture('fresh-asserted-with-project', { served: receipt }),
  await capture('fresh-asserted-without-project', { served: withoutProject }),
  await capture('kept-receipt', { served: receipt, kept: true }),
  await capture('kept-receipt-token-rotated', { served: receipt, kept: true, changedToken: true }),
];

const fixture = {
  reference: pin,
  runtime: process.version,
  sourceHashes: Object.fromEntries(
    [custodyFile, clientFile].map(file => [
      file,
      createHash('sha256').update(readFileSync(join(root, file))).digest('hex'),
    ]),
  ),
  cases,
};
const text = JSON.stringify(fixture, null, 2) + '\n';
if (process.argv.includes('--write')) {
  writeFileSync(resolve(process.env.RPI_REFERENCE_OUTPUT ?? fileURLToPath(new URL('../tests/fixtures/vault-custody-wrapper.json', import.meta.url))), text);
  console.log(JSON.stringify({ wrote: cases.length, runtime: process.version }));
} else {
  const output = resolve(process.env.RPI_REFERENCE_OUTPUT ?? fileURLToPath(new URL('../tests/fixtures/vault-custody-wrapper.json', import.meta.url)));
  if (readFileSync(output, 'utf8') !== text) {
    console.error('custody wrapper reference differs from the committed fixture');
    process.exit(1);
  }
  console.log(JSON.stringify({ checked: cases.length, runtime: process.version }));
}
