// Differential reference for the native command manifests.
//
// Loads the UNCHANGED upstream multiprovider extension under Node 24.22.0 with a
// fake ExtensionAPI and records every `pi.registerCommand` call in order: the
// name and description the host must expose for /multilogin, /multilogout,
// /accounts, /switch-account and /vprovider. It also pins the sha256 of every
// upstream file the import touches.
//
// The command *flows* (dialog controls, notifications, errors) are compared
// through the fixtures the M-CMD-A1 and M-CMD-A2 references produce
// (`commands-reference.json`, `commands-a2-reference.json`); this script covers
// the registration half of the native contract.
//
// Usage (Node 24.22.0, LC_ALL=en_US.UTF-8):
//   node --experimental-transform-types tests/m_native_cmd-reference.ts          # check
//   node --experimental-transform-types tests/m_native_cmd-reference.ts --write  # regenerate
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { pathToFileURL } from 'node:url'

assert.equal(process.version.startsWith('v24.22.'), true, 'run the reference with Node 24.22.0')
assert.equal(new Intl.Collator().resolvedOptions().locale, 'en-US', 'run with LC_ALL=en_US.UTF-8')

const here = import.meta.dirname
const root = resolve(process.env.PI_MULTIPROVIDER_REFERENCE ?? join(here, '../../../../provider-references/multiprovider'))
const fixturePath = join(here, 'm_native_cmd-reference.json')
const write = process.argv.includes('--write')
const url = (path) => pathToFileURL(join(root, path)).href
const modules = (path) => resolve(root, 'node_modules', path)

const PINNED = {
  'extensions/multiprovider.ts': '086a0d9c4fbc8431e81026e5d689747d1c31dfcdf89c9d180c0bbd7e37c0a267',
  'extensions/pool-manager.ts': '67873d40ae349d13d0293c1b92fabaeb9f3b531a7242a0424c721ff3e323cbe8',
  'src/multilogin.ts': 'f6ce501e18b0fb8550fcfbbe77ace3d06cde9bd5b2ad25b29bc8f2e079c590dc',
  'src/auth-store.ts': '29916553aa160b3c676ab843891da2b202fadf536fa220980d564b4ed50d48c6',
  'src/service.ts': '2eafa61662b3ee75a96a7b07a6ccad62ca600bbb2139a407b81cd9983564a026',
  'src/session-pins.ts': '45b1ff1fbfcc6853a717b917aaede404f32fe96a64208c75283a53e954bdc005',
  'src/types.ts': 'aaf5e77ffc5738bfc5ee642a61238793ec0869123ceef12a9650c1384b0d181e',
  'src/errors.ts': '1e4f189fc4aac4488c84912b29bfc7e78629600ead41ce2924c8d8c3a3bac4b7',
  'src/managed.ts': '5a139118f805ed4825259df761dd5e2fbec515da19d3bc10b9719c9bf30bdeef',
  'src/virtual.ts': '6c38efe1d8bcd180e3287b182df592914a1af0a7091f872cd6dc32042575f808',
  'src/announcement.ts': 'fc0bf72467dba7b524146ade4a66c420d546a01e7867b8095c8b7d8b1804ae18',
  'src/index.ts': '90b3b79432e4d556f75b7b8e25fc3fb512cb56293f5263e6b2090465f5cfaa66',
  'src/lift.ts': '887b91aab533565504449b6094c1a2b076832b0fdc73bfc352e2c46a53ac3695',
  'src/register.ts': '946932988b5884081d0f173b464e1b2d42740849803463bdb8fd9841fcc87449',
}
for (const [file, expected] of Object.entries(PINNED)) {
  const actual = createHash('sha256').update(await readFile(join(root, file))).digest('hex')
  assert.equal(actual, expected, `${file} differs from the pinned upstream`)
}
const piVersion = JSON.parse(await readFile(modules('@earendil-works/pi-coding-agent/package.json'), 'utf8')).version
assert.equal(piVersion, '1.3.0')

const agentDir = await mkdtemp(join(tmpdir(), 'rpi-multiprovider-native-cmd-'))
process.env.PI_CODING_AGENT_DIR = agentDir
try {
  const { initTheme } = await import(modules('@earendil-works/pi-coding-agent/dist/index.js'))
  initTheme('dark')
  const multiprovider = (await import(url('extensions/multiprovider.ts'))).default

  const bus = new Map()
  const commands = []
  const registrations = new Map()
  const pi = {
    events: {
      emit(name, value) { for (const fn of bus.get(name) ?? []) fn(value) },
      on(name, fn) {
        const listeners = bus.get(name) ?? new Set()
        listeners.add(fn)
        bus.set(name, listeners)
        return () => listeners.delete(fn)
      },
    },
    on() {},
    registerProvider() {},
    unregisterProvider() {},
    registerCommand(name, options) {
      assert.equal(typeof options?.handler, 'function', `${name} must register a handler`)
      assert.equal(registrations.has(name), false, `${name} must be registered once`)
      registrations.set(name, options)
      commands.push({ name, description: options.description })
    },
    appendEntry() {},
    getAllTools: () => [],
  }
  await multiprovider(pi)
  assert.deepEqual(commands.map(command => command.name), [
    'multilogin',
    'multilogout',
    'accounts',
    'switch-account',
    'vprovider',
  ])
  for (const command of commands) assert.equal(typeof command.description, 'string')

  const output = {
    reference: {
      upstream: 'bd97d65a36f541935dc91e33ba15d7910a30f9d7',
      pi: '1.3.0',
      node: process.version,
    },
    pinned: PINNED,
    commands,
  }
  const text = `${JSON.stringify(output, null, 2)}\n`
  if (write) {
    await writeFile(fixturePath, text)
  } else {
    assert.equal(await readFile(fixturePath, 'utf8'), text, 'm_native_cmd-reference.json is stale; rerun with --write')
  }
  console.log(`m-native-cmd reference ok: ${commands.length} commands`)
} finally {
  await rm(agentDir, { recursive: true, force: true })
}
