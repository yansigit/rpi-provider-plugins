// Differential reference for the /multilogin, /multilogout, /accounts and
// /switch-account command flows. It runs the UNCHANGED upstream extension
// (extensions/multiprovider.ts), its login helpers (src/multilogin.ts) and its
// auth store against a fake Pi (pi.registerCommand, pi.on, pi.appendEntry) and
// a fake ctx whose ui is recorded and answered from a script. Every awaited
// Pi UI call is projected to a native dialog control (docs/presentation-map.md),
// the answers are native dialog replies, and the trace, notifications, appended
// session entries and final multiprovider-auth.json are written to
// tests/commands-reference.json for the Rust port to replay.
//
// Usage (Node 24.22.0, LC_ALL=en_US.UTF-8):
//   node --experimental-transform-types tests/commands-reference.ts          # check
//   node --experimental-transform-types tests/commands-reference.ts --write  # regenerate
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import crypto from 'node:crypto'
import { existsSync } from 'node:fs'
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { registerHooks, syncBuiltinESMExports } from 'node:module'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { pathToFileURL } from 'node:url'

process.env.TZ = 'UTC'
assert.equal(process.version.startsWith('v24.22.'), true, 'run the reference with Node 24.22.0')
assert.equal(new Intl.Collator().resolvedOptions().locale, 'en-US', 'run with LC_ALL=en_US.UTF-8')

const here = import.meta.dirname
const root = resolve(process.env.PI_MULTIPROVIDER_REFERENCE ?? join(here, '../../../../provider-references/multiprovider'))
const fixturePath = join(here, 'commands-reference.json')
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
  'node_modules/@earendil-works/pi-coding-agent/dist/modes/interactive/components/oauth-selector.js': '9aa24ad74415288998e98b0a53b1e9fdf3c2eea2d060af4517486045db83d99d',
  'node_modules/@earendil-works/pi-coding-agent/dist/modes/interactive/components/login-dialog.js': '2b00ad41244cb1dc63a9fcbfca855a00e2acbc68ec2bc4249024cefdc1a7a157',
  'node_modules/@earendil-works/pi-coding-agent/dist/modes/interactive/components/extension-selector.js': '22d9dd842d59300a1c4d9f4fa07bdbb2b4089c6670037ac769919cc1de4fbc58',
}
for (const [file, expected] of Object.entries(PINNED)) {
  const actual = createHash('sha256').update(await readFile(join(root, file))).digest('hex')
  assert.equal(actual, expected, `${file} differs from the pinned upstream`)
}
const piVersion = JSON.parse(await readFile(modules('@earendil-works/pi-coding-agent/package.json'), 'utf8')).version
assert.equal(piVersion, '1.3.0')

// Clock and identifiers are injected: upstream reads Date and randomUUID only.
let now = 1700000000000
let sequence = 0
const DateOriginal = Date
class Clock extends DateOriginal {
  constructor(...args) { super(...(args.length ? args : [now])) }
  static now() { return now }
}
globalThis.Date = Clock
// Account ids are fixture-N. The temporary file name that the store also draws
// from randomUUID() is recognized by its caller and stays random, so it does
// not shift the account numbering.
const randomUUIDOriginal = crypto.randomUUID
crypto.randomUUID = () => (new Error().stack ?? '').includes('writeStateUnlocked')
  ? randomUUIDOriginal()
  : `fixture-${++sequence}`
syncBuiltinESMExports()

// The pool manager is the pool-manager lane's surface. Its module is replaced at
// resolution time by a scripted stub; /multilogin's own call into it is the
// boundary that is compared.
const POOL_STUB = 'rpi-reference:pool-manager'
const POOL_STUB_SOURCE = `export async function openPoolManager(ctx, provider, callbacks, methods) {
  return globalThis.__rpiPoolManagerStub(provider, methods, callbacks)
}
`
registerHooks({
  resolve(specifier, context, nextResolve) {
    if (specifier === './pool-manager.ts' && context.parentURL?.endsWith('/extensions/multiprovider.ts')) {
      return { url: POOL_STUB, shortCircuit: true }
    }
    return nextResolve(specifier, context)
  },
  load(loadUrl, context, nextLoad) {
    if (loadUrl === POOL_STUB) return { format: 'module', source: POOL_STUB_SOURCE, shortCircuit: true }
    return nextLoad(loadUrl, context)
  },
})

const baseDir = await mkdtemp(join(tmpdir(), 'rpi-multiprovider-commands-base-'))
process.env.PI_CODING_AGENT_DIR = baseDir
const { initTheme, } = await import(modules('@earendil-works/pi-coding-agent/dist/index.js'))
initTheme('dark')
const { createProvider, createAssistantMessageEventStream, normalizeContext } = await import(modules('@earendil-works/pi-ai/dist/index.js'))
const multiprovider = (await import(url('extensions/multiprovider.ts'))).default
const { LoginDialogHostComponent, loginCredential, selectLogin, showLoginDialog } = await import(url('src/multilogin.ts'))
const { OAuthSelectorComponent } = await import(modules('@earendil-works/pi-coding-agent/dist/index.js'))
const { MultiAuthStore } = await import(url('src/auth-store.ts'))
const { MultiProviderService } = await import(url('src/service.ts'))
const { SESSION_PIN_ENTRY_TYPE } = await import(url('src/session-pins.ts'))
assert.equal(SESSION_PIN_ENTRY_TYPE, 'pi-multiprovider:switch-account')

let snapshotLog = null
const snapshotOriginal = MultiProviderService.prototype.snapshot
MultiProviderService.prototype.snapshot = async function snapshot() {
  const value = await snapshotOriginal.call(this)
  if (snapshotLog !== null) snapshotLog.push(JSON.parse(JSON.stringify(value)))
  return value
}

const clone = (value) => JSON.parse(JSON.stringify(value))
const stripAnsi = (text) => text.replace(/\x1b\[[0-9;]*m/g, '')
const DIALOG_SEPARATOR = ' · '

// Scripted provider logins. Steps are interpreted identically by the Rust fake
// port: notify steps emit an AuthEvent, prompt steps await the answer, and
// `{name}` placeholders in the credential template are filled from prompt answers.
const BROWSER = {
  steps: [
    { notify: { type: 'device_code', userCode: 'ABCD-1234', verificationUri: 'https://login.invalid/device' } },
    { notify: { type: 'progress', message: 'Waiting for authentication...' } },
    { prompt: { type: 'select', message: 'Select login method:', options: [{ id: 'browser', label: 'Browser OAuth' }, { id: 'manual', label: 'Manual code' }] }, as: 'method' },
    { prompt: { type: 'manual_code', message: 'Paste the authorization code' }, as: 'code' },
  ],
  credential: { type: 'oauth', refresh: '{method}-refresh', access: '{code}', expires: 1700000060000 },
}
const SECRET = {
  steps: [{ prompt: { type: 'secret', message: 'Enter API key' }, as: 'key' }],
  credential: { type: 'api_key', key: '{key}' },
}
const TOKEN = {
  steps: [{ prompt: { type: 'text', message: 'Token' }, as: 'token' }],
  credential: { type: 'token', value: '{token}' },
}
const FAIL = {
  steps: [{ notify: { type: 'info', message: 'Gamma requires the console', links: [{ label: 'Console', url: 'https://login.invalid/console' }, { url: 'https://login.invalid/help' }] } }],
  fail: 'Gamma sign-in rejected',
}

const AUTH_URL = {
  steps: [
    { notify: { type: 'auth_url', url: 'https://login.invalid/authorize', instructions: 'Approve access in the browser' } },
    { prompt: { type: 'text', message: 'Paste the redirect URL', placeholder: 'https://login.invalid/callback' }, as: 'redirect' },
  ],
  credential: { type: 'oauth', refresh: 'eta-refresh', access: '{redirect}', expires: 1700000060000 },
}
const WORLD = {
  alpha: { id: 'alpha', name: 'Alpha Co', oauth: { name: 'Alpha OAuth', loginLabel: 'Alpha sign-in', login: BROWSER }, apiKey: { name: 'API key', login: SECRET }, models: [{ id: 'alpha-1', name: 'Alpha 1' }] },
  beta: { id: 'beta', name: 'Beta Labs', apiKey: { name: 'Beta key' }, models: [{ id: 'beta-1', name: 'Beta 1' }] },
  delta: { id: 'delta', name: 'Delta None', models: [{ id: 'delta-1', name: 'Delta 1' }] },
  epsilon: { id: 'epsilon', name: 'Epsilon Plain', oauth: { name: 'Epsilon OAuth' }, models: [{ id: 'eps-1', name: 'Eps 1' }] },
  gamma: { id: 'gamma', name: 'Gamma AI', oauth: { name: 'Gamma OAuth', login: FAIL }, models: [{ id: 'gamma-1', name: 'Gamma 1' }] },
  kappa: { id: 'kappa', name: 'Kappa Two', apiKey: { name: 'Kappa key', login: TOKEN }, models: [{ id: 'kappa-1', name: 'Kappa 1' }] },
  eta: { id: 'eta', name: 'Eta Redirect', oauth: { name: 'Eta OAuth', login: AUTH_URL }, models: [{ id: 'eta-1', name: 'Eta 1' }] },
}
const ALL = Object.keys(WORLD)
const DEFAULT_RUNTIME = {
  providers: ALL,
  auth: { alpha: { configured: true, source: 'stored credential' }, gamma: { configured: true, source: 'GAMMA_TOKEN' } },
  oauth: { alpha: true, gamma: true },
}
const TEMPLATE = { api: 'test-api', baseUrl: 'https://example.invalid', reasoning: true, thinkingLevelMap: { high: 'high-effort', off: null }, input: ['text'], cost: { input: 1, output: 2, cacheRead: 0.5, cacheWrite: 0 }, contextWindow: 1000, maxTokens: 100 }
const WORK = { label: 'Work', credential: { type: 'oauth', refresh: 'refresh-work', access: 'access-work', expires: 1700000600000 } }
const TEAM = { label: 'Team', credential: { type: 'api_key', key: 'sk-team' } }
const ALPHA_POOL = [{ op: 'add', provider: 'alpha', input: WORK }, { op: 'add', provider: 'alpha', input: TEAM }]
// A failed request through the installed provider: the Work account's resolved
// credential (`access-work`) errors, so the scheduler cools it down and serves
// the same request from Team.
const COOLDOWN = { provider: 'alpha', model: 'alpha-1', failKey: 'access-work', error: 'HTTP 429 Too Many Requests' }
const COOLDOWN_SETUP = [...ALPHA_POOL, { op: 'setPool', provider: 'alpha', input: { affinity: false } }, { op: 'scheduler', input: { errorsBeforeSwitch: 1 } }]
const VIRTUAL = { id: 'vp', label: 'Virtual Pool', models: [{ id: 'model', label: 'Model', backends: [{ providerId: 'alpha', modelId: 'alpha-1', template: TEMPLATE }] }] }
const SESSION_PIN = (data) => ({ type: 'custom', customType: SESSION_PIN_ENTRY_TYPE, data })

// Fake Pi UI projection ------------------------------------------------------

function modelOf(providerId, model) {
  return { id: model.id, name: model.name, api: 'test-api', provider: providerId, baseUrl: 'https://login.invalid', reasoning: false, input: ['text'], cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 }, contextWindow: 1000, maxTokens: 100 }
}

// Pi's showPrompt receives only the message and placeholder, so the prompt type
// is recorded here before the login awaits it; secret prompts are masked.
let promptType = 'text'
function scriptedLogin(script) {
  return async (interaction) => {
    const vars = {}
    for (const step of script.steps) {
      if ('notify' in step) interaction.notify(step.notify)
      else {
        promptType = step.prompt.type
        vars[step.as] = await interaction.prompt(step.prompt)
      }
    }
    if (script.fail !== undefined) throw new Error(script.fail)
    return fill(script.credential, vars)
  }
}

// Some scenarios make one real request through the installed provider first, so
// the scheduler records a failure and a cooldown. The base stream succeeds with
// a `done` event, except for the account whose resolved credential (`apiKey`)
// equals `failKey`; that account returns an error event.
let streamSpec = null
function providerStream(providerId) {
  return (model, _context, options) => {
    const fail = streamSpec !== null
      && streamSpec.provider === providerId
      && streamSpec.failKey === (options?.apiKey ?? 'upstream')
    const result = {
      role: 'assistant', api: model.api, provider: model.provider, model: model.id,
      content: [], timestamp: Date.now(), stopReason: fail ? 'error' : 'stop',
      ...(fail ? { errorMessage: streamSpec.error } : {}),
      usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } },
    }
    const events = createAssistantMessageEventStream()
    events.push({ type: 'start', partial: result })
    if (fail) events.push({ type: 'error', reason: 'error', error: result })
    else events.push({ type: 'done', reason: 'stop', message: result })
    events.end(result)
    return events
  }
}

function fill(value, vars) {
  if (typeof value === 'string') {
    return value.replace(/\{(\w+)\}/g, (_match, name) => {
      assert.ok(name in vars, `unknown placeholder ${name}`)
      return vars[name]
    })
  }
  if (Array.isArray(value)) return value.map((item) => fill(item, vars))
  if (value !== null && typeof value === 'object') return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, fill(item, vars)]))
  return value
}

function makeProvider(id) {
  const def = WORLD[id]
  const auth = {}
  if (def.oauth) {
    auth.oauth = {
      name: def.oauth.name,
      ...(def.oauth.loginLabel === undefined ? {} : { loginLabel: def.oauth.loginLabel }),
      ...(def.oauth.login === undefined ? {} : { login: scriptedLogin(def.oauth.login) }),
      async refresh(credential) { return credential },
      async toAuth(credential) { return { apiKey: credential.access } },
    }
  }
  if (def.apiKey) {
    auth.apiKey = {
      name: def.apiKey.name,
      ...(def.apiKey.login === undefined ? {} : { login: scriptedLogin(def.apiKey.login) }),
      async resolve({ credential }) {
        return credential?.key === undefined
          ? undefined
          : { auth: { apiKey: credential.key }, source: 'test' }
      },
    }
  }
  return createProvider({
    id,
    name: def.name,
    auth,
    models: def.models.map((model) => modelOf(id, model)),
    api: { stream: providerStream(id), streamSimple: providerStream(id) },
  })
}

// Projection of Pi's OAuthSelectorComponent rows (plain text, theme codes removed).
function formatType(authType) { return authType === 'api_key' ? 'API key' : 'subscription' }
function statusText(option) {
  const status = option.status
  if (status === undefined) return ' • not configured'
  if (status.type !== option.authType) return ` • ${formatType(status.type)} configured`
  if (!status.source || status.source === 'OAuth' || status.source === 'stored credential') return ' ✓ configured'
  const source = /^[A-Z][A-Z0-9_]*(?:, [A-Z][A-Z0-9_]*)*$/.test(status.source) ? `env: ${status.source}` : status.source
  return ` ✓ ${source}`
}
function oauthLabel(option, showType) {
  return option.name + (showType ? ` [${formatType(option.authType)}]` : '') + statusText(option)
}

// Projections of Pi's dialog text carry the dialog title as the prefix.
function dialogTitle(host) {
  const title = host.dialog.children[1]
  assert.equal(typeof title.text, 'string')
  return stripAnsi(title.text)
}

function selectIndex(reply, count) {
  if (reply === null) return undefined
  assert.equal(reply.type, 'select')
  assert.equal(reply.values.length, 1)
  assert.match(reply.values[0], /^(0|[1-9][0-9]*)$/)
  const index = Number(reply.values[0])
  assert.ok(index < count, `select value ${index} outside ${count} choices`)
  return index
}

function inputValue(reply) {
  if (reply === null) return undefined
  assert.equal(reply.type, 'input')
  assert.equal(typeof reply.value, 'string')
  return reply.value
}

// Per-scenario state.
let trace = []
let answers = []
let poolActions = []
let authPath = ''
let controlCount = 0
const norm = (text) => (authPath === '' ? text : text.replaceAll(authPath, '$AUTH_PATH'))
const note = (text, kind) => { trace.push({ notify: { text: norm(text), kind } }) }
function show(control) {
  assert.ok(answers.length > 0, `unscripted control ${JSON.stringify(control)}`)
  const reply = answers.shift() ?? null
  controlCount += 1
  trace.push({ control, reply })
  return reply
}

function instrumentLoginHost(host) {
  const title = dialogTitle(host)
  const dialog = host.dialog
  const showSelectOriginal = host.showSelect
  host.showSelect = (message, options, signal) => {
    const pending = showSelectOriginal.call(host, message, options, signal)
    assert.notEqual(host.children[0], dialog, 'selector replaces the dialog view while selecting')
    const index = selectIndex(show({ kind: 'select', title: `${title}${DIALOG_SEPARATOR}${message}`, choices: options.map((option, i) => ({ label: option.label, value: String(i) })) }), options.length)
    const selector = host.activeView
    if (index === undefined) selector.onCancelCallback()
    else selector.onSelectCallback(options[index].label)
    return pending
  }
  const prompt = (original) => (message, placeholder) => {
    const pending = original.call(dialog, message, placeholder)
    const value = inputValue(show({ kind: 'input', title: `${title}${DIALOG_SEPARATOR}${message}`, placeholder: placeholder ?? '', masked: promptType === 'secret' }))
    if (value === undefined) dialog.cancel()
    else { dialog.input.setValue(value); dialog.input.onSubmit() }
    return pending
  }
  const manual = dialog.showManualInput
  dialog.showPrompt = prompt(dialog.showPrompt)
  dialog.showManualInput = (message) => {
    const pending = manual.call(dialog, message)
    const value = inputValue(show({ kind: 'input', title: `${title}${DIALOG_SEPARATOR}${message}`, placeholder: '', masked: false }))
    if (value === undefined) dialog.cancel()
    else { dialog.input.setValue(value); dialog.input.onSubmit() }
    return pending
  }
  // showAuth opens a browser through the host; it is projected, not performed.
  dialog.showAuth = (authUrl, instructions) => {
    note([title, `Open ${authUrl}`, instructions].filter(Boolean).join(DIALOG_SEPARATOR), 'info')
  }
  const deviceCode = dialog.showDeviceCode
  dialog.showDeviceCode = (info) => {
    deviceCode.call(dialog, info)
    note(`${title}${DIALOG_SEPARATOR}Open ${info.verificationUri} and enter code ${info.userCode}`, 'info')
  }
  const waiting = dialog.showWaiting
  dialog.showWaiting = (message) => {
    waiting.call(dialog, message)
    note(`${title}${DIALOG_SEPARATOR}${message}`, 'info')
  }
  const info = dialog.showInfo
  dialog.showInfo = (message, links = [], showCloseHint = false) => {
    info.call(dialog, message, links, showCloseHint)
    const linkTexts = links.map((link) => (link.label ? `${link.label}: ${link.url}` : link.url))
    note([title, message, ...linkTexts].join(DIALOG_SEPARATOR), 'info')
  }
  const progress = dialog.showProgress
  dialog.showProgress = (message) => {
    progress.call(dialog, message)
    note(`${title}${DIALOG_SEPARATOR}${message}`, 'info')
  }
  return host
}

// OAuthSelectorComponent is built inside the factory and only its wrapper is
// returned, so the selector is captured as it is constructed (filterProviders
// runs once from its constructor). Upstream code is not modified.
let constructedSelector = null
const filterProvidersOriginal = OAuthSelectorComponent.prototype.filterProviders
OAuthSelectorComponent.prototype.filterProviders = function filterProviders(query) {
  constructedSelector = this
  return filterProvidersOriginal.call(this, query)
}

function customControl(factory) {
  return new Promise((resolve) => {
    constructedSelector = null
    const wrapper = factory({ requestRender() {} }, undefined, {}, resolve)
    const component = constructedSelector ?? wrapper
    if (constructedSelector !== null) {
      const options = component.allProviders
      const query = component.searchInput.getValue()
      const showType = new Set(options.map((option) => option.authType)).size > 1
      const control = {
        kind: 'select',
        title: 'Select provider to configure:',
        searchable: true,
        choices: options.map((option, i) => ({ label: oauthLabel(option, showType), value: String(i) })),
        ...(query === '' ? {} : { query }),
      }
      const index = selectIndex(show(control), options.length)
      if (index === undefined) component.onCancelCallback()
      else component.onSelectCallback(options[index].id, options[index].authType)
    } else if (component.dialog !== undefined && typeof component.showSelect === 'function') {
      instrumentLoginHost(component)
    } else {
      throw new Error('unexpected custom component')
    }
  })
}

function makeUi(hasUI, counters) {
  if (!hasUI) {
    return { select: async () => undefined, confirm: async () => false, input: async () => undefined, notify() {}, custom: async () => undefined }
  }
  return {
    select(title, labels) {
      counters.select += 1
      const index = selectIndex(show({ kind: 'select', title, choices: labels.map((label, i) => ({ label, value: String(i) })) }), labels.length)
      return Promise.resolve(index === undefined ? undefined : labels[index])
    },
    confirm(title, message) {
      const reply = show({ kind: 'confirm', title, description: message })
      if (reply === null) return Promise.resolve(false)
      assert.equal(reply.type, 'confirm')
      return Promise.resolve(reply.confirmed === true)
    },
    input(title, placeholder) {
      const value = inputValue(show({ kind: 'input', title, placeholder: placeholder ?? '', masked: false }))
      return Promise.resolve(value)
    },
    notify(text, kind) { note(text, kind ?? 'info') },
    custom(factory) { return customControl(factory) },
  }
}

// Scenario driver -----------------------------------------------------------

function registryFor(scenario) {
  const providers = new Map()
  for (const id of scenario.providers ?? ALL) providers.set(id, makeProvider(id))
  const runtimeSpec = scenario.runtime === undefined ? DEFAULT_RUNTIME : scenario.runtime
  const runtime = runtimeSpec === null ? undefined : {
    getProviders: () => runtimeSpec.providers.map((id) => providers.get(id)).filter((provider) => provider !== undefined),
    getProviderAuthStatus: (id) => runtimeSpec.auth?.[id] ?? { configured: false },
    isUsingOAuth: (id) => runtimeSpec.oauth?.[id] === true,
  }
  const models = [...providers.keys()].flatMap((id) => WORLD[id].models.map((model) => modelOf(id, model)))
  const registry = {
    providers,
    runtime,
    getAll: () => models,
    getProvider: (id) => providers.get(id),
    getProviderAuthStatus: (id) => runtimeSpec?.auth?.[id] ?? { configured: false },
  }
  return registry
}

function resolveAccountRef(ref, setupIds) {
  if (ref === 'last') return setupIds.at(-1)
  return ref
}

async function runSetup(scenario, store) {
  const setupIds = []
  for (const op of scenario.setup ?? []) {
    if (op.op === 'add') setupIds.push((await store.addAccount(op.provider, clone(op.input))).id)
    else if (op.op === 'setPool') await store.updatePool(op.provider, clone(op.input))
    else if (op.op === 'account') await store.updateAccount(op.provider, resolveAccountRef(op.account, setupIds), clone(op.input))
    else if (op.op === 'saveVirtual') await store.saveVirtualProvider(clone(op.input))
    else if (op.op === 'scheduler') await store.updateSchedulerSettings(clone(op.input))
    else throw new Error(`unknown setup operation ${op.op}`)
  }
  return setupIds
}

async function runCommand(scenario) {
  const dir = await mkdtemp(join(tmpdir(), 'rpi-multiprovider-commands-'))
  authPath = join(dir, 'multiprovider-auth.json')
  process.env.PI_CODING_AGENT_DIR = dir
  try {
    now = 1700000000000
    sequence = 0
    const setupStore = new MultiAuthStore(authPath)
    await runSetup(scenario, setupStore)
    const initialAuth = existsSync(authPath) ? await readFile(authPath, 'utf8') : null
    const idSequence = sequence

    const registry = registryFor(scenario)
    const bus = new Map()
    const handlers = new Map()
    const commands = new Map()
    const appended = []
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
      on(name, handler) { handlers.set(name, handler) },
      registerProvider(provider) { registry.providers.set(provider.id, provider) },
      unregisterProvider(id) { registry.providers.delete(id) },
      registerCommand(name, options) { commands.set(name, options) },
      appendEntry(customType, data) {
        if (scenario.journalError !== undefined) throw new Error(scenario.journalError)
        appended.push({ customType, data: clone(data) })
      },
      getAllTools: () => [],
    }
    const entries = clone(scenario.entries ?? [])
    const ctx = {
      cwd: dir,
      mode: scenario.mode ?? 'tui',
      hasUI: scenario.hasUI !== false,
      model: scenario.model ?? undefined,
      isIdle: () => true,
      sessionManager: { getSessionId: () => 'session-1', getEntries: () => entries },
      modelRegistry: registry,
    }
    const counters = { select: 0 }
    ctx.ui = makeUi(ctx.hasUI, counters)

    globalThis.__rpiPoolManagerStub = async (provider, methods) => {
      trace.push({ pool: { provider: provider.id, methods: methods.map(({ label, value }) => ({ label, value })) } })
      assert.ok(poolActions.length > 0, 'unscripted pool manager call')
      return clone(poolActions.shift())
    }

    trace = []
    answers = []
    poolActions = []
    await multiprovider(pi)
    await handlers.get('session_start')({}, ctx)
    assert.deepEqual(trace, [], 'session_start shows nothing')

    streamSpec = scenario.stream === undefined ? null : clone(scenario.stream)
    if (streamSpec !== null) {
      const provider = registry.providers.get(streamSpec.provider)
      assert.ok(provider !== undefined, `unknown stream provider ${streamSpec.provider}`)
      const model = registry.getAll().find(candidate => candidate.provider === streamSpec.provider && candidate.id === streamSpec.model)
      assert.ok(model !== undefined, `unknown stream model ${streamSpec.model}`)
      for await (const _event of provider.streamSimple(model, normalizeContext({ messages: [] }))) {
        // Drive the real scheduler so the failed account records a cooldown.
      }
      assert.deepEqual(trace, [], 'a scripted request shows nothing')
    }
    streamSpec = null

    trace = []
    answers = clone(scenario.answers ?? [])
    poolActions = clone(scenario.poolActions ?? [])
    snapshotLog = []
    let error = null
    try {
      await commands.get(scenario.command).handler(scenario.args ?? '', ctx)
    } catch (caught) {
      if (caught instanceof assert.AssertionError) throw caught
      error = norm(caught.message)
    }
    assert.deepEqual(answers, [], 'every scripted answer is consumed')
    assert.deepEqual(poolActions, [], `every scripted pool action is consumed (error: ${error})`)
    const serviceSnapshot = snapshotLog.length === 0 ? null : snapshotLog[0]
    snapshotLog = null
    const authFile = existsSync(authPath) ? await readFile(authPath, 'utf8') : null
    const notifications = trace.filter((item) => 'notify' in item).map((item) => item.notify)
    const result = {
      input: {
        command: scenario.command,
        args: scenario.args ?? '',
        hasUI: ctx.hasUI,
        mode: ctx.mode,
        model: scenario.model ?? null,
        sessionId: 'session-1',
        entries: scenario.entries ?? [],
        initialAuth,
        idSequence,
        serviceSnapshot,
        answers: scenario.answers ?? [],
        poolActions: scenario.poolActions ?? [],
        stream: scenario.stream ?? null,
        runtime: scenario.runtime === undefined ? DEFAULT_RUNTIME : scenario.runtime,
        providers: scenario.providers ?? ALL,
        ...(scenario.journalError === undefined ? {} : { journalError: scenario.journalError }),
      },
      expected: { trace, notifications, entries: appended, authFile, error },
    }
    await handlers.get('session_shutdown')?.({}, ctx)
    return result
  } finally {
    globalThis[Symbol.for('pi-multiprovider.live-providers')]?.clear()
    await rm(dir, { recursive: true, force: true })
  }
}

async function runLoginFlow(scenario) {
  const registry = registryFor({ providers: ALL, runtime: null })
  const flows = []
  for (const flow of scenario.flows) {
    const provider = registry.providers.get(flow.provider)
    const prompts = []
    const events = []
    const remaining = clone(flow.answers)
    const interaction = {
      signal: new AbortController().signal,
      notify(event) { events.push(clone(event)) },
      async prompt(prompt) {
        prompts.push(clone(prompt))
        return remaining.shift()
      },
    }
    let result
    try {
      result = { credential: await loginCredential({ provider, authType: flow.authType }, interaction) }
    } catch (error) {
      result = { error: error.message }
    }
    assert.deepEqual(remaining, [], 'every scripted answer is consumed')
    flows.push({ prompts, events, result })
  }
  return { input: { flows: scenario.flows }, expected: { flows } }
}

async function runHostSelect(scenario) {
  trace = []
  answers = clone(scenario.answers)
  const host = new LoginDialogHostComponent({ requestRender() {} }, 'alpha', () => {}, 'Alpha Co', scenario.title)
  instrumentLoginHost(host)
  const dialog = host.dialog
  const pending = host.showSelect(scenario.message, scenario.options)
  const outcome = await pending.then((value) => ({ value }), (error) => ({ error: error.message }))
  assert.equal(host.children[0], dialog, 'dialog view is restored after the selector closes')
  assert.deepEqual(answers, [])
  return { input: { title: scenario.title, message: scenario.message, options: scenario.options, answers: scenario.answers }, expected: { trace, outcome } }
}

async function runLoginDialog(scenario) {
  trace = []
  answers = clone(scenario.answers)
  const registry = registryFor({ providers: ALL, runtime: null })
  const counters = { select: 0 }
  const ctx = { hasUI: true, mode: 'tui', ui: makeUi(true, counters), modelRegistry: registry }
  const provider = registry.providers.get(scenario.provider)
  const result = await showLoginDialog(ctx, { provider, authType: scenario.authType }, scenario.title === undefined ? {} : { title: scenario.title })
  assert.equal(counters.select, 0, 'the login dialog never calls ctx.ui.select')
  assert.deepEqual(answers, [])
  const outcome = result === undefined ? null : ('error' in result ? { error: result.error.message } : { credential: result.credential })
  return { input: { provider: scenario.provider, authType: scenario.authType, title: scenario.title ?? null, answers: scenario.answers }, expected: { trace, outcome } }
}

// Scenarios -----------------------------------------------------------------

const scenarios = []
const command = (name, spec) => scenarios.push({ name, kind: 'command', ...spec })
const SELECT = (index) => ({ type: 'select', values: [String(index)] })
const CONFIRM = (confirmed) => ({ type: 'confirm', confirmed })
const INPUT = (value) => ({ type: 'input', value })
const ALL_POOLS = ALPHA_POOL

// /multilogin
command('multilogin no UI rejects without output', { command: 'multilogin', args: '', hasUI: false })
command('multilogin non-TUI mode notifies', { command: 'multilogin', args: '', mode: 'rpc' })
command('multilogin unknown provider reference', { command: 'multilogin', args: 'nope' })
command('multilogin no providers with login methods', { command: 'multilogin', args: '', runtime: { providers: ['delta'], auth: {}, oauth: {} } })
command('multilogin single option by name skips the selector', { command: 'multilogin', args: 'beta labs', poolActions: [{ type: 'closed' }] })
command('multilogin provider selector cancel', { command: 'multilogin', args: '', answers: [null] })
command('multilogin selector without runtime shows not-configured rows', { command: 'multilogin', args: '', runtime: null, answers: [null] })
command('multilogin search query selects an api key method', { command: 'multilogin', args: 'alpha', answers: [SELECT(1)], poolActions: [{ type: 'closed' }] })
command('multilogin auth url login asks for the redirect', { command: 'multilogin', args: 'eta', answers: [INPUT(''), INPUT('https://login.invalid/callback?code=7')], poolActions: [{ type: 'add', method: 'oauth' }, { type: 'closed' }] })
command('multilogin add cancelled at the label prompt', { command: 'multilogin', args: 'alpha', setup: ALL_POOLS, answers: [SELECT(0), null], poolActions: [{ type: 'add', method: 'oauth' }, { type: 'closed' }] })
command('multilogin oauth add uses the default label', { command: 'multilogin', args: 'alpha', answers: [SELECT(0), INPUT(''), SELECT(0), INPUT('code-123')], poolActions: [{ type: 'add', method: 'oauth' }, { type: 'closed' }] })
command('multilogin api key add uses a custom trimmed label', { command: 'multilogin', args: 'alpha', setup: ALL_POOLS, answers: [SELECT(1), INPUT('  Team key  '), INPUT('sk-live-1')], poolActions: [{ type: 'add', method: 'api_key' }, { type: 'closed' }] })
command('multilogin api key login cancelled at the secret prompt', { command: 'multilogin', args: 'alpha', answers: [SELECT(1), INPUT(''), null], poolActions: [{ type: 'add', method: 'api_key' }, { type: 'closed' }] })
command('multilogin login method selector cancelled', { command: 'multilogin', args: 'alpha', answers: [SELECT(0), INPUT(''), null], poolActions: [{ type: 'add', method: 'oauth' }, { type: 'closed' }] })
command('multilogin manual code cancelled', { command: 'multilogin', args: 'alpha', answers: [SELECT(0), INPUT(''), SELECT(1), null], poolActions: [{ type: 'add', method: 'oauth' }, { type: 'closed' }] })
command('multilogin provider login failure is reported', { command: 'multilogin', args: 'gamma', answers: [INPUT('')], poolActions: [{ type: 'add', method: 'oauth' }, { type: 'closed' }] })
command('multilogin paste key rejects an empty key', { command: 'multilogin', args: 'beta', answers: [INPUT(''), INPUT('   ')], poolActions: [{ type: 'add', method: 'api_key_paste' }, { type: 'closed' }] })
command('multilogin paste key cancelled at the key prompt', { command: 'multilogin', args: 'beta', answers: [INPUT('x'), null], poolActions: [{ type: 'add', method: 'api_key_paste' }, { type: 'closed' }] })
command('multilogin paste key creates a pool', { command: 'multilogin', args: 'beta', answers: [INPUT('Beta Team'), INPUT('  key-xyz  ')], poolActions: [{ type: 'add', method: 'api_key_paste' }, { type: 'closed' }] })
command('multilogin method without login reports the upstream error', { command: 'multilogin', args: 'epsilon', answers: [INPUT('')], poolActions: [{ type: 'add', method: 'oauth' }, { type: 'closed' }] })
command('multilogin stored credential rejected by the store', { command: 'multilogin', args: 'kappa', answers: [INPUT(''), INPUT('tok-1')], poolActions: [{ type: 'add', method: 'api_key' }, { type: 'closed' }] })
command('multilogin reauthenticates an oauth account', { command: 'multilogin', args: 'alpha', setup: ALL_POOLS, answers: [SELECT(0), SELECT(0), INPUT('code-2')], poolActions: [{ type: 'reauth', accountId: 'fixture-1', method: 'oauth' }, { type: 'closed' }] })
command('multilogin reauthentication of a missing account', { command: 'multilogin', args: 'alpha', setup: ALL_POOLS, answers: [SELECT(0)], poolActions: [{ type: 'reauth', accountId: 'gone', method: 'oauth' }, { type: 'closed' }] })
command('multilogin reauthentication failure is reported', { command: 'multilogin', args: 'gamma', setup: [{ op: 'add', provider: 'gamma', input: { label: 'Gamma Main', credential: { type: 'oauth', refresh: 'g-r', access: 'g-a', expires: 1700000600000 } } }], answers: [SELECT(0)], poolActions: [{ type: 'reauth', accountId: 'fixture-1', method: 'oauth' }, { type: 'closed' }] })
command('multilogin reauthentication cancelled at the method selector', { command: 'multilogin', args: 'alpha', setup: ALL_POOLS, answers: [SELECT(0), null], poolActions: [{ type: 'reauth', accountId: 'fixture-1', method: 'oauth' }, { type: 'closed' }] })
command('multilogin paste reauthentication cancelled at the key prompt', { command: 'multilogin', args: 'beta', setup: [{ op: 'add', provider: 'beta', input: { label: 'Beta Main', credential: { type: 'api_key', key: 'sk-beta' } } }], answers: [null], poolActions: [{ type: 'reauth', accountId: 'fixture-1', method: 'api_key_paste' }, { type: 'closed' }] })
command('multilogin reauthenticates a paste account', { command: 'multilogin', args: 'beta', setup: [{ op: 'add', provider: 'beta', input: { label: 'Beta Main', credential: { type: 'api_key', key: 'sk-beta' } } }], answers: [INPUT('beta-new')], poolActions: [{ type: 'reauth', accountId: 'fixture-1', method: 'api_key_paste' }, { type: 'closed' }] })
command('multilogin loops through add then reauthentication', { command: 'multilogin', args: 'alpha', setup: ALL_POOLS, answers: [SELECT(0), INPUT(''), SELECT(0), INPUT('code-9'), INPUT('sk-2')], poolActions: [{ type: 'add', method: 'oauth' }, { type: 'reauth', accountId: 'fixture-1', method: 'api_key' }, { type: 'closed' }] })

// /multilogout
command('multilogout no UI rejects without output', { command: 'multilogout', args: '', hasUI: false })
command('multilogout non-TUI mode notifies', { command: 'multilogout', args: '', mode: 'rpc' })
command('multilogout without stored accounts', { command: 'multilogout', args: '' })
command('multilogout provider selector cancel', { command: 'multilogout', args: '', setup: [...ALPHA_POOL, { op: 'add', provider: 'beta', input: { label: 'Beta Main', credential: { type: 'api_key', key: 'sk-beta' } } }], answers: [null] })
command('multilogout unknown reference falls back to the provider selector', { command: 'multilogout', args: 'zeta', setup: [...ALPHA_POOL, { op: 'add', provider: 'beta', input: { label: 'Beta Main', credential: { type: 'api_key', key: 'sk-beta' } } }], answers: [SELECT(0), SELECT(1), CONFIRM(false)] })
command('multilogout removes an account picked from the lists', { command: 'multilogout', args: '', setup: [...ALPHA_POOL, { op: 'add', provider: 'beta', input: { label: 'Beta Main', credential: { type: 'api_key', key: 'sk-beta' } } }], answers: [SELECT(1), SELECT(0), CONFIRM(true)] })
command('multilogout reference is case-insensitive and confirmation can decline', { command: 'multilogout', args: 'ALPHA', setup: ALPHA_POOL, answers: [SELECT(1), CONFIRM(false)] })
command('multilogout account selector cancel', { command: 'multilogout', args: 'alpha', setup: ALPHA_POOL, answers: [null] })
command('multilogout duplicate labels remove the first match', { command: 'multilogout', args: 'alpha', setup: [{ op: 'add', provider: 'alpha', input: { label: 'Shared', credential: { type: 'api_key', key: 'sk-1' } } }, { op: 'add', provider: 'alpha', input: { label: 'Shared', credential: { type: 'api_key', key: 'sk-2' } } }], answers: [SELECT(1), CONFIRM(true)] })
command('multilogout removes the last account and the pool', { command: 'multilogout', args: 'beta', setup: [{ op: 'add', provider: 'beta', input: { label: 'Beta Main', credential: { type: 'api_key', key: 'sk-beta' } } }], answers: [SELECT(0), CONFIRM(true)] })

// /accounts
command('accounts without pools', { command: 'accounts', args: '' })
command('accounts lists every provider', { command: 'accounts', args: '', setup: [...ALPHA_POOL, { op: 'add', provider: 'gamma', input: { label: 'Gamma Main', credential: { type: 'oauth', refresh: 'g-r', access: 'g-a', expires: 1700000600000 } } }], answers: [SELECT(0)] })
command('accounts without UI prints nothing', { command: 'accounts', args: '', hasUI: false, setup: ALPHA_POOL })
command('accounts shows priority policy and disabled accounts', { command: 'accounts', args: '', setup: [...ALPHA_POOL, { op: 'setPool', provider: 'alpha', input: { policy: 'priority', affinity: false } }, { op: 'account', provider: 'alpha', account: 'fixture-2', input: { enabled: false, weight: 3, priority: 2 } }], answers: [null] })
command('accounts shows a cooled-down account after a failed request', { command: 'accounts', args: '', setup: COOLDOWN_SETUP, stream: COOLDOWN, answers: [SELECT(0)] })

// /switch-account
const ACCOUNTS_MODEL = { provider: 'alpha', id: 'alpha-1', name: 'Alpha 1' }
command('switch-account no UI rejects without output', { command: 'switch-account', args: '', hasUI: false, model: ACCOUNTS_MODEL, setup: ALPHA_POOL })
command('switch-account proceeds in non-TUI mode with UI', { command: 'switch-account', args: '', mode: 'rpc', model: ACCOUNTS_MODEL, setup: ALPHA_POOL, answers: [null] })
command('switch-account without a selected model', { command: 'switch-account', args: '', model: null })
command('switch-account reports a provider without pools', { command: 'switch-account', args: '', model: { provider: 'gamma', id: 'gamma-1', name: 'Gamma 1' } })
command('switch-account exact match pins the account', { command: 'switch-account', args: 'work', model: ACCOUNTS_MODEL, setup: ALPHA_POOL })
command('switch-account selected journal failure stops success UI', { command: 'switch-account', args: 'work', model: ACCOUNTS_MODEL, setup: ALPHA_POOL, journalError: 'journal commit refused' })
command('switch-account unique prefix pins the account', { command: 'switch-account', args: 'te', model: ACCOUNTS_MODEL, setup: ALPHA_POOL })
command('switch-account ambiguous prefix asks for a choice', { command: 'switch-account', args: 'te', model: ACCOUNTS_MODEL, setup: [...ALPHA_POOL, { op: 'add', provider: 'alpha', input: { label: 'Tenant', credential: { type: 'api_key', key: 'sk-tenant' } } }], answers: [SELECT(1)] })
command('switch-account auto keyword clears the pin', { command: 'switch-account', args: 'auto', model: ACCOUNTS_MODEL, setup: ALPHA_POOL })
command('switch-account automatic journal failure stops success UI', { command: 'switch-account', args: 'auto', model: ACCOUNTS_MODEL, setup: ALPHA_POOL, journalError: 'journal commit refused' })
command('switch-account automatic reports an already automatic session', { command: 'switch-account', args: 'automatic', model: ACCOUNTS_MODEL, setup: [...ALPHA_POOL, { op: 'setPool', provider: 'alpha', input: { affinity: false } }] })
command('switch-account unmatched reference falls back to the selector', { command: 'switch-account', args: 'zzz', model: ACCOUNTS_MODEL, setup: ALPHA_POOL, answers: [SELECT(0)] })
command('switch-account selector cancel', { command: 'switch-account', args: '', model: ACCOUNTS_MODEL, setup: ALPHA_POOL, answers: [null] })
command('switch-account selector marks the current pin', { command: 'switch-account', args: '', model: ACCOUNTS_MODEL, setup: ALPHA_POOL, entries: [SESSION_PIN({ pool: 'alpha', key: 'session-1', accountId: 'fixture-2', label: 'Team' })], answers: [SELECT(2)] })
command('switch-account labels and pins a cooled-down account', { command: 'switch-account', args: '', model: ACCOUNTS_MODEL, setup: COOLDOWN_SETUP, stream: COOLDOWN, answers: [SELECT(2)] })
command('switch-account disabled account is refused', { command: 'switch-account', args: 'disabled', model: ACCOUNTS_MODEL, setup: [...ALPHA_POOL, { op: 'add', provider: 'alpha', input: { label: 'Disabled Acct', enabled: false, credential: { type: 'api_key', key: 'sk-off' } } }] })
command('switch-account excludes the upstream account while unconfigured', { command: 'switch-account', args: '', model: ACCOUNTS_MODEL, setup: ALPHA_POOL, runtime: { providers: ALL, auth: { alpha: { configured: false } }, oauth: {} }, answers: [SELECT(1)] })
command('switch-account virtual pool id is the scheduler id', { command: 'switch-account', args: '', model: { provider: 'vp', id: 'model', name: 'Model' }, setup: [{ op: 'saveVirtual', input: VIRTUAL }, ...ALPHA_POOL], answers: [SELECT(1)] })

// Direct flows and the login-dialog host, from the upstream multilogin spec.
scenarios.push({
  name: 'login helpers run api key and oauth flows (multilogin.spec)',
  kind: 'direct-login',
  flows: [
    { provider: 'alpha', authType: 'api_key', answers: ['simulated-api-key'] },
    { provider: 'alpha', authType: 'oauth', answers: ['browser', 'simulated-oauth-code'] },
  ],
})
scenarios.push({ name: 'host select resolves the chosen option (multilogin.spec)', kind: 'host-select', title: 'Add Alpha Co account', message: 'Choose login method:', options: [{ id: 'browser', label: 'Browser OAuth' }, { id: 'manual', label: 'Manual code' }], answers: [SELECT(0)] })
scenarios.push({ name: 'host select cancel rejects and restores the view (multilogin.spec)', kind: 'host-select', title: 'Add Alpha Co account', message: 'Choose login method:', options: [{ id: 'browser', label: 'Browser OAuth' }, { id: 'manual', label: 'Manual code' }], answers: [null] })
scenarios.push({ name: 'login dialog hosts its select without ctx.ui.select (multilogin.spec)', kind: 'login-dialog', provider: 'alpha', authType: 'oauth', answers: [SELECT(0), null] })

// Run ------------------------------------------------------------------------

const output = { reference: { upstream: 'bd97d65a36f541935dc91e33ba15d7910a30f9d7', pi: piVersion, node: process.version }, pinned: PINNED, world: WORLD, scenarios: [] }
for (const scenario of scenarios) {
  sequence = 0
  now = 1700000000000
  let result
  try {
    if (scenario.kind === 'command') result = await runCommand(scenario)
    else if (scenario.kind === 'direct-login') result = await runLoginFlow(scenario)
    else if (scenario.kind === 'host-select') result = await runHostSelect(scenario)
    else if (scenario.kind === 'login-dialog') result = await runLoginDialog(scenario)
    else throw new Error(`unknown scenario kind ${scenario.kind}`)
  } catch (error) {
    throw new Error(`${scenario.name}: ${error.message}`, { cause: error })
  }
  output.scenarios.push({ name: scenario.name, kind: scenario.kind, ...result.input && { input: result.input }, expected: result.expected })
}

await rm(baseDir, { recursive: true, force: true })
const text = `${JSON.stringify(output, null, 2)}\n`
if (write) {
  await writeFile(fixturePath, text)
  process.stdout.write(`wrote ${output.scenarios.length} scenarios\n`)
} else {
  const committed = await readFile(fixturePath, 'utf8')
  if (committed !== text) {
    process.stderr.write('commands-reference.json differs from the regenerated reference\n')
    process.exit(1)
  }
  process.stdout.write(`ok ${output.scenarios.length} scenarios byte-identical\n`)
}
