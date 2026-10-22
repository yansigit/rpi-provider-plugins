// Retired-model preflight counters reference: unchanged upstream Pi 1.3.0
// `stream.ts` through the integrated host, offline, with injected clock/IDs.
// Counts admissions, network sends and request-ID allocations per model so a
// retired alias must refuse before any auth or network work.
//
//   RPI_REFERENCE_DIR=<...>/antigravity RPI_REFERENCE_INTEGRATED=<...>/integrated \
//   RPI_REFERENCE_OUTPUT=tests/fixtures/ag1-retired.json \
//   node --experimental-test-module-mocks scripts/ag1-retired-reference.mjs --write
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { copyFile, mkdtemp, readFile, rm, symlink, writeFile } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { tmpdir } from 'node:os';
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

const aiRoot = join(integrated, 'node_modules/@earendil-works/pi-ai');
const manifest = JSON.parse(await readFile(join(aiRoot, 'package.json'), 'utf8'));
assert.equal(manifest.version, '1.3.0');
const aiPath = pathToFileURL(join(aiRoot, manifest.exports['.'].import)).href;
const ai = await import(aiPath);
let events = [];
mock.module(aiPath, {
  exports: {
    ...ai,
    createAssistantMessageEventStream: () => {
      const stream = ai.createAssistantMessageEventStream();
      const push = stream.push.bind(stream);
      stream.push = (event) => {
        push(event);
        events.push(structuredClone(event));
      };
      return stream;
    },
  },
});

const crypto = await import('node:crypto');
let sequence = 0;
let ids = [];
const nextId = () => {
  const value = `00000000-0000-4000-8000-${String(++sequence).padStart(12, '0')}`;
  ids.push(value);
  return value;
};
mock.module('node:crypto', {
  exports: {
    ...Object.fromEntries(Object.entries(crypto).filter(([key]) => key !== 'default')),
    randomUUID: nextId,
  },
});
globalThis.crypto.randomUUID = nextId;
Date.now = () => 1784285195116;
globalThis.performance.now = () => 1000;
globalThis.fetch = async () => {
  throw new Error('unexpected local project network request');
};

const corePath = pathToFileURL(
  join(integrated, 'node_modules/@cortexkit/antigravity-auth-core/dist/index.js'),
).href;
const core = await import(corePath);
let requests = [];
const text = 'data: {"candidates":[{"content":{"parts":[{"text":"hello"}]},"finishReason":"STOP"}]}\n\n';
mock.module(corePath, {
  exports: {
    ...core,
    fetchWithAgyCliTransport: async (url, init) => {
      requests.push({ url, method: init.method, headers: init.headers, body: init.body });
      return new Response(text);
    },
  },
});

const temporary = await mkdtemp(join(tmpdir(), 'rpi-agy-retired-reference-'));
const fixture = JSON.parse(
  await readFile(new URL('../tests/fixtures/stream.json', import.meta.url), 'utf8'),
);
const base = fixture.cases[0];
const models = [
  'antigravity-gemini-3.8-flash',
  'antigravity-gemini-3.7-flash',
  'gemini-3.6-flash',
  'antigravity-gemini-3.1-pro-high',
  'gemini-3.1-pro',
  'gemini-claude-opus-4-6',
  'claude-sonnet-4-6-thinking',
  'gemini-pro-agent',
  'antigravity-gemini-3.7-flash-preview-customtools',
  'antigravity-gemini-3.8-flash-high',
];
const cases = [];
try {
  for (const file of ['stream.ts', 'convert.ts', 'credential-cache.ts']) {
    await copyFile(join(root, 'packages/pi/src', file), join(temporary, file));
  }
  await symlink(join(integrated, 'node_modules'), join(temporary, 'node_modules'), 'junction');
  const { streamCortexKitAntigravity } = await import(
    pathToFileURL(join(temporary, 'stream.ts')).href
  );
  const { rememberPackedRefresh } = await import(
    pathToFileURL(join(temporary, 'credential-cache.ts')).href
  );
  for (const id of models) {
    events = [];
    requests = [];
    ids = [];
    sequence = 0;
    let admissions = 0;
    const model = { ...base.model, id };
    const options = { sessionId: id, apiKey: 'fixture-access' };
    const packed = 'fixture-refresh|fixture-project|fixture-managed';
    rememberPackedRefresh('fixture-access', packed);
    const controller = new AbortController();
    const result = await streamCortexKitAntigravity(
      model,
      base.context,
      { ...options, signal: controller.signal },
      async () => {
        ++admissions;
      },
    ).result();
    cases.push({
      model,
      context: base.context,
      options,
      packed,
      admissions,
      ids: [...ids],
      requests: structuredClone(requests),
      events: structuredClone(events),
      result: structuredClone(result),
    });
  }
  const files = ['packages/pi/src/stream.ts', 'packages/pi/src/credential-cache.ts'];
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
      { reference: pin, host: manifest.version, runtime: process.version, body: text, sourceHashes, cases },
      null,
      2,
    ) + '\n';
  if (write) {
    await writeFile(output, document);
    console.log(JSON.stringify({ cases: cases.length }));
  } else {
    assert.equal(await readFile(output, 'utf8'), document, 'fixture differs; run with --write');
    console.log(JSON.stringify({ checked: true, cases: cases.length }));
  }
} finally {
  await rm(temporary, { recursive: true, force: true });
}
