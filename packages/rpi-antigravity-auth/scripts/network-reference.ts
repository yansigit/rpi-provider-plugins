// Each failure runs in a child because an unhandled upstream socket error can terminate Node.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { readFileSync, writeFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import * as net from 'node:net';
import * as tls from 'node:tls';
import { resolve, join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
const root = resolve(process.env.RPI_REFERENCE_DIR!);
const pin = 'a97f9055017f3c1546afbfe61a19d50b428be3d8';
const self = fileURLToPath(import.meta.url);
const certificate = resolve(fileURLToPath(new URL('../tests/fixtures/tls/server-cert.pem', import.meta.url)));
const key = fileURLToPath(new URL('../tests/fixtures/tls/server-key.pem', import.meta.url));
const scenario = process.argv[2];
if (!scenario) {
  assert.equal(execFileSync('git', ['-C', root, 'rev-parse', 'HEAD'], {encoding: 'utf8'}).trim(), pin);
  const cases = ['direct-timeout', 'proxy-head-timeout', 'proxy-handshake-timeout', 'header-timeout', 'body-idle', 'body-cancel', 'body-drop'].map(scenario => ({scenario, ...JSON.parse(execFileSync(process.execPath, [self, scenario], {encoding: 'utf8', timeout: 5000, env: {...process.env, NODE_EXTRA_CA_CERTS: certificate}}))}));
  const sourceHashes = Object.fromEntries(['packages/core/src/agy-transport.ts', 'packages/core/dist/agy-transport.js'].map(file => [file, createHash('sha256').update(readFileSync(join(root, file))).digest('hex')]));
  writeFileSync(process.env.RPI_REFERENCE_OUTPUT!, JSON.stringify({reference: pin, runtime: process.version, sourceHashes, cases}, null, 2) + '\n');
  console.log(JSON.stringify(cases));
} else {
  for (const name of ['HTTPS_PROXY', 'https_proxy', 'ALL_PROXY', 'all_proxy', 'NO_PROXY', 'no_proxy']) delete process.env[name];
  let finished = false;
  const debug: string[] = [];
  function report(outcome: string, message?: string, more = {}) {
    if (finished) return;
    finished = true;
    process.stdout.write(JSON.stringify({outcome, message, debug, ...more}), () => process.exit(0));
  }
  process.on('uncaughtException', (error: Error) => report('uncaughtException', error.message));
  setTimeout(() => report('unsettled'), 1500);
  const transport = await import(pathToFileURL(join(root, 'packages/core/dist/agy-transport.js')).href);
  let peerClosed = false;
  const plain = scenario.startsWith('proxy-') || scenario === 'direct-timeout';
  const accept = (socket: net.Socket) => {
    socket.on('error', () => {});
    socket.on('close', () => { peerClosed = true; });
    let replied = false;
    socket.on('data', () => {
      if (replied) return;
      replied = true;
      if (scenario === 'proxy-handshake-timeout') socket.write('HTTP/1.1 200 OK\r\n\r\n');
      if (scenario.startsWith('body-')) socket.write('HTTP/1.1 200 OK\r\n\r\nfirst');
    });
  };
  const server = plain ? net.createServer(accept) : tls.createServer({key: readFileSync(key), cert: readFileSync(certificate)}, accept);
  await new Promise<void>(done => server.listen(0, '127.0.0.1', done));
  const port = (server.address() as net.AddressInfo).port;
  if (scenario.startsWith('proxy-')) process.env.HTTPS_PROXY = `http://127.0.0.1:${port}`;
  const controller = new AbortController();
  try {
    const response = await transport.fetchWithAgyCliTransport(`https://localhost:${port}/json`, {body: 'x'}, {signal: controller.signal, timeoutMs: 100, idleTimeoutMs: 100, onDebug: (line: string) => debug.push(line)});
    const reader = response.body.getReader();
    const first = await reader.read();
    assert.equal(Buffer.from(first.value).toString(), 'first');
    if (scenario === 'body-cancel') controller.abort();
    if (scenario === 'body-drop') {
      await reader.cancel();
      await new Promise(done => setTimeout(done, 200));
      report('cancelled-reader', undefined, {peerClosed});
    } else {
      await reader.read();
      report('resolved');
    }
  } catch (error: any) { report('rejected', error.message); }
}
