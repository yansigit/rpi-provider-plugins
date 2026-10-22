// Differential reference for the pool manager screen (`extensions/pool-manager.ts`),
// the settings-view primitives (`extensions/settings-ui.ts`) and the /vprovider
// command (`extensions/multiprovider.ts` `vprovider` handler + its inline
// `VirtualProviderEditorDialog`).
//
// It runs the UNCHANGED upstream modules against a fake Pi (`pi.registerCommand`,
// `pi.on`, `pi.appendEntry`) and a fake ctx whose `ui` is recorded and answered
// from a script. Every Pi UI call is projected to a native dialog control
// (docs/presentation-map-a2.md), the answers are native dialog replies, and the
// projected trace, notifications, store requests, reconcile/scheduler-default
// calls, the final multiprovider-auth.json, the pool buffer and the pool action
// are written to tests/commands-a2-reference.json for the Rust port to replay.
//
// The pool manager is driven through the upstream `openPoolManager` with the
// callback block copied verbatim from the /multilogin handler of
// `extensions/multiprovider.ts` (the same source the Rust port mirrors); the
// callbacks' store calls, `reconcile(ctx)` and `service.updateSchedulerDefaults`
// are recorded so the Rust port can be compared call for call.
//
// Usage (Node 24.22.0, LC_ALL=en_US.UTF-8):
//   node --experimental-transform-types tests/commands-a2-reference.ts          # check
//   node --experimental-transform-types tests/commands-a2-reference.ts --write  # regenerate
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import crypto from 'node:crypto'
import { existsSync } from 'node:fs'
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { syncBuiltinESMExports } from 'node:module'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { pathToFileURL } from 'node:url'

process.env.TZ = 'UTC'
assert.equal(process.version.startsWith('v24.22.'), true, 'run the reference with Node 24.22.0')
assert.equal(new Intl.Collator().resolvedOptions().locale, 'en-US', 'run with LC_ALL=en_US.UTF-8')

const here = import.meta.dirname
const root = resolve(process.env.PI_MULTIPROVIDER_REFERENCE ?? join(here, '../../../../provider-references/multiprovider'))
const fixturePath = join(here, 'commands-a2-reference.json')
const write = process.argv.includes('--write')
const url = (path) => pathToFileURL(join(root, path)).href
const modules = (path) => resolve(root, 'node_modules', path)

const PINNED = {
  'extensions/multiprovider.ts': '086a0d9c4fbc8431e81026e5d689747d1c31dfcdf89c9d180c0bbd7e37c0a267',
  'extensions/pool-manager.ts': '67873d40ae349d13d0293c1b92fabaeb9f3b531a7242a0424c721ff3e323cbe8',
  'extensions/settings-ui.ts': 'bbbf4fcd97d6c9e0e62180b15ea20adbd15129a63f243f4431ad192ae4a0f782',
  'src/index.ts': '90b3b79432e4d556f75b7b8e25fc3fb512cb56293f5263e6b2090465f5cfaa66',
  'src/auth-store.ts': '29916553aa160b3c676ab843891da2b202fadf536fa220980d564b4ed50d48c6',
  'src/service.ts': '2eafa61662b3ee75a96a7b07a6ccad62ca600bbb2139a407b81cd9983564a026',
  'src/types.ts': 'aaf5e77ffc5738bfc5ee642a61238793ec0869123ceef12a9650c1384b0d181e',
  'src/multilogin.ts': 'f6ce501e18b0fb8550fcfbbe77ace3d06cde9bd5b2ad25b29bc8f2e079c590dc',
  'src/virtual.ts': '6c38efe1d8bcd180e3287b182df592914a1af0a7091f872cd6dc32042575f808',
  'src/session-pins.ts': '45b1ff1fbfcc6853a717b917aaede404f32fe96a64208c75283a53e954bdc005',
  'src/managed.ts': '5a139118f805ed4825259df761dd5e2fbec515da19d3bc10b9719c9bf30bdeef',
  'src/announcement.ts': 'fc0bf72467dba7b524146ade4a66c420d546a01e7867b8095c8b7d8b1804ae18',
  'src/errors.ts': '1e4f189fc4aac4488c84912b29bfc7e78629600ead41ce2924c8d8c3a3bac4b7',
  'src/lift.ts': '887b91aab533565504449b6094c1a2b076832b0fdc73bfc352e2c46a53ac3695',
  'src/register.ts': '946932988b5884081d0f173b464e1b2d42740849803463bdb8fd9841fcc87449',
  'node_modules/@earendil-works/pi-tui/dist/components/settings-list.js': 'ef0ba838df7a8ad0c6f833e9263fe6ac03033008f2283e6106033a8cf5f3af30',
  'node_modules/@earendil-works/pi-tui/dist/components/select-list.js': '4c8fe79d053ce465454c55bbeb7e61d84ea875f0b9bce8c6e238f3d7721c7f42',
  'node_modules/@earendil-works/pi-tui/dist/components/input.js': '2962cbaae94876aee9d838a5eb28287323c06ac7d47277c801500a660eb738b9',
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
const randomUUIDOriginal = crypto.randomUUID
crypto.randomUUID = () => (new Error().stack ?? '').includes('writeStateUnlocked')
  ? randomUUIDOriginal()
  : `fixture-${++sequence}`
syncBuiltinESMExports()

const baseDir = await mkdtemp(join(tmpdir(), 'rpi-multiprovider-commands-a2-base-'))
process.env.PI_CODING_AGENT_DIR = baseDir
const { createProvider } = await import(modules('@earendil-works/pi-ai/dist/index.js'))
const multiprovider = (await import(url('extensions/multiprovider.ts'))).default
const { openPoolManager } = await import(url('extensions/pool-manager.ts'))
const { SelectSubmenu, SectionSubmenu, IntegerInputSubmenu, StringInputSubmenu, formatMs } = await import(url('extensions/settings-ui.ts'))
const { MultiAuthStore } = await import(url('src/auth-store.ts'))

// A plain-text theme: the fake ctx renders no colors, so projected labels are
// the plain strings the native dialogs carry. Upstream only uses fg/bold.
const THEME = { fg: (_color, text) => text, bold: (text) => text }

const clone = (value) => JSON.parse(JSON.stringify(value))

function modelOf(providerId, model) {
  return {
    id: model.id,
    name: model.name,
    api: 'test-api',
    provider: providerId,
    baseUrl: 'https://login.invalid',
    reasoning: model.thinkingLevelMap !== undefined,
    ...(model.thinkingLevelMap === undefined ? {} : { thinkingLevelMap: model.thinkingLevelMap }),
    input: ['text'],
    cost: { input: 1, output: 2, cacheRead: 0.5, cacheWrite: 0 },
    contextWindow: 1000,
    maxTokens: 100,
  }
}
function templateOf(modelId) {
  for (const [providerId, def] of Object.entries(WORLD)) {
    const model = def.models.find((candidate) => candidate.id === modelId)
    if (model !== undefined) {
      const captured = modelOf(providerId, model)
      return {
        api: captured.api,
        baseUrl: captured.baseUrl,
        reasoning: captured.reasoning,
        ...(captured.thinkingLevelMap === undefined ? {} : { thinkingLevelMap: captured.thinkingLevelMap }),
        input: [...captured.input],
        cost: { ...captured.cost },
        contextWindow: captured.contextWindow,
        maxTokens: captured.maxTokens,
      }
    }
  }
  throw new Error(`unknown model ${modelId}`)
}

const BROWSER = {
  steps: [
    { notify: { type: 'device_code', userCode: 'ABCD-1234', verificationUri: 'https://login.invalid/device' } },
    { prompt: { type: 'manual_code', message: 'Paste the authorization code' }, as: 'code' },
  ],
  credential: { type: 'oauth', refresh: 'alpha-refresh', access: '{code}', expires: 1700000060000 },
}
const SECRET = {
  steps: [{ prompt: { type: 'secret', message: 'Enter API key' }, as: 'key' }],
  credential: { type: 'api_key', key: '{key}' },
}
const FAIL = {
  steps: [{ notify: { type: 'info', message: 'Gamma requires the console' } }],
  fail: 'Gamma sign-in rejected',
}

const WORLD = {
  alpha: {
    id: 'alpha', name: 'Alpha Co',
    oauth: { name: 'Alpha OAuth', loginLabel: 'Alpha sign-in', login: BROWSER },
    apiKey: { name: 'API key', login: SECRET },
    models: [{ id: 'alpha-1', name: 'Alpha 1' }, { id: 'alpha-2', name: 'Alpha 2', thinkingLevelMap: { high: 'high-effort', off: null } }],
  },
  beta: { id: 'beta', name: 'Beta Labs', apiKey: { name: 'API key', login: SECRET }, models: [{ id: 'beta-1', name: 'Beta 1' }] },
  paster: { id: 'paster', name: 'Paster Inc', apiKey: { name: 'Paste key' }, models: [{ id: 'paster-1', name: 'Paster 1' }] },
  delta: { id: 'delta', name: 'Delta None', models: [{ id: 'delta-1', name: 'Delta 1' }] },
  epsilon: { id: 'epsilon', name: 'Epsilon Plain', oauth: { name: 'Epsilon OAuth' }, models: [{ id: 'eps-1', name: 'Eps 1' }] },
  gamma: { id: 'gamma', name: 'Gamma AI', oauth: { name: 'Gamma OAuth', login: FAIL }, models: [{ id: 'gamma-1', name: 'Gamma 1' }] },
  zeta: { id: 'zeta', name: 'Zeta Zero', models: [] },
}
const ALL = Object.keys(WORLD)
const DEFAULT_RUNTIME = {
  providers: ALL,
  auth: { alpha: { configured: true, label: 'Alpha OAuth' }, gamma: { configured: true, source: 'GAMMA_TOKEN' } },
  oauth: { alpha: true, gamma: true },
}

const WORK = { label: 'Work', credential: { type: 'oauth', refresh: 'refresh-work', access: 'access-work', expires: 1700000600000 } }
const TEAM = { label: 'Team', credential: { type: 'api_key', key: 'sk-team' } }
const VIRTUAL = { id: 'vp', label: 'Virtual Pool', models: [{ id: 'model', label: 'Model', backends: [{ providerId: 'alpha', modelId: 'alpha-1', template: templateOf('alpha-1') }] }] }

function makeProvider(id) {
  const def = WORLD[id]
  const auth = {}
  if (def.oauth) {
    auth.oauth = {
      name: def.oauth.name,
      ...(def.oauth.loginLabel === undefined ? {} : { loginLabel: def.oauth.loginLabel }),
      ...(def.oauth.login === undefined ? {} : { login: async () => { throw new Error('not used') } }),
      async refresh(credential) { return credential },
      async toAuth(credential) { return { apiKey: credential.access } },
    }
  }
  if (def.apiKey) {
    auth.apiKey = {
      name: def.apiKey.name,
      ...(def.apiKey.login === undefined ? {} : { login: async () => { throw new Error('not used') } }),
      async resolve() { return undefined },
    }
  }
  return createProvider({
    id,
    name: def.name,
    auth,
    models: def.models.map((model) => modelOf(id, model)),
    api: { stream() { throw new Error('not used') }, streamSimple() { throw new Error('not used') } },
  })
}

// Fake Pi / ctx --------------------------------------------------------------

function registryFor(scenario) {
  const providers = new Map()
  for (const id of scenario.providers ?? ALL) providers.set(id, makeProvider(id))
  const runtimeSpec = scenario.runtime === undefined ? DEFAULT_RUNTIME : scenario.runtime
  const runtime = runtimeSpec === null ? undefined : {
    getProviders: () => runtimeSpec.providers.map((id) => providers.get(id)).filter((provider) => provider !== undefined),
    getProviderAuthStatus: (id) => runtimeSpec.auth?.[id] ?? { configured: false },
    isUsingOAuth: (id) => runtimeSpec.oauth?.[id] === true,
  }
  const models = [...providers.keys()].flatMap((id) => (WORLD[id]?.models ?? []).map((model) => modelOf(id, model)))
  return {
    providers,
    runtime,
    getAll: () => models,
    getProvider: (id) => providers.get(id),
    getProviderAuthStatus: (id) => runtimeSpec?.auth?.[id] ?? { configured: false },
  }
}

function makePi(registry, commands, appended, bus, handlers) {
  return {
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
    appendEntry(customType, data) { appended.push({ customType, data: clone(data) }) },
    getAllTools: () => [],
  }
}

let trace = []
let answers = []
// The native replies for every control the drivers show, in order. They are the
// input half of the fixture: the Rust fake Ui replays them.
let controlReplies = []
let authPath = ''
const norm = (text) => (authPath === '' ? text : text.replaceAll(authPath, '$AUTH_PATH'))
const notifications = () => trace.filter((item) => item.notify !== undefined).map((item) => item.notify)

function makeUi(hasUI, custom) {
  const note = (text, kind) => trace.push({ notify: { text: norm(text), kind } })
  const show = (control) => {
    assert.ok(answers.length > 0, `unscripted control ${JSON.stringify(control)}`)
    const reply = answers.shift() ?? null
    trace.push({ control, reply })
    return reply
  }
  const ui = {
    // Pi's non-interactive UI drops notifications; the projection records only
    // what an attached UI would show.
    notify(text, kind) { if (hasUI) note(text, kind ?? 'info') },
    custom(factory) { return custom(factory) },
  }
  if (!hasUI) {
    return { ...ui, select: async () => undefined, confirm: async () => false, input: async () => undefined }
  }
  return {
    ...ui,
    select(title, labels) {
      const reply = show({ kind: 'select', title, choices: labels.map((label, index) => ({ label, value: String(index) })) })
      if (reply === null) return Promise.resolve(undefined)
      assert.equal(reply.type, 'select')
      assert.equal(reply.values.length, 1)
      const index = Number(reply.values[0])
      assert.ok(index < labels.length)
      return Promise.resolve(labels[index])
    },
    confirm(title, message) {
      const reply = show({ kind: 'confirm', title, description: message })
      if (reply === null) return Promise.resolve(false)
      assert.equal(reply.type, 'confirm')
      return Promise.resolve(reply.confirmed === true)
    },
    input(title, placeholder) {
      const reply = show({ kind: 'input', title, placeholder: placeholder ?? '', masked: false })
      if (reply === null) return Promise.resolve(undefined)
      assert.equal(reply.type, 'input')
      return Promise.resolve(reply.value)
    },
  }
}

// Native projections ---------------------------------------------------------

const stripAnsi = (text) => String(text).replace(/\x1b\[[0-9;]*m/g, '')
const textOf = (text) => stripAnsi(text)
const textChildren = (component) => (component.children ?? []).filter((child) => typeof child.text === 'string')

function settingRows(list) {
  return list.items.map((item) => {
    const row = { id: item.id, label: textOf(item.label), value: textOf(item.currentValue ?? '') }
    if (item.description !== undefined) row.description = textOf(item.description)
    if (item.values !== undefined) row.values = [...item.values]
    if (item.submenu) row.submenu = true
    return row
  })
}
function settingQuery(list) {
  return list.searchInput === undefined ? '' : list.searchInput.getValue()
}
function settingsControl(title, description, list, selected) {
  const control = {
    kind: 'settings',
    title: textOf(title),
    description: textOf(description),
    searchable: true,
    settings: settingRows(list),
    query: settingQuery(list),
  }
  if (selected !== undefined) control.selected = selected
  return control
}
function selectControl(title, description, options, selectedIndex) {
  const control = {
    kind: 'select',
    title: textOf(title),
    choices: options.map((option) => ({ value: option.value, label: textOf(option.label) })),
  }
  if (description !== undefined) control.description = textOf(description)
  const option = options[selectedIndex]
  if (option !== undefined) control.selected = option.value
  return control
}
function inputControl(title, description, value, integerMin) {
  const control = {
    kind: 'input',
    title: textOf(title),
    description: textOf(description),
    initialValue: value,
    trim: true,
  }
  if (integerMin !== undefined) control.integerMin = integerMin
  return control
}

// selectAfterFor from extensions/pool-manager.ts.
function selectAfterFor(id) {
  const segments = id.split('.')
  if (segments[0] === 'account' || segments[0] === 'upstream' || segments[0] === 'stored') return 'accounts'
  return segments[0] ?? id
}

// Pool manager driver --------------------------------------------------------

// The row highlighted in a settings page. The port tracks the same index into
// the full row list: 0 initially, the opened row while a submenu is up, and
// selectAfterFor(change) after a change rebuilds the screen.
const selection = new WeakMap()
function selectedFor(list) {
  const stored = selection.get(list)
  if (stored !== undefined) return stored
  return list.items[0]?.id
}

// component.refresh(selectAfterFor(id)): selectItem keeps the current index when
// the id is absent, so the first row stays highlighted.
function selectionAfter(list, id) {
  const after = selectAfterFor(id)
  if (list.items.some((item) => item.id === after)) return after
  return list.items[0]?.id
}

function poolPage(component) {
  let list = component.settingsList
  let title = component.titleText.text
  let description = component.subtitleText.text
  for (;;) {
    const submenu = list.submenuComponent
    if (submenu === null || submenu === undefined) {
      return { kind: 'settings', list, control: settingsControl(title, description, list, selectedFor(list)) }
    }
    const texts = textChildren(submenu)
    const pageTitle = texts[0]?.text ?? ''
    const pageDescription = texts[1]?.text ?? ''
    if (submenu instanceof SelectSubmenu) {
      return { kind: 'select', list, submenu, control: selectControl(pageTitle, pageDescription, submenu.selectList.items, submenu.selectList.selectedIndex) }
    }
    if (submenu instanceof SectionSubmenu) {
      list = submenu.settingsList
      title = pageTitle
      description = pageDescription
      continue
    }
    if (submenu instanceof IntegerInputSubmenu) {
      // The row's own minimum: integerInputSubmenu(theme, 'Weight', …, 1) and
      // (theme, 'Priority', …, 0); the minimum is captured in a closure upstream.
      return { kind: 'input', list, submenu, control: inputControl(pageTitle, pageDescription, submenu.input.getValue(), pageTitle === 'Weight' ? 1 : 0), validation: texts[2]?.text ?? '' }
    }
    if (submenu instanceof StringInputSubmenu) {
      return { kind: 'input', list, submenu, control: inputControl(pageTitle, pageDescription, submenu.input.getValue(), undefined), validation: '' }
    }
    throw new Error('unknown pool manager submenu')
  }
}

function displayedRow(list, id) {
  const items = typeof list.getDisplayItems === 'function' ? list.getDisplayItems() : list.items
  const index = items.findIndex((item) => item.id === id)
  assert.notEqual(index, -1, `setting row ${id} is not displayed`)
  return { item: items[index], index }
}

async function settlePool(component, before, invocation) {
  for (let attempt = 0; attempt < 2000; attempt += 1) {
    if (invocation.finished || component.settingsList !== before) return
    await new Promise((resolveTick) => setTimeout(resolveTick, 0))
  }
  throw new Error('the pool manager did not settle after a change')
}

// The upstream persist path is the SettingsList's onChange. Wrapping it tells
// the driver whether a leaf's `done` carried a value (persist) or not (a plain
// cancel such as the remove row's "Cancel" option), which decides whether the
// screen is rebuilt.
const watchedLists = new WeakSet()
let persistFired = false
function watchChanges(list) {
  if (watchedLists.has(list)) return
  watchedLists.add(list)
  const original = list.onChange
  list.onChange = (id, value) => { persistFired = true; original.call(list, id, value) }
}

async function drivePoolManager(component, steps, invocation) {
  let index = 0
  let lastOpened
  while (!invocation.finished) {
    const step = steps[index]
    assert.ok(step !== undefined, 'pool manager steps ended while the dialog is open')
    index += 1
    // The control is projected before the host's search box is typed into: the
    // query only travels back with the reply.
    const page = poolPage(component)
    if (step.search !== undefined) {
      assert.equal(page.kind, 'settings', 'search needs a settings page')
      assert.ok(page.list.searchInput !== undefined, 'search needs a searchable settings list')
      for (const char of step.search) page.list.handleInput(char)
    }
    if (step.change !== undefined) {
      assert.equal(page.kind, 'settings')
      const before = component.settingsList
      const { item, index: rowIndex } = displayedRow(page.list, step.change)
      assert.ok(item.values !== undefined && !item.submenu, `setting row ${step.change} does not cycle values`)
      watchChanges(page.list)
      persistFired = false
      page.list.selectedIndex = rowIndex
      page.list.handleInput('\r')
      assert.ok(persistFired, `setting row ${step.change} did not persist`)
      const reply = { type: 'change', id: step.change, value: item.currentValue, query: settingQuery(page.list) }
      controlReplies.push(reply)
      trace.push({ control: page.control, reply })
      await settlePool(component, before, invocation)
      if (!invocation.finished) selection.set(component.settingsList, selectionAfter(component.settingsList, step.change))
      continue
    }
    if (step.open !== undefined) {
      assert.equal(page.kind, 'settings')
      const { item, index: rowIndex } = displayedRow(page.list, step.open)
      assert.ok(item.submenu, `setting row ${step.open} has no submenu`)
      page.list.selectedIndex = rowIndex
      selection.set(page.list, step.open)
      lastOpened = step.open
      page.list.handleInput('\r')
      const reply = { type: 'open', id: step.open, query: settingQuery(page.list) }
      controlReplies.push(reply)
      trace.push({ control: page.control, reply })
      continue
    }
    if (step.pick !== undefined) {
      assert.equal(page.kind, 'select')
      const before = component.settingsList
      const options = page.submenu.selectList.items
      const optionIndex = options.findIndex((option) => option.value === step.pick)
      assert.notEqual(optionIndex, -1, `select option ${step.pick} is not available`)
      watchChanges(page.list)
      persistFired = false
      page.submenu.selectList.setSelectedIndex(optionIndex)
      page.submenu.handleInput('\r')
      const reply = { type: 'select', values: [step.pick] }
      controlReplies.push(reply)
      trace.push({ control: page.control, reply })
      if (!persistFired) continue
      await settlePool(component, before, invocation)
      if (!invocation.finished) selection.set(component.settingsList, selectionAfter(component.settingsList, lastOpened))
      continue
    }
    if (step.submit !== undefined) {
      assert.equal(page.kind, 'input')
      const before = component.settingsList
      let value = step.submit
      if (page.control.integerMin !== undefined) {
        const parsed = /^\d+$/.test(value) ? Number(value) : Number.NaN
        assert.ok(Number.isSafeInteger(parsed) && parsed >= page.control.integerMin, `input ${value} fails the native integer validation`)
        value = String(parsed)
      }
      watchChanges(page.list)
      persistFired = false
      page.submenu.input.setValue(value)
      page.submenu.input.onSubmit(value)
      const reply = { type: 'input', value }
      controlReplies.push(reply)
      trace.push({ control: page.control, reply })
      assert.equal(page.validation, '', `upstream rejected the projected input ${value}`)
      assert.ok(persistFired, `input ${value} did not persist`)
      await settlePool(component, before, invocation)
      if (!invocation.finished) selection.set(component.settingsList, selectionAfter(component.settingsList, lastOpened))
      continue
    }
    if (step.cancel === true) {
      const before = component.settingsList
      component.settingsList.handleInput('\x1b')
      controlReplies.push(null)
      trace.push({ control: page.control, reply: null })
      if (invocation.finished) return
      assert.equal(component.settingsList, before, 'cancelling a submenu must keep the parent list')
      continue
    }
    throw new Error(`unknown pool manager step ${JSON.stringify(step)}`)
  }
}

// /vprovider editor driver ---------------------------------------------------

function editorPage(component) {
  const texts = textChildren(component.pageContainer)
  const title = texts[0]?.text ?? ''
  const description = texts[1]?.text ?? ''
  const error = texts[2]?.text ?? ''
  const merged = error === '' ? description : `${description}\n${error}`
  if (component.activeInput !== undefined) {
    return { kind: 'input', control: inputControl(title, merged, component.activeInput.getValue(), title === 'Set weight' ? 1 : undefined) }
  }
  assert.ok(component.activeList !== undefined, 'the editor page has no list or input')
  const list = component.activeList
  return { kind: 'settings', list, control: settingsControl(title, merged, list, list.items[0]?.id) }
}

function driveEditor(component, steps, invocation) {
  let index = 0
  while (!invocation.finished) {
    const step = steps[index]
    assert.ok(step !== undefined, 'editor steps ended while the dialog is open')
    index += 1
    const page = editorPage(component)
    if (step.change !== undefined) {
      assert.equal(page.kind, 'settings')
      const { item, index: rowIndex } = displayedRow(page.list, step.change)
      page.list.selectedIndex = rowIndex
      page.list.handleInput('\r')
      const reply = { type: 'change', id: step.change, value: item.currentValue, query: settingQuery(page.list) }
      controlReplies.push(reply)
      trace.push({ control: page.control, reply })
      continue
    }
    if (step.submit !== undefined) {
      assert.equal(page.kind, 'input')
      let value = step.submit
      if (page.control.integerMin !== undefined) {
        const parsed = /^\d+$/.test(value) ? Number(value) : Number.NaN
        assert.ok(Number.isSafeInteger(parsed) && parsed >= page.control.integerMin, `input ${value} fails the native integer validation`)
        value = String(parsed)
      }
      component.activeInput.setValue(value)
      component.activeInput.onSubmit(value)
      const reply = { type: 'input', value }
      controlReplies.push(reply)
      trace.push({ control: page.control, reply })
      continue
    }
    if (step.cancel === true) {
      component.handleInput('\x1b')
      controlReplies.push(null)
      trace.push({ control: page.control, reply: null })
      continue
    }
    throw new Error(`unknown editor step ${JSON.stringify(step)}`)
  }
}

// Scenario state -------------------------------------------------------------

function loggedStore(store) {
  return {
    getPool: async (id) => { trace.push({ store: { op: 'pool', provider: id } }); return store.getPool(id) },
    getSchedulerSettings: async () => { trace.push({ store: { op: 'getScheduler' } }); return store.getSchedulerSettings() },
    updatePool: async (id, settings) => {
      trace.push({ store: { op: 'setPool', provider: id, input: clone(settings) } })
      return store.updatePool(id, settings)
    },
    updateAccount: async (id, accountId, settings) => {
      trace.push({ store: { op: 'account', provider: id, account: accountId, input: clone(settings) } })
      return store.updateAccount(id, accountId, settings)
    },
    removeAccount: async (id, accountId) => {
      trace.push({ store: { op: 'remove', provider: id, account: accountId } })
      return store.removeAccount(id, accountId)
    },
    updateSchedulerSettings: async (patch) => {
      const input = {}
      const clear = []
      for (const [key, value] of Object.entries(patch)) {
        if (value === undefined) clear.push(key)
        else input[key] = value
      }
      const request = { op: 'setScheduler', input }
      if (clear.length > 0) request.clear = clear
      trace.push({ store: request })
      return store.updateSchedulerSettings(patch)
    },
  }
}

async function runSetup(scenario, store) {
  const ids = []
  for (const op of scenario.setup ?? []) {
    if (op.op === 'add') ids.push((await store.addAccount(op.provider, clone(op.input))).id)
    else if (op.op === 'setPool') await store.updatePool(op.provider, clone(op.input))
    else if (op.op === 'account') await store.updateAccount(op.provider, op.account === 'last' ? ids.at(-1) : op.account, clone(op.input))
    else if (op.op === 'setScheduler') await store.updateSchedulerSettings(clone(op.input))
    else if (op.op === 'saveVirtual') await store.saveVirtualProvider(clone(op.input))
    else throw new Error(`unknown setup operation ${op.op}`)
  }
  return ids
}

async function withStore(scenario, body) {
  const dir = await mkdtemp(join(tmpdir(), 'rpi-multiprovider-commands-a2-'))
  authPath = join(dir, 'multiprovider-auth.json')
  process.env.PI_CODING_AGENT_DIR = dir
  try {
    now = 1700000000000
    sequence = 0
    const store = new MultiAuthStore(authPath)
    await runSetup(scenario, store)
    const initialAuth = existsSync(authPath) ? await readFile(authPath, 'utf8') : null
    const idSequence = sequence
    const result = await body(store, { dir, initialAuth, idSequence })
    const authFile = existsSync(authPath) ? await readFile(authPath, 'utf8') : null
    return { result, authFile }
  } finally {
    globalThis[Symbol.for('pi-multiprovider.live-providers')]?.clear()
    await rm(dir, { recursive: true, force: true })
  }
}

function scriptRunner(scenario) {
  const stepScripts = clone(scenario.steps ?? [])
  const drives = []
  const invocationFor = (name, drive) => (factory) => new Promise((resolveOutcome, rejectOutcome) => {
    const invocation = { finished: false }
    const done = (outcome) => { invocation.finished = true; resolveOutcome(outcome) }
    const component = factory({ requestRender() {} }, THEME, {}, done)
    assert.equal(component.constructor.name, name)
    const steps = stepScripts.shift()
    assert.ok(steps !== undefined, `unscripted ${name} invocation`)
    const drivePromise = Promise.resolve().then(() => drive(component, steps, invocation))
    drivePromise.catch((error) => { invocation.finished = true; rejectOutcome(error) })
    drives.push(drivePromise)
  })
  return {
    custom: invocationFor(scenario.component, scenario.drive),
    finish() {
      assert.deepEqual(stepScripts, [], 'every scripted dialog invocation is consumed')
      return Promise.all(drives)
    },
  }
}

// The pool manager callbacks, copied verbatim from the /multilogin handler of
// extensions/multiprovider.ts (lines 1110-1189), with the store call log and
// `reconcile`/`service.updateSchedulerDefaults` recorded by the harness.
async function runPoolScreen(scenario) {
  const registry = registryFor(scenario)
  const provider = registry.providers.get(scenario.provider)
  return withStore(scenario, async (store, extra) => {
    const log = loggedStore(store)
    const initialPool = await store.getPool(provider.id)
    let buffer = {
      policy: initialPool?.policy ?? 'round-robin',
      affinity: initialPool?.affinity ?? true,
      includeUpstream: initialPool?.includeUpstream ?? true,
      upstream: { ...(initialPool?.upstream ?? {}) },
    }
    const initialBuffer = clone(buffer)
    const reconcile = async () => { trace.push({ reconcile: true }) }
    const callbacks = {
      async loadState() {
        const pool = await log.getPool(provider.id)
        const scheduler = await log.getSchedulerSettings()
        const runtime = registry.runtime
        const upstreamStatus = runtime?.getProviderAuthStatus(provider.id)
        const upstreamConfigured = upstreamStatus !== undefined && upstreamStatus.configured
        const upstreamSource = upstreamConfigured ? (upstreamStatus.label ?? upstreamStatus.source) : undefined
        const upstreamState = {
          ...(upstreamConfigured ? { upstreamConfigured } : {}),
          ...(upstreamSource === undefined ? {} : { upstreamSource }),
        }
        if (pool === undefined) {
          return {
            poolExists: false,
            policy: buffer.policy,
            affinity: buffer.affinity,
            includeUpstream: buffer.includeUpstream,
            upstream: { ...buffer.upstream },
            accounts: [],
            scheduler,
            ...upstreamState,
          }
        }
        buffer = {
          policy: pool.policy,
          affinity: pool.affinity,
          includeUpstream: pool.includeUpstream,
          upstream: { ...(pool.upstream ?? {}) },
        }
        return {
          poolExists: true,
          policy: pool.policy,
          affinity: pool.affinity,
          includeUpstream: pool.includeUpstream,
          upstream: { ...(pool.upstream ?? {}) },
          accounts: pool.accounts,
          scheduler,
          ...upstreamState,
        }
      },
      async updatePool(settings) {
        if (await log.getPool(provider.id) === undefined) {
          if (settings.policy !== undefined) buffer.policy = settings.policy
          if (settings.affinity !== undefined) buffer.affinity = settings.affinity
          if (settings.includeUpstream !== undefined) buffer.includeUpstream = settings.includeUpstream
          if (settings.upstream !== undefined) buffer.upstream = { ...settings.upstream }
          return
        }
        await log.updatePool(provider.id, settings)
        await reconcile()
      },
      async updateAccount(accountId, settings) {
        await log.updateAccount(provider.id, accountId, settings)
        await reconcile()
      },
      async removeAccount(accountId) {
        await log.removeAccount(provider.id, accountId)
        await reconcile()
      },
      async updateScheduler(key, valueMs) {
        const patch = { [key]: valueMs }
        const effective = await log.updateSchedulerSettings(patch)
        trace.push({ schedulerDefaults: clone(effective) })
      },
    }

    trace = []
    answers = []
    answers.push(...clone(scenario.answers ?? []))
    controlReplies = []
    const runner = scriptRunner({ component: 'PoolManagerComponent', drive: drivePoolManager, steps: scenario.steps })
    const ui = makeUi(scenario.hasUI !== false, runner.custom)
    const ctx = { hasUI: scenario.hasUI !== false, mode: scenario.mode ?? 'tui', ui }
    const methods = scenario.methods.map((method) => ({ ...method }))
    let action = null
    let error = null
    try {
      action = await openPoolManager(ctx, provider, callbacks, methods)
    } catch (caught) {
      if (caught instanceof assert.AssertionError) throw caught
      error = norm(caught.message)
    }
    await runner.finish()
    assert.deepEqual(answers, [], 'every scripted answer is consumed')
    const answersRecorded = [...clone(controlReplies), ...clone(scenario.answers ?? [])]
    return {
      input: {
        provider: scenario.provider,
        methods: scenario.methods,
        hasUI: ctx.hasUI,
        mode: ctx.mode,
        setup: scenario.setup ?? [],
        initialBuffer,
        answers: answersRecorded,
        steps: scenario.steps ?? [],
        runtime: scenario.runtime === undefined ? DEFAULT_RUNTIME : scenario.runtime,
        providers: scenario.providers ?? ALL,
        initialAuth: extra.initialAuth,
        idSequence: extra.idSequence,
      },
      expected: {
        trace,
        notifications: notifications(),
        buffer: clone(buffer),
        action: clone(action),
        authFile: null,
        error,
      },
    }
  }).then(({ result, authFile }) => {
    result.expected.authFile = authFile
    return result
  })
}

async function runVproviderCommand(scenario) {
  const registry = registryFor(scenario)
  return withStore(scenario, async (store, extra) => {
    const bus = new Map()
    const handlers = new Map()
    const commands = new Map()
    const appended = []
    const pi = makePi(registry, commands, appended, bus, handlers)
    trace = []
    controlReplies = []
    const runner = scriptRunner({ component: 'VirtualProviderEditorDialog', drive: driveEditor, steps: scenario.steps })
    const ui = makeUi(scenario.hasUI !== false, runner.custom)
    const ctx = {
      cwd: extra.dir,
      mode: scenario.mode ?? 'tui',
      hasUI: scenario.hasUI !== false,
      model: scenario.model ?? undefined,
      isIdle: () => true,
      sessionManager: { getSessionId: () => 'session-1', getEntries: () => [] },
      modelRegistry: registry,
      ui,
    }
    await multiprovider(pi)
    await handlers.get('session_start')({}, ctx)
    trace = []
    answers = []
    answers.push(...clone(scenario.answers ?? []))
    let error = null
    try {
      await commands.get('vprovider').handler(scenario.args ?? '', ctx)
    } catch (caught) {
      if (caught instanceof assert.AssertionError) throw caught
      error = norm(caught.message)
    }
    await runner.finish()
    assert.deepEqual(answers, [], 'every scripted answer is consumed')
    await handlers.get('session_shutdown')?.({}, ctx)
    const answersRecorded = [...clone(controlReplies), ...clone(scenario.answers ?? [])]
    return {
      input: {
        command: 'vprovider',
        args: scenario.args ?? '',
        hasUI: ctx.hasUI,
        mode: ctx.mode,
        setup: scenario.setup ?? [],
        answers: answersRecorded,
        steps: scenario.steps ?? [],
        runtime: scenario.runtime === undefined ? DEFAULT_RUNTIME : scenario.runtime,
        providers: scenario.providers ?? ALL,
        initialAuth: extra.initialAuth,
        idSequence: extra.idSequence,
      },
      expected: { trace, notifications: notifications(), entries: appended, authFile: null, error },
    }
  }).then(({ result, authFile }) => {
    result.expected.authFile = authFile
    return result
  })
}

function runFormatMs(scenario) {
  return {
    input: { values: scenario.values },
    expected: { formatted: scenario.values.map((value) => formatMs(value)) },
  }
}

// Scenarios ------------------------------------------------------------------

const scenarios = []
const pool = (name, spec) => scenarios.push({ name, kind: 'pool-screen', ...spec })
const vp = (name, spec) => scenarios.push({ name, kind: 'vprovider', ...spec })
const fm = (name, values) => scenarios.push({ name, kind: 'format-ms', values })

const PASTER_METHODS = [{ label: 'API key · Paste key (paste)', value: 'api_key_paste' }]
const ALPHA_METHODS = [{ label: 'Alpha sign-in', value: 'oauth' }, { label: 'API key', value: 'api_key' }]
const POOL_ALPHA = [
  { op: 'add', provider: 'alpha', input: WORK },
  { op: 'add', provider: 'alpha', input: TEAM },
]
const POOL_PASTER = [{ op: 'add', provider: 'paster', input: { label: 'Paste Main', credential: { type: 'api_key', key: 'sk-paste' } } }]
const CONFIRM = (confirmed) => ({ type: 'confirm', confirmed })

fm('formatMs durations', [100, 250, 500, 1000, 1500, 2000, 2500, 5000, 15000, 30000, 45000, 60000, 90000, 120000, 300000, 900000, 1800000, 3600000, 4500000, 5400000, 7200000, 21600000])

pool('pool manager closes without a pool', { provider: 'paster', methods: PASTER_METHODS, steps: [[{ cancel: true }]] })
pool('changes pool settings before the pool exists', {
  provider: 'paster',
  methods: PASTER_METHODS,
  steps: [[
    { change: 'pool.policy' },
    { change: 'pool.policy' },
    { change: 'pool.affinity' },
    { change: 'pool.affinity' },
    { open: 'accounts' },
    { open: 'accounts.add' },
    { pick: 'api_key_paste' },
  ]],
})
pool('account add returns the method to the caller', {
  provider: 'alpha',
  methods: ALPHA_METHODS,
  steps: [[
    { open: 'accounts' },
    { open: 'accounts.add' },
    { pick: 'oauth' },
  ]],
})
pool('reauth returns the account and method to the caller', {
  provider: 'paster',
  methods: PASTER_METHODS,
  setup: POOL_PASTER,
  steps: [[
    { open: 'accounts' },
    { open: 'stored.fixture-1' },
    { open: 'account.fixture-1.reauth' },
    { pick: 'api_key_paste' },
  ]],
})
pool('pool exists and cycles every root row', {
  provider: 'paster',
  methods: PASTER_METHODS,
  setup: POOL_PASTER,
  steps: [[
    { change: 'pool.policy' },
    { change: 'pool.policy' },
    { change: 'pool.policy' },
    { change: 'pool.affinity' },
    { cancel: true },
  ]],
})
pool('edits a stored account and returns to the accounts section', {
  provider: 'paster',
  methods: PASTER_METHODS,
  setup: POOL_PASTER,
  steps: [[
    { open: 'accounts' },
    { open: 'stored.fixture-1' },
    { change: 'account.fixture-1.enabled' },
    { open: 'accounts' },
    { open: 'stored.fixture-1' },
    { open: 'account.fixture-1.weight' },
    { submit: '3' },
    { open: 'accounts' },
    { open: 'stored.fixture-1' },
    { open: 'account.fixture-1.priority' },
    { submit: '2' },
    { open: 'accounts' },
    { open: 'stored.fixture-1' },
    { open: 'account.fixture-1.label' },
    { submit: 'Main key' },
    { open: 'accounts' },
    { open: 'stored.fixture-1' },
    { open: 'account.fixture-1.label' },
    { submit: '' },
    { cancel: true },
  ]],
})
pool('remove account keeps then removes the credential', {
  provider: 'paster',
  methods: PASTER_METHODS,
  setup: POOL_PASTER,
  steps: [[
    { open: 'accounts' },
    { open: 'stored.fixture-1' },
    { open: 'account.fixture-1.remove' },
    { pick: 'keep' },
    { open: 'account.fixture-1.remove' },
    { pick: 'remove' },
    { cancel: true },
  ]],
})
pool('upstream preferences are edited when the provider is configured', {
  provider: 'alpha',
  methods: ALPHA_METHODS,
  setup: [...POOL_ALPHA, { op: 'setPool', provider: 'alpha', input: { policy: 'priority', affinity: false } }],
  steps: [[
    { open: 'accounts' },
    { open: 'upstream' },
    { change: 'upstream.enabled' },
    { open: 'accounts' },
    { open: 'upstream' },
    { open: 'upstream.label' },
    { submit: 'Main login' },
    { open: 'accounts' },
    { open: 'upstream' },
    { open: 'upstream.weight' },
    { submit: '3' },
    { open: 'accounts' },
    { open: 'upstream' },
    { open: 'upstream.priority' },
    { submit: '0' },
    { open: 'accounts' },
    { open: 'upstream' },
    { open: 'upstream.label' },
    { submit: 'Pi default' },
    { cancel: true },
  ]],
})
pool('scheduler cooldowns and the error count are edited', {
  provider: 'paster',
  methods: PASTER_METHODS,
  setup: [{ op: 'setScheduler', input: { rateLimitCooldownMs: 45000 } }, ...POOL_PASTER],
  steps: [[
    { open: 'scheduler' },
    { open: 'scheduler.rateLimitCooldownMs' },
    { pick: '60000' },
    { open: 'scheduler' },
    { open: 'scheduler.quotaCooldownMs' },
    { pick: '900000' },
    { open: 'scheduler' },
    { open: 'scheduler.transientBaseCooldownMs' },
    { pick: '250' },
    { open: 'scheduler' },
    { open: 'scheduler.maxCooldownMs' },
    { pick: 'default' },
    { open: 'scheduler' },
    { open: 'scheduler.errorsBeforeSwitch' },
    { pick: '5' },
    { open: 'scheduler' },
    { open: 'scheduler.errorsBeforeSwitch' },
    { pick: 'default' },
    { cancel: true },
  ]],
})
pool('scheduler summary falls back to the defaults', {
  provider: 'paster',
  methods: PASTER_METHODS,
  setup: POOL_PASTER,
  steps: [[{ open: 'scheduler' }, { cancel: true }, { cancel: true }]],
})
pool('search query survives a submenu round trip', {
  provider: 'paster',
  methods: PASTER_METHODS,
  setup: POOL_PASTER,
  steps: [[
    { open: 'scheduler', search: 'sched' },
    { cancel: true },
    { cancel: true },
  ]],
})
pool('search query is dropped after a change', {
  provider: 'paster',
  methods: PASTER_METHODS,
  setup: POOL_PASTER,
  steps: [[
    { change: 'pool.affinity', search: 'session' },
    { cancel: true },
  ]],
})
pool('cancelling the submenu restores the opened row', {
  provider: 'paster',
  methods: PASTER_METHODS,
  setup: POOL_PASTER,
  steps: [[
    { open: 'scheduler' },
    { cancel: true },
    { open: 'accounts' },
    { cancel: true },
    { cancel: true },
  ]],
})
pool('empty pool with an upstream credential detected', {
  provider: 'alpha',
  methods: ALPHA_METHODS,
  runtime: { providers: ALL, auth: { alpha: { configured: true, source: 'ALPHA_TOKEN' } }, oauth: {} },
  steps: [[{ open: 'accounts' }, { open: 'upstream' }, { cancel: true }, { cancel: true }, { cancel: true }]],
})
pool('no accounts yet with an upstream pending row', {
  provider: 'alpha',
  methods: ALPHA_METHODS,
  runtime: { providers: ALL, auth: { alpha: { configured: true, label: 'Alpha login' } }, oauth: { alpha: true } },
  steps: [[{ open: 'accounts' }, { cancel: true }, { cancel: true }]],
})

pool('reauth prefers the stored oauth credential kind', {
  provider: 'alpha',
  methods: ALPHA_METHODS,
  setup: POOL_ALPHA,
  steps: [[
    { open: 'accounts' },
    { open: 'stored.fixture-1' },
    { open: 'account.fixture-1.reauth' },
    { pick: 'oauth' },
  ]],
})
pool('reauth prefers the stored api-key credential kind', {
  provider: 'alpha',
  methods: ALPHA_METHODS,
  setup: POOL_ALPHA,
  steps: [[
    { open: 'accounts' },
    { open: 'stored.fixture-2' },
    { open: 'account.fixture-2.reauth' },
    { pick: 'api_key' },
  ]],
})
pool('a pool without login methods has no add or reauth rows', {
  provider: 'paster',
  methods: [],
  setup: POOL_PASTER,
  steps: [[{ open: 'accounts' }, { cancel: true }, { cancel: true }]],
})
pool('cancelling the weight input returns to the account section', {
  provider: 'paster',
  methods: PASTER_METHODS,
  setup: POOL_PASTER,
  steps: [[
    { open: 'accounts' },
    { open: 'stored.fixture-1' },
    { open: 'account.fixture-1.weight' },
    { cancel: true },
    { cancel: true },
    { cancel: true },
    { cancel: true },
  ]],
})
pool('accounts summary counts disabled accounts', {
  provider: 'paster',
  methods: PASTER_METHODS,
  setup: [
    { op: 'add', provider: 'paster', input: { label: 'Paste One', credential: { type: 'api_key', key: 'sk-1' } } },
    { op: 'add', provider: 'paster', input: { label: 'Paste Two', enabled: false, credential: { type: 'api_key', key: 'sk-2' } } },
    { op: 'add', provider: 'paster', input: { label: 'Paste Three', credential: { type: 'api_key', key: 'sk-3' } } },
  ],
  steps: [[{ open: 'accounts' }, { cancel: true }, { cancel: true }]],
})
pool('stored upstream preferences are shown', {
  provider: 'alpha',
  methods: ALPHA_METHODS,
  setup: [...POOL_ALPHA, { op: 'setPool', provider: 'alpha', input: { upstream: { label: 'Main', weight: 3, priority: 2 } } }],
  steps: [[
    { open: 'accounts' },
    { open: 'upstream' },
    { cancel: true },
    { cancel: true },
    { cancel: true },
  ]],
})
pool('an upstream credential without a source label is shown', {
  provider: 'alpha',
  methods: ALPHA_METHODS,
  runtime: { providers: ALL, auth: { alpha: { configured: true } }, oauth: { alpha: true } },
  steps: [[{ open: 'accounts' }, { cancel: true }, { cancel: true }]],
})

// /vprovider
vp('vprovider requires interactive mode', { hasUI: false })
vp('vprovider requires tui mode', { mode: 'rpc' })
vp('creates a virtual provider with one backend', {
  steps: [[
    { change: 'create' },
    { submit: 'pooled' },
    { submit: 'pooled-model' },
    { change: 'add' },
    { change: 'alpha' },
    { change: 'alpha-1' },
    { change: 'save' },
  ]],
})
vp('creates a virtual provider with two backends and backend actions', {
  steps: [[
    { change: 'create' },
    { submit: 'pooled' },
    { submit: 'pooled-model' },
    { change: 'add' },
    { change: 'beta' },
    { change: 'beta-1' },
    { change: 'add' },
    { change: 'alpha' },
    { change: 'alpha-2' },
    { change: 'backend-1' },
    { change: 'toggle' },
    { change: 'backend-1' },
    { change: 'toggle' },
    { change: 'save' },
  ]],
})
vp('the backend weight row returns to the menu (upstream quirk)', {
  steps: [[
    { change: 'create' },
    { submit: 'pooled' },
    { submit: 'pooled-model' },
    { change: 'add' },
    { change: 'alpha' },
    { change: 'alpha-1' },
    { change: 'backend-0' },
    { change: 'weight' },
    { change: 'save' },
  ]],
})
vp('save without an enabled backend is rejected', {
  steps: [[
    { change: 'create' },
    { submit: 'pooled' },
    { submit: 'pooled-model' },
    { change: 'save' },
    { cancel: true },
  ]],
})
vp('invalid provider id is rejected and recovered', {
  steps: [[
    { change: 'create' },
    { submit: 'Pooled!' },
    { submit: 'pooled' },
    { submit: 'pooled-model' },
    { change: 'add' },
    { change: 'alpha' },
    { change: 'alpha-1' },
    { change: 'save' },
  ]],
})
vp('invalid model id is rejected and recovered', {
  steps: [[
    { change: 'create' },
    { submit: 'pooled' },
    { submit: 'bad model' },
    { submit: 'pooled-model' },
    { change: 'add' },
    { change: 'alpha' },
    { change: 'alpha-1' },
    { change: 'save' },
  ]],
})
vp('duplicate backend is rejected', {
  steps: [[
    { change: 'create' },
    { submit: 'pooled' },
    { submit: 'pooled-model' },
    { change: 'add' },
    { change: 'alpha' },
    { change: 'alpha-1' },
    { change: 'add' },
    { change: 'alpha' },
    { change: 'alpha-1' },
    { cancel: true },
    { change: 'save' },
  ]],
})
vp('removes a backend and saves', {
  steps: [[
    { change: 'create' },
    { submit: 'pooled' },
    { submit: 'pooled-model' },
    { change: 'add' },
    { change: 'alpha' },
    { change: 'alpha-1' },
    { change: 'add' },
    { change: 'beta' },
    { change: 'beta-1' },
    { change: 'backend-0' },
    { change: 'remove' },
    { change: 'save' },
  ]],
})
vp('edits a stored virtual provider by id', {
  setup: [{ op: 'saveVirtual', input: VIRTUAL }],
  args: 'vp',
  steps: [[
    { change: 'model-id' },
    { submit: 'model-2' },
    { change: 'save' },
  ]],
})
vp('stored provider reference is case insensitive', {
  setup: [{ op: 'saveVirtual', input: VIRTUAL }],
  args: 'VP',
  steps: [[{ change: 'discard' }]],
})
vp('unknown reference starts at the root', {
  setup: [{ op: 'saveVirtual', input: VIRTUAL }],
  args: 'nope',
  steps: [[{ change: 'edit-vp' }, { change: 'discard' }]],
})
vp('deletes a stored virtual provider', {
  setup: [{ op: 'saveVirtual', input: VIRTUAL }],
  answers: [CONFIRM(true)],
  steps: [[{ change: 'delete-vp' }]],
})
vp('delete declined at the confirmation', {
  setup: [{ op: 'saveVirtual', input: VIRTUAL }],
  answers: [CONFIRM(false)],
  steps: [[{ change: 'delete-vp' }]],
})
vp('dismisses the root dialog', {
  steps: [[{ cancel: true }]],
})
vp('cancels the create flow at the provider id input', {
  steps: [[{ change: 'create' }, { cancel: true }, { cancel: true }]],
})
vp('cancels the create flow from the editor menu', {
  steps: [[{ change: 'create' }, { submit: 'pooled' }, { submit: 'pooled-model' }, { cancel: true }]],
})
vp('picker cancel returns to the editor menu', {
  steps: [[
    { change: 'create' },
    { submit: 'pooled' },
    { submit: 'pooled-model' },
    { change: 'add' },
    { cancel: true },
    { change: 'add' },
    { change: 'delta' },
    { cancel: true },
    { change: 'add' },
    { change: 'alpha' },
    { change: 'alpha-1' },
    { change: 'save' },
  ]],
})
vp('heals a stored backend template on save', {
  setup: [{
    op: 'saveVirtual',
    input: { id: 'vp', label: 'Virtual Pool', models: [{ id: 'model', label: 'Model', backends: [{ providerId: 'alpha', modelId: 'alpha-1', weight: 1 }] }] },
  }],
  args: 'vp',
  steps: [[{ change: 'save' }]],
})
vp('save skips healing for a disabled backend', {
  setup: [{
    op: 'saveVirtual',
    input: {
      id: 'vp',
      label: 'Virtual Pool',
      models: [{ id: 'model', label: 'Model', backends: [
        { providerId: 'alpha', modelId: 'alpha-1', template: templateOf('alpha-1') },
        { providerId: 'alpha', modelId: 'alpha-2', enabled: false, weight: 1 },
      ] }],
    },
  }],
  args: 'vp',
  steps: [[{ change: 'save' }]],
})
vp('an empty candidate list shows no backing providers', {
  providers: ['zeta'],
  steps: [[
    { change: 'create' },
    { submit: 'vp2' },
    { submit: 'vp2-model' },
    { change: 'add' },
    { cancel: true },
    { cancel: true },
  ]],
})
vp('editing only touches the first model', {
  setup: [{
    op: 'saveVirtual',
    input: {
      id: 'vp',
      label: 'Virtual Pool',
      models: [
        { id: 'model', label: 'Model', backends: [{ providerId: 'alpha', modelId: 'alpha-1', template: templateOf('alpha-1') }] },
        { id: 'other', label: 'Other', backends: [{ providerId: 'beta', modelId: 'beta-1', template: templateOf('beta-1') }] },
      ],
    },
  }],
  args: 'vp',
  steps: [[{ change: 'save' }]],
})
vp('excludes providers without models and virtual providers', {
  setup: [{ op: 'saveVirtual', input: VIRTUAL }],
  steps: [[
    { change: 'create' },
    { submit: 'vp2' },
    { submit: 'vp2-model' },
    { change: 'add' },
    { cancel: true },
    { cancel: true },
  ]],
})

// Run ------------------------------------------------------------------------

const output = {
  reference: { upstream: 'bd97d65a36f541935dc91e33ba15d7910a30f9d7', pi: piVersion, node: process.version },
  pinned: PINNED,
  world: WORLD,
  // The host model objects the editor captures templates from.
  catalog: Object.fromEntries(ALL.map((id) => [id, WORLD[id].models.map((model) => modelOf(id, model))])),
  scenarios: [],
}
for (const scenario of scenarios) {
  sequence = 0
  now = 1700000000000
  let result
  try {
    if (scenario.kind === 'pool-screen') result = await runPoolScreen(scenario)
    else if (scenario.kind === 'vprovider') result = await runVproviderCommand(scenario)
    else if (scenario.kind === 'format-ms') result = runFormatMs(scenario)
    else throw new Error(`unknown scenario kind ${scenario.kind}`)
  } catch (error) {
    throw new Error(`${scenario.name}: ${error.message}`, { cause: error })
  }
  output.scenarios.push({ name: scenario.name, kind: scenario.kind, input: result.input, expected: result.expected })
}

await rm(baseDir, { recursive: true, force: true })
const text = `${JSON.stringify(output, null, 2)}\n`
if (write) {
  await writeFile(fixturePath, text)
  process.stdout.write(`wrote ${output.scenarios.length} scenarios\n`)
} else {
  const committed = await readFile(fixturePath, 'utf8')
  if (committed !== text) {
    process.stderr.write('commands-a2-reference.json differs from the regenerated reference\n')
    process.exit(1)
  }
  process.stdout.write(`ok ${output.scenarios.length} scenarios byte-identical\n`)
}
