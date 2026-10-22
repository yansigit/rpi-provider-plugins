// Differential reference for AG-2: the trust store Node's switches select and the
// OpenSSL chain diagnostics the AGY transport reports for a loopback TLS peer.
//
//   node scripts/ag2-trust-reference.mjs            # regenerate and compare (check mode)
//   node scripts/ag2-trust-reference.mjs --write    # write tests/fixtures/ag2-trust.json
//   node scripts/ag2-trust-reference.mjs child <url>  # one fresh-process trust probe
//
// Every case runs in a fresh Node process because NODE_EXTRA_CA_CERTS,
// NODE_USE_SYSTEM_CA, NODE_OPTIONS and SSL_CERT_FILE are captured before
// application code. Nothing here touches a real service: both peers are
// loopback TLS servers built from the committed disposable fixtures.
import { execFile, execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { readFileSync, writeFileSync } from 'node:fs';
import { promisify } from 'node:util';
import * as tls from 'node:tls';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { join, resolve } from 'node:path';

const root = resolve(process.env.RPI_REFERENCE_DIR);
const pin = 'a97f9055017f3c1546afbfe61a19d50b428be3d8';
const self = fileURLToPath(import.meta.url);
const tlsDir = resolve(fileURLToPath(new URL('../tests/fixtures/tls/', import.meta.url)));
const placeholders = {
  '@cert@': join(tlsDir, 'server-cert.pem'),
  '@key@': join(tlsDir, 'server-key.pem'),
  '@leaf@': join(tlsDir, 'leaf-cert.pem'),
  '@leafkey@': join(tlsDir, 'leaf-key.pem'),
  '@ca@': join(tlsDir, 'ca-cert.pem'),
  '@certdir@': tlsDir,
};
const trustVars = ['NODE_USE_SYSTEM_CA', 'NODE_OPTIONS', 'NODE_EXTRA_CA_CERTS', 'SSL_CERT_FILE', 'SSL_CERT_DIR'];

function resolveEnv(set) {
  return Object.fromEntries(Object.entries(set).map(([key, value]) => [key, placeholders[value] ?? value]));
}

if (process.argv[2] === 'child') {
  const url = process.argv[3];
  const transport = await import(pathToFileURL(join(root, 'packages/core/dist/agy-transport.js')).href);
  const result = { trusted: false, message: null };
  try {
    const response = await transport.fetchWithAgyCliTransport(url, { body: 'x' }, { timeoutMs: 3000 });
    result.trusted = response.status === 200 && (await response.text()) === 'ok';
  } catch (error) {
    result.message = error.message;
  }
  process.stdout.write(JSON.stringify(result));
  process.exit(0);
}

assert_reference();
const cases = [
  { name: 'default', server: 'self-signed', set: {} },
  { name: 'extra-self-signed', server: 'self-signed', set: { NODE_EXTRA_CA_CERTS: '@cert@' } },
  { name: 'leaf-unknown-ca', server: 'leaf', set: {} },
  { name: 'leaf-extra-ca', server: 'leaf', set: { NODE_EXTRA_CA_CERTS: '@ca@' } },
  { name: 'system-ca', server: 'self-signed', set: { NODE_USE_SYSTEM_CA: '1' } },
  { name: 'system-ca-extra', server: 'self-signed', set: { NODE_USE_SYSTEM_CA: '1', NODE_EXTRA_CA_CERTS: '@cert@' } },
  { name: 'system-ca-ssl-file', server: 'self-signed', set: { NODE_USE_SYSTEM_CA: '1', SSL_CERT_FILE: '@cert@' } },
  { name: 'system-ca-ssl-dir', server: 'self-signed', set: { NODE_USE_SYSTEM_CA: '1', SSL_CERT_DIR: '@certdir@' } },
  { name: 'system-ca-option', server: 'self-signed', set: { NODE_OPTIONS: '--use-system-ca', SSL_CERT_FILE: '@cert@' } },
  { name: 'system-ca-off', server: 'self-signed', set: { NODE_USE_SYSTEM_CA: '0', SSL_CERT_FILE: '@cert@' } },
  { name: 'ssl-file-only', server: 'self-signed', set: { SSL_CERT_FILE: '@cert@' } },
  { name: 'openssl-ca', server: 'self-signed', set: { NODE_OPTIONS: '--use-openssl-ca' } },
  { name: 'openssl-ca-ssl-file', server: 'self-signed', set: { NODE_OPTIONS: '--use-openssl-ca', SSL_CERT_FILE: '@cert@' } },
  { name: 'openssl-ca-extra', server: 'self-signed', set: { NODE_OPTIONS: '--use-openssl-ca', NODE_EXTRA_CA_CERTS: '@cert@' } },
  { name: 'system-ca-leaf-ca-file', server: 'leaf', set: { NODE_USE_SYSTEM_CA: '1', SSL_CERT_FILE: '@ca@' } },
  { name: 'openssl-ca-leaf-ca-file', server: 'leaf', set: { NODE_OPTIONS: '--use-openssl-ca', SSL_CERT_FILE: '@ca@' } },
];

const servers = {};
async function start(which, cert, key) {
  const server = tls.createServer({ key: readFileSync(key), cert: readFileSync(cert) }, socket => {
    socket.on('error', () => {});
    socket.on('data', () => socket.write('HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok'));
  });
  await new Promise(done => server.listen(0, '127.0.0.1', done));
  servers[which] = server;
  return `https://localhost:${server.address().port}/json`;
}
const urls = {
  'self-signed': await start('self-signed', placeholders['@cert@'], placeholders['@key@']),
  leaf: await start('leaf', placeholders['@leaf@'], placeholders['@leafkey@']),
};

const run = promisify(execFile);

const results = [];
for (const test of cases) {
  const env = { ...process.env };
  for (const name of trustVars) delete env[name];
  Object.assign(env, resolveEnv(test.set));
  const { stdout } = await run(process.execPath, [self, 'child', urls[test.server]], {
    encoding: 'utf8',
    env,
    timeout: 10000,
  });
  const probe = JSON.parse(stdout);
  results.push({
    name: test.name,
    server: test.server,
    set: test.set,
    unset: trustVars.filter(name => !(name in test.set)),
    result: { trusted: probe.trusted, message: probe.message },
  });
}
for (const server of Object.values(servers)) server.close();

const fixture = {
  reference: pin,
  runtime: process.version,
  sourceHashes: Object.fromEntries(
    ['packages/core/src/agy-transport.ts', 'packages/core/dist/agy-transport.js'].map(file => [
      file,
      createHash('sha256').update(readFileSync(join(root, file))).digest('hex'),
    ]),
  ),
  certificateSha256: createHash('sha256').update(readFileSync(placeholders['@cert@'])).digest('hex'),
  cases: results,
};
const text = JSON.stringify(fixture, null, 2) + '\n';
if (process.argv.includes('--write')) {
  writeFileSync(resolve(process.env.RPI_REFERENCE_OUTPUT ?? fileURLToPath(new URL('../tests/fixtures/ag2-trust.json', import.meta.url))), text);
  console.log(JSON.stringify({ wrote: results.length, runtime: process.version }));
} else {
  const output = resolve(process.env.RPI_REFERENCE_OUTPUT ?? fileURLToPath(new URL('../tests/fixtures/ag2-trust.json', import.meta.url)));
  const committed = readFileSync(output, 'utf8');
  if (committed !== text) {
    console.error('AG-2 trust reference differs from the committed fixture');
    process.exit(1);
  }
  console.log(JSON.stringify({ checked: results.length, runtime: process.version }));
}

function assert_reference() {
  const head = execFileSync('git', ['-C', root, 'rev-parse', 'HEAD'], { encoding: 'utf8' }).trim();
  if (head !== pin) throw new Error(`reference ${head} != ${pin}`);
  if (process.version !== 'v24.22.0') throw new Error(`runtime ${process.version} != v24.22.0`);
}
