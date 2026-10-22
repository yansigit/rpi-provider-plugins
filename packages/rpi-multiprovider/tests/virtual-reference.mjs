// Differential reference for upstream src/virtual.ts (multiprovider @bd97d65).
// Usage (Node 24.22.0 only):
//   node --experimental-transform-types tests/virtual-reference.mjs [--check | --write] [upstream-root]
// Without a flag the JSON document is printed. --check regenerates it and fails on any byte difference
// from tests/virtual-reference.json; --write replaces that fixture.
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { syncBuiltinESMExports } from 'node:module'
import { readFile, writeFile } from 'node:fs/promises'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'

const REFERENCE_COMMIT = 'bd97d65a36f541935dc91e33ba15d7910a30f9d7'
const here = dirname(fileURLToPath(import.meta.url))
const fixturePath = join(here, 'virtual-reference.json')
const args = process.argv.slice(2)
const mode = args.includes('--write') ? 'write' : args.includes('--check') ? 'check' : 'print'
const positional = args.filter(arg => !arg.startsWith('--'))
const root = resolve(positional[0] ?? join(here, '../../../../provider-references/multiprovider'))
assert.match(process.versions.node, /^24\./, 'the reference runtime is Node 24')

const pinnedHashes = {
  'src/virtual.ts': '6c38efe1d8bcd180e3287b182df592914a1af0a7091f872cd6dc32042575f808',
  'src/lift.ts': '887b91aab533565504449b6094c1a2b076832b0fdc73bfc352e2c46a53ac3695',
  'src/service.ts': '2eafa61662b3ee75a96a7b07a6ccad62ca600bbb2139a407b81cd9983564a026',
  'src/types.ts': 'aaf5e77ffc5738bfc5ee642a61238793ec0869123ceef12a9650c1384b0d181e',
  'src/errors.ts': '1e4f189fc4aac4488c84912b29bfc7e78629600ead41ce2924c8d8c3a3bac4b7',
  'node_modules/@earendil-works/pi-ai/dist/api/lazy.js': 'f3e5dece7b31d0a223d96994f062edcf82fbe91039accaf40c0553abbe2ee359',
  'node_modules/@earendil-works/pi-ai/dist/utils/event-stream.js': 'fe0d774b75db25ff0149d4267884bc4fd34db82d53c26843758d0eda2467ce65',
}
const hashes = {}
for (const [file, expected] of Object.entries(pinnedHashes)) {
  const actual = createHash('sha256').update(await readFile(join(root, file))).digest('hex')
  assert.equal(actual, expected, `upstream file changed: ${file}`)
  hashes[file] = actual
}

// Virtual clock: Date, performance.now, and the 250 ms same-account pause are all driven by `now`.
let now = 1700000000000
const DateOriginal = Date
class Clock extends DateOriginal {
  constructor(...args) { super(...(args.length ? args : [now])) }
  static now() { return now }
}
globalThis.Date = Clock
Object.defineProperty(globalThis.performance, 'now', { value: () => now - 1700000000000 })
let pauseHook = () => { throw new Error('pause outside a case') }
const originalSetTimeout = globalThis.setTimeout
globalThis.setTimeout = (fn, ms, ...rest) => {
  if (ms !== 250) return originalSetTimeout(fn, ms, ...rest)
  pauseHook()
  queueMicrotask(() => fn(...rest))
  return 0
}
syncBuiltinESMExports()

const { createVirtualIntegrations, createVirtualProvider, captureVirtualModelTemplate, healVirtualTemplates, virtualSchedulerId } =
  await import(pathToFileURL(join(root, 'src/virtual.ts')))
const { MultiProviderService } = await import(pathToFileURL(join(root, 'src/service.ts')))

// ---- scripted data helpers -------------------------------------------------------------------
const SEP = '::'
const usage = () => ({ input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } })
const assistant = (stopReason, fields = {}) => ({
  role: 'assistant',
  content: fields.text === undefined ? [] : [{ type: 'text', text: fields.text }],
  api: 'test-api',
  provider: fields.provider ?? 'prov-a',
  model: fields.model ?? 'model-a',
  usage: usage(),
  stopReason,
  ...(fields.errorMessage === undefined ? {} : { errorMessage: fields.errorMessage }),
  timestamp: 0,
})
const zeroCost = { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 }
const modelA = { id: 'model-a', name: 'Model A', api: 'test-api', provider: 'prov-a', baseUrl: 'https://a.invalid', reasoning: false, thinkingLevelMap: { high: 'high-effort', off: null }, input: ['text'], cost: zeroCost, contextWindow: 1000, maxTokens: 100 }
const modelB = { ...modelA, id: 'model-b', name: 'Model B', provider: 'prov-b', baseUrl: 'https://b.invalid' }
const modelC = { ...modelA, id: 'model-c', name: 'Model C', provider: 'prov-c', baseUrl: 'https://c.invalid' }
const std = { 'prov-a': [modelA], 'prov-b': [modelB] }
const std3 = { 'prov-a': [modelA], 'prov-b': [modelB], 'prov-c': [modelC] }
const twoBackends = [{ providerId: 'prov-a', modelId: 'model-a' }, { providerId: 'prov-b', modelId: 'model-b' }]
const threeBackends = [...twoBackends, { providerId: 'prov-c', modelId: 'model-c' }]
const pooled = (backends, extra = {}) => ({ id: 'pooled', label: 'Pooled', models: [{ id: 'ultra', backends }], ...extra })
const template = (model, overrides = {}) => ({ ...captureVirtualModelTemplate(model), ...overrides })
const ok = text => {
  const done = assistant('stop', { text })
  return { events: [
    { type: 'text_start', contentIndex: 0, partial: done },
    { type: 'text_delta', contentIndex: 0, delta: text, partial: done },
    { type: 'text_end', contentIndex: 0, content: text, partial: done },
    { type: 'done', reason: 'stop', message: done },
  ] }
}
const fail = errorMessage => ({ events: [{ type: 'error', reason: 'error', error: assistant('error', { errorMessage }) }] })
const started = tag => ({ type: 'start', partial: assistant('stop', { text: tag }) })
const textDelta = text => ({ type: 'text_delta', contentIndex: 0, delta: text, partial: assistant('stop', { text }) })
const withResponse = (script, status, headers = {}) => ({ ...script, response: { status, headers } })
const abortedError = () => ({ type: 'error', reason: 'aborted', error: assistant('aborted', { errorMessage: 'cancelled' }) })

// ---- one case: a fresh service, virtual provider and fake backing registry -------------------
let trace = []
const canonicalFailure = failure => ({ ...failure, ...(failure.cause === undefined ? {} : { cause: { name: failure.cause.name, message: failure.cause.message } }) })
const record = entry => { trace.push(structuredClone(entry)) }
const withNow = value => {
  if (Array.isArray(value)) return value.map(withNow)
  if (value !== null && typeof value === 'object') {
    const out = {}
    for (const [key, item] of Object.entries(value)) out[key] = key === 'timestamp' && item === '$NOW' ? now : withNow(item)
    return out
  }
  return value
}

async function runCase(input) {
  now = 1700000000000
  trace = []
  const backing = new Map(Object.entries(input.backing ?? {}))
  const scripts = new Map(Object.entries(input.scripts ?? {}).map(([provider, queue]) => [provider, { queue, next: 0 }]))
  const world = { affinity: 'session-1', activeController: undefined }
  const pendingSlots = new Set()
  const service = new MultiProviderService({
    now: () => now,
    randomId: () => 'lease',
    randomInt: max => { record({ randomInt: max }); return 0 },
    ...(input.service?.errorsBeforeSwitch === undefined ? {} : { errorsBeforeSwitch: input.service.errorsBeforeSwitch }),
    ...(input.service?.affinity === undefined ? {} : { affinity: input.service.affinity }),
  })
  pauseHook = () => {
    record({ pause: 250 })
    now += 250
    if (input.abortOnPause) world.activeController.abort()
  }

  function send(providerId, kind, model, context, options) {
    const { signal, onResponse, ...rest } = options
    record({ send: { provider: providerId, kind, model, context, options: rest } })
    const state = scripts.get(providerId)
    const index = state?.next ?? 0
    if (state) state.next = index + 1
    if (state === undefined || index >= state.queue.length) throw new Error(`fixture script exhausted: ${providerId}`)
    const script = state.queue[index]
    if (script.abort) world.activeController.abort()
    if (script.throw) throw new Error(script.throw)
    return (async function* () {
      if (script.response) {
        record({ onResponse: { status: script.response.status, headers: script.response.headers, model } })
        await onResponse?.(structuredClone(script.response), model)
      }
      for (const event of script.events ?? []) {
        if (event.throw) throw new Error(event.throw)
        yield withNow(structuredClone(event))
      }
    })()
  }
  const backingProvider = providerId => ({
    id: providerId,
    name: providerId,
    auth: {},
    getModels: () => backing.get(providerId) ?? [],
    stream: (model, context, options) => send(providerId, 'stream', model, context, options),
    streamSimple: (model, context, options) => send(providerId, 'streamSimple', model, context, options),
  })

  const integrationOptions = {
    ...(input.integrationOptions?.labels === undefined ? {} : { getProviderLabel: providerId => input.integrationOptions.labels[providerId] }),
    ...(input.integrationOptions?.maxAccountAttempts === undefined ? {} : { maxAccountAttempts: input.integrationOptions.maxAccountAttempts }),
  }
  const integrations = createVirtualIntegrations(input.config, integrationOptions)
  for (const integration of integrations) {
    const classify = integration.classifyFailure
    integration.classifyFailure = (failure, account) => {
      const result = classify(failure, account)
      record({ classify: { failure: canonicalFailure(failure), account: account.id }, result: result ?? null })
      return result
    }
  }

  const virtual = createVirtualProvider({
    service,
    config: input.config,
    getAffinityKey: () => {
      record({ affinity: world.affinity })
      return world.affinity
    },
    getBackingProvider: providerId => {
      const found = backing.has(providerId)
      record({ lookup: providerId, found })
      return found ? backingProvider(providerId) : undefined
    },
    resolveAmbientAuth: async (providerId, model) => {
      record({ ambient: { providerId, model: model.id } })
      const scripted = input.ambient?.[providerId]
      if (scripted?.throw) throw new Error(scripted.throw)
      return structuredClone(scripted ?? { ok: true, apiKey: `ambient-${providerId}` })
    },
    isBackendConfigured: providerId => {
      const configured = input.isBackendConfigured?.[providerId] ?? true
      record({ configured: providerId, result: configured })
      return configured
    },
    ...(input.providerOptions?.maxAccountAttempts === undefined ? {} : { maxAccountAttempts: input.providerOptions.maxAccountAttempts }),
    ...(input.onFailover === undefined ? {} : {
      onFailover: info => {
        record({ failover: { ...info, failure: canonicalFailure(info.failure) } })
        if (input.onFailover === 'throw') throw new Error('fixture failover failed')
        return input.onFailover === 'surface' ? true : input.onFailover === 'void' ? undefined : false
      },
    }),
  })

  const schedulerId = virtualSchedulerId(input.config.id, 'ultra')
  const slots = new Map()
  const outputs = []
  const templates = input.templates ?? {}
  for (const [index, op] of input.ops.entries()) {
    record({ begin: index })
    const asyncOp = ['snapshot', 'auth', 'integrations', 'pin', 'drain', 'stream'].includes(op.op)
    if (asyncOp && op.op !== 'drain' && op.op !== 'stream') assert.equal(pendingSlots.size, 0, `async op ${op.op} while a stream is pending`)
    let output = null
    switch (op.op) {
      case 'register': {
        const errors = []
        for (const integration of integrations) {
          try { service.registerProvider(integration) } catch (error) { errors.push(error.message) }
        }
        output = { errors }
        break
      }
      case 'snapshot': output = await service.snapshot(); break
      case 'models': output = virtual.getModels(); break
      case 'auth': {
        const resolution = await virtual.auth.apiKey.resolve({ ctx: {}, signal: new AbortController().signal })
        output = resolution ?? null
        break
      }
      case 'integrations': {
        output = []
        for (const integration of integrations) {
          output.push({ id: integration.id, label: integration.label, selectionBias: integration.selectionBias, maxAccountAttempts: integration.maxAccountAttempts ?? null, accounts: await integration.accounts() })
        }
        break
      }
      case 'capture': {
        const model = (backing.get(op.provider) ?? []).find(item => item.id === op.model)
        output = captureVirtualModelTemplate(model)
        break
      }
      case 'heal': {
        const opTemplates = op.templates ?? templates
        const healed = healVirtualTemplates(input.config, (providerId, modelId) => {
          record({ resolveTemplate: { providerId, modelId } })
          return structuredClone(opTemplates[`${providerId}${SEP}${modelId}`])
        })
        output = healed ?? null
        break
      }
      case 'setAffinity': world.affinity = op.key; break
      case 'setSettings': service.updateSchedulerDefaults(op.settings); break
      case 'setBacking':
        if (op.models === null) backing.delete(op.provider)
        else backing.set(op.provider, op.models)
        break
      case 'advance': now += op.ms; break
      case 'abort': slots.get(op.slot).controller?.abort(); break
      case 'affinity': output = service.getAffinity(schedulerId, op.key) ?? null; break
      case 'pin':
        try { await service.pinAccount(schedulerId, op.key, op.account); output = {} } catch (error) { output = { error: error.message } }
        break
      case 'classify':
        output = integrations[op.index ?? 0].classifyFailure(structuredClone(op.failure), { id: 'classify-probe' }) ?? null
        break
      case 'open': {
        const model = virtual.getModels().find(item => item.id === op.model)
        if (op.affinityKey !== undefined) world.affinity = op.affinityKey
        const options = { ...(op.options ?? {}) }
        const controller = op.signal ? new AbortController() : undefined
        if (controller) options.signal = controller.signal
        world.activeController = controller
        if (op.onResponse) {
          options.onResponse = async (response, responseModel) => {
            record({ userOnResponse: { status: response.status, headers: response.headers, model: responseModel } })
          }
        }
        const context = structuredClone(op.context ?? { messages: [] })
        const stream = op.kind === 'streamSimple'
          ? virtual.streamSimple(model, context, options)
          : virtual.stream(model, context, options)
        slots.set(op.slot, { stream, controller })
        pendingSlots.add(op.slot)
        break
      }
      case 'drain':
      case 'stream': {
        if (op.op === 'stream') {
          // stream = open + drain with no intervening async work
          const model = virtual.getModels().find(item => item.id === op.model)
          if (op.affinityKey !== undefined) world.affinity = op.affinityKey
          const options = { ...(op.options ?? {}) }
          const controller = op.signal ? new AbortController() : undefined
          if (controller) options.signal = controller.signal
          world.activeController = controller
          if (op.onResponse) options.onResponse = async (response, responseModel) => { record({ userOnResponse: { status: response.status, headers: response.headers, model: responseModel } }) }
          const context = structuredClone(op.context ?? { messages: [] })
          const stream = op.kind === 'streamSimple' ? virtual.streamSimple(model, context, options) : virtual.stream(model, context, options)
          slots.set(op.slot, { stream, controller })
          pendingSlots.add(op.slot)
        }
        const slot = slots.get(op.slot)
        const events = []
        pendingSlots.delete(op.slot)
        for await (const event of slot.stream) events.push(structuredClone(event))
        output = { events }
        break
      }
      default: throw new Error(`unknown op ${op.op}`)
    }
    outputs.push(output)
  }
  return { ops: outputs, trace, now }
}

// ---- spec expectations (upstream tests/virtual.spec.ts) ---------------------------------------
function expectSubset(actual, expected, path = 'value') {
  if (expected !== null && typeof expected === 'object' && !Array.isArray(expected)) {
    assert.ok(actual !== null && typeof actual === 'object', `${path} should be an object`)
    for (const [key, value] of Object.entries(expected)) expectSubset(actual[key], value, `${path}.${key}`)
    return
  }
  assert.deepEqual(actual, expected, path)
}
const sends = result => result.trace.filter(entry => entry.send).map(entry => entry.send)
const lastType = events => events.at(-1)?.type
const debugEvents = result => JSON.stringify(result.ops.map(output => output?.events ?? output))
const specChecks = {
  'registers one unbiased scheduler per virtual model': result => {
    const [snapshot] = result.ops.slice(-1)
    assert.equal(snapshot.providers.length, 1)
    assert.equal(snapshot.providers[0].id, 'pooled::ultra')
    assert.equal(snapshot.providers[0].firstAccountBias, false)
    assert.deepEqual(snapshot.providers[0].accounts.map(account => account.id), ['prov-a::model-a', 'prov-b::model-b'])
  },
  'exposes virtual models templated from the first healthy backend': result => {
    assert.equal(result.ops[0].length, 1)
    expectSubset(result.ops[0][0], { id: 'ultra', name: 'ultra', provider: 'pooled', api: 'test-api', baseUrl: 'https://a.invalid', thinkingLevelMap: { high: 'high-effort', off: null }, contextWindow: 1000 })
  },
  'falls back to the persisted backend template before backing providers register': result => {
    expectSubset(result.ops[0][0], { id: 'ultra', provider: 'pooled', api: 'test-api', baseUrl: 'https://a.invalid', reasoning: true, thinkingLevelMap: { high: 'high-effort', off: null }, contextWindow: 1000, maxTokens: 100 })
  },
  'prefers the live backing model over the persisted template': result => {
    expectSubset(result.ops[0][0], { api: 'test-api', baseUrl: 'https://a.invalid', reasoning: false, thinkingLevelMap: { high: 'high-effort', off: null }, contextWindow: 1000 })
  },
  'ignores persisted templates on disabled backends': result => {
    assert.equal(result.ops[0][0].reasoning, false)
    assert.equal('thinkingLevelMap' in result.ops[0][0], false)
  },
  'heals stored configs by filling missing templates from live backings': result => {
    assert.deepEqual(result.ops[0].models[0].backends[0].template, captureVirtualModelTemplate(modelA))
    assert.equal(result.ops[0].models[0].backends[1].template, undefined)
    assert.equal(result.ops[1], null)
  },
  'round-robins backends across sessions and delegates with backing model and ambient auth': result => {
    assert.equal(lastType(result.ops[0].events), 'done', debugEvents(result))
    assert.equal(lastType(result.ops[1].events), 'done', debugEvents(result))
    assert.deepEqual(sends(result).map(send => send.model.id), ['model-a', 'model-b'])
    assert.deepEqual(sends(result).map(send => send.options.apiKey), ['ambient-prov-a', 'ambient-prov-b'])
    assert.ok(sends(result).every(send => send.model.baseUrl.startsWith('https://')))
  },
  'pins the session affinity key to the last healthy backend': result => {
    assert.deepEqual(sends(result).map(send => send.model.id), ['model-a', 'model-a'])
  },
  'fails over to the next backend before output': result => {
    assert.equal(lastType(result.ops[0].events), 'done')
    assert.deepEqual(sends(result).map(send => send.model.id), ['model-a', 'model-b'])
  },
  'fails over when a backend provider or model is unavailable': result => {
    assert.equal(lastType(result.ops[0].events), 'done')
    assert.deepEqual(sends(result).map(send => send.model.id), ['model-b'])
  },
  'replays the terminal error once every backend is exhausted': result => {
    assert.equal(lastType(result.ops[0].events), 'error')
    assert.equal(sends(result).length, 2)
  },
  'absorbs up to three backend errors before failing over': result => {
    assert.equal(lastType(result.ops[0].events), 'done')
    assert.deepEqual(sends(result).map(send => send.model.id), ['model-a', 'model-a', 'model-a', 'model-b'])
  },
  'surfaces the buffered error when onFailover claims the transition': result => {
    assert.equal(lastType(result.ops[0].events), 'error')
    assert.deepEqual(sends(result).map(send => send.model.id), ['model-a', 'model-a', 'model-a'])
    const hooks = result.trace.filter(entry => entry.failover).map(entry => entry.failover)
    assert.equal(hooks.length, 1)
    expectSubset(hooks[0], { providerId: 'pooled::ultra', fromAccountId: 'prov-a::model-a', errorsOnAccount: 3, failure: { message: 'HTTP 500', outputStarted: false } })
  },
  'classifies status-less transport failures as transient backend failures': result => {
    const transport = ['Connection error.', 'fetch failed', 'Request timed out.', 'terminated', 'other side closed', 'getaddrinfo ENOTFOUND api.example.invalid', 'connect ECONNREFUSED 127.0.0.1:443', 'upstream connect error or disconnect/reset before headers', 'UND_ERR_SOCKET']
    const kinds = result.ops.slice(0, transport.length)
    kinds.forEach((output, index) => assert.deepEqual(output, { kind: 'transient', retryable: true }, transport[index]))
    assert.equal(result.ops[transport.length], null)
    assert.equal(result.ops[transport.length + 1], null)
  },
  'fails over to the next backend after connection errors without an HTTP status': result => {
    assert.equal(lastType(result.ops[0].events), 'done')
    assert.deepEqual(sends(result).map(send => send.model.id), ['model-a', 'model-a', 'model-a', 'model-b'])
  },
  'lands the host retry on the next backend after a surfaced failover': result => {
    assert.equal(lastType(result.ops[0].events), 'error')
    assert.equal(lastType(result.ops[2].events), 'done')
    assert.deepEqual(sends(result).map(send => send.model.id), ['model-a', 'model-a', 'model-a', 'model-b'])
  },
  'reports unconfigured auth when no backend provider is configured': result => {
    assert.equal(result.ops[0], null)
  },
  'reports placeholder auth once a backend is configured': result => {
    expectSubset(result.ops[0], { auth: { apiKey: 'virtual-provider' }, source: 'virtual provider' })
  },
}

// ---- case definitions ---------------------------------------------------------------------------
const pooledFor = (name, backends, extra) => ({ name, config: pooled(backends), ...extra })
const cases = []
const spec = (name, input) => cases.push({ name: `spec: ${name}`, spec: name, input })
const generated = (name, input) => cases.push({ name: `generated: ${name}`, spec: null, input })
const reg = [{ op: 'register' }]
const ops = (...items) => items

spec('registers one unbiased scheduler per virtual model', { config: pooled(twoBackends), ops: [...reg, { op: 'snapshot' }] })
spec('exposes virtual models templated from the first healthy backend', { config: pooled(twoBackends), backing: std, ops: [{ op: 'models' }] })
spec('falls back to the persisted backend template before backing providers register', {
  config: pooled([{ providerId: 'prov-a', modelId: 'model-a', template: template(modelA, { reasoning: true }) }]),
  ops: [{ op: 'models' }],
})
spec('prefers the live backing model over the persisted template', {
  config: pooled([{ providerId: 'prov-a', modelId: 'model-a', template: template(modelA, { api: 'openai-completions', baseUrl: 'https://stored.invalid', reasoning: true, contextWindow: 5000 }) }]),
  backing: { 'prov-a': [modelA] },
  ops: [{ op: 'models' }],
})
spec('ignores persisted templates on disabled backends', {
  config: pooled([{ providerId: 'prov-a', modelId: 'model-a', enabled: false, template: template(modelA, { reasoning: true }) }, { providerId: 'prov-b', modelId: 'model-b' }]),
  ops: [{ op: 'models' }],
})
spec('heals stored configs by filling missing templates from live backings', {
  config: pooled(twoBackends),
  templates: { [`prov-a${SEP}model-a`]: captureVirtualModelTemplate(modelA) },
  ops: [{ op: 'heal' }, { op: 'heal', templates: {} }],
})
spec('round-robins backends across sessions and delegates with backing model and ambient auth', {
  config: pooled(twoBackends), backing: std, scripts: { 'prov-a': [ok('from-a')], 'prov-b': [ok('from-b')] },
  ops: [{ op: 'stream', slot: 's1', model: 'ultra', affinityKey: 'session-1' }, { op: 'stream', slot: 's2', model: 'ultra', affinityKey: 'session-2' }],
})
spec('pins the session affinity key to the last healthy backend', {
  config: pooled(twoBackends), backing: std, scripts: { 'prov-a': [ok('from-a'), ok('from-a')], 'prov-b': [ok('from-b')] },
  ops: [{ op: 'stream', slot: 's1', model: 'ultra', affinityKey: 'session-1' }, { op: 'stream', slot: 's2', model: 'ultra', affinityKey: 'session-1' }],
})
spec('fails over to the next backend before output', {
  config: pooled(twoBackends), backing: std, service: { errorsBeforeSwitch: 1 }, scripts: { 'prov-a': [fail('HTTP 500 upstream')], 'prov-b': [ok('from-b')] },
  ops: [{ op: 'stream', slot: 's1', model: 'ultra' }],
})
spec('fails over when a backend provider or model is unavailable', {
  config: pooled(twoBackends), backing: { 'prov-b': [modelB] }, scripts: { 'prov-b': [ok('from-b')] },
  ops: [{ op: 'stream', slot: 's1', model: 'ultra' }],
})
spec('replays the terminal error once every backend is exhausted', {
  config: pooled(twoBackends), backing: std, service: { errorsBeforeSwitch: 1 }, scripts: { 'prov-a': [fail('HTTP 500')], 'prov-b': [fail('HTTP 503')] },
  ops: [{ op: 'stream', slot: 's1', model: 'ultra' }],
})
spec('absorbs up to three backend errors before failing over', {
  config: pooled(twoBackends), backing: std, scripts: { 'prov-a': [fail('HTTP 503'), fail('HTTP 503'), fail('HTTP 503')], 'prov-b': [ok('from-b')] },
  ops: [{ op: 'stream', slot: 's1', model: 'ultra' }],
})
spec('surfaces the buffered error when onFailover claims the transition', {
  config: pooled(twoBackends), backing: std, onFailover: 'surface', scripts: { 'prov-a': [fail('HTTP 500'), fail('HTTP 500'), fail('HTTP 500')], 'prov-b': [fail('HTTP 503')] },
  ops: [{ op: 'stream', slot: 's1', model: 'ultra' }],
})
spec('classifies status-less transport failures as transient backend failures', {
  config: pooled(twoBackends),
  ops: [
    ...['Connection error.', 'fetch failed', 'Request timed out.', 'terminated', 'other side closed', 'getaddrinfo ENOTFOUND api.example.invalid', 'connect ECONNREFUSED 127.0.0.1:443', 'upstream connect error or disconnect/reset before headers', 'UND_ERR_SOCKET'].map(message => ({ op: 'classify', failure: { message, outputStarted: false } })),
    { op: 'classify', failure: { message: 'HTTP 400 invalid request', outputStarted: false } },
    { op: 'classify', failure: { message: '429 rate limit exceeded', outputStarted: false } },
  ],
})
spec('fails over to the next backend after connection errors without an HTTP status', {
  config: pooled(twoBackends), backing: std, scripts: { 'prov-a': [fail('Connection error.'), fail('Connection error.'), fail('Connection error.')], 'prov-b': [ok('from-b')] },
  ops: [{ op: 'stream', slot: 's1', model: 'ultra' }],
})
spec('lands the host retry on the next backend after a surfaced failover', {
  config: pooled(twoBackends), backing: std, onFailover: 'surface', scripts: { 'prov-a': [fail('Connection error.'), fail('Connection error.'), fail('Connection error.')], 'prov-b': [ok('from-b')] },
  ops: [{ op: 'stream', slot: 's1', model: 'ultra' }, { op: 'advance', ms: 60000 }, { op: 'stream', slot: 's2', model: 'ultra' }],
})
spec('reports unconfigured auth when no backend provider is configured', {
  config: pooled(twoBackends), isBackendConfigured: { 'prov-a': false, 'prov-b': false }, ops: [{ op: 'auth' }],
})
spec('reports placeholder auth once a backend is configured', {
  config: pooled(twoBackends), ops: [{ op: 'auth' }],
})

// generated edge cases
generated('all backends failing exhausts tolerance and replays the last terminal', {
  config: pooled(threeBackends), backing: std3, scripts: { 'prov-a': [fail('HTTP 500'), fail('HTTP 500'), fail('HTTP 500')], 'prov-b': [fail('HTTP 503'), fail('HTTP 503'), fail('HTTP 503')], 'prov-c': [fail('HTTP 502'), fail('HTTP 502'), fail('HTTP 502')] },
  ops: [{ op: 'stream', slot: 's1', model: 'ultra' }, { op: 'snapshot' }],
})
generated('all backends failing with a single-error tolerance and cooldown snapshot', {
  config: pooled(threeBackends), backing: std3, service: { errorsBeforeSwitch: 1 }, scripts: { 'prov-a': [fail('HTTP 500')], 'prov-b': [fail('HTTP 503')], 'prov-c': [fail('HTTP 429 slow')] },
  ops: [{ op: 'stream', slot: 's1', model: 'ultra' }, { op: 'snapshot' }],
})
generated('mid-stream failure after output is not failed over', {
  config: pooled(twoBackends), backing: std,
  scripts: { 'prov-a': [{ events: [started('a'), textDelta('partial'), { type: 'error', reason: 'error', error: assistant('error', { text: 'partial', errorMessage: 'stream broke' }) }] }], 'prov-b': [ok('from-b')] },
  ops: [{ op: 'stream', slot: 's1', model: 'ultra' }],
})
generated('iterator throw after output keeps the failed backend and surfaces the error', {
  config: pooled(twoBackends), backing: std,
  scripts: { 'prov-a': [{ events: [started('a'), textDelta('partial'), { throw: 'socket hang up' }] }], 'prov-b': [ok('from-b')] },
  ops: [{ op: 'stream', slot: 's1', model: 'ultra' }],
})
generated('stream ending without a terminal before output is fatal', {
  config: pooled(twoBackends), backing: std, scripts: { 'prov-a': [{ events: [] }], 'prov-b': [ok('from-b')] },
  ops: [{ op: 'stream', slot: 's1', model: 'ultra' }],
})
generated('stream ending without a terminal after output is surfaced', {
  config: pooled(twoBackends), backing: std, scripts: { 'prov-a': [{ events: [started('a'), textDelta('partial')] }], 'prov-b': [ok('from-b')] },
  ops: [{ op: 'stream', slot: 's1', model: 'ultra' }],
})
generated('weighted backends bias selection across sessions', {
  config: pooled([{ providerId: 'prov-a', modelId: 'model-a', weight: 3 }, { providerId: 'prov-b', modelId: 'model-b', weight: 1 }]),
  backing: std,
  scripts: { 'prov-a': [ok('a'), ok('a'), ok('a'), ok('a')], 'prov-b': [ok('b'), ok('b'), ok('b'), ok('b')] },
  ops: ['session-1', 'session-2', 'session-3', 'session-4'].map((key, index) => ({ op: 'stream', slot: `s${index}`, model: 'ultra', affinityKey: key })),
})
generated('zero and null weights normalize to scheduler defaults', {
  config: pooled([{ providerId: 'prov-a', modelId: 'model-a', weight: 0 }, { providerId: 'prov-b', modelId: 'model-b', weight: null }]),
  backing: std,
  scripts: { 'prov-a': [ok('a'), ok('a')], 'prov-b': [ok('b'), ok('b')] },
  ops: [{ op: 'integrations' }, { op: 'register' }, { op: 'snapshot' }, { op: 'stream', slot: 's1', model: 'ultra', affinityKey: 'session-1' }, { op: 'stream', slot: 's2', model: 'ultra', affinityKey: 'session-2' }],
})
generated('disabled backends are excluded from accounts, templates and selection', {
  config: pooled([{ providerId: 'prov-a', modelId: 'model-a', enabled: false }, { providerId: 'prov-b', modelId: 'model-b' }]),
  backing: std, scripts: { 'prov-b': [ok('b')] },
  ops: [...reg, { op: 'snapshot' }, { op: 'models' }, { op: 'auth' }, { op: 'stream', slot: 's1', model: 'ultra' }],
})
generated('all backends disabled yield no-enabled-accounts and no auth', {
  config: pooled([{ providerId: 'prov-a', modelId: 'model-a', enabled: false }, { providerId: 'prov-b', modelId: 'model-b', enabled: false }]),
  backing: std,
  ops: [...reg, { op: 'models' }, { op: 'auth' }, { op: 'stream', slot: 's1', model: 'ultra' }],
})
generated('template heals when a backing model disappears and reappears', {
  config: pooled([{ providerId: 'prov-a', modelId: 'model-a', template: template(modelA, { reasoning: true }) }, { providerId: 'prov-b', modelId: 'model-b' }]),
  backing: std,
  scripts: { 'prov-b': [ok('from-b')], 'prov-a': [ok('from-a')] },
  ops: [
    { op: 'models' },
    { op: 'setBacking', provider: 'prov-a', models: null },
    { op: 'models' },
    { op: 'stream', slot: 's1', model: 'ultra', affinityKey: 'session-1' },
    { op: 'setBacking', provider: 'prov-a', models: [modelA] },
    { op: 'advance', ms: 60000 },
    { op: 'models' },
    { op: 'stream', slot: 's2', model: 'ultra', affinityKey: 'session-2' },
  ],
})
generated('template fallback without any persisted template uses upstream defaults', {
  config: pooled([{ providerId: 'prov-a', modelId: 'model-a' }]),
  ops: [{ op: 'models' }],
})
generated('malformed config: duplicate backends fail on acquisition', {
  config: pooled([{ providerId: 'prov-a', modelId: 'model-a' }, { providerId: 'prov-a', modelId: 'model-a' }]),
  backing: std, scripts: { 'prov-a': [ok('a')] },
  ops: [...reg, { op: 'models' }, { op: 'auth' }, { op: 'stream', slot: 's1', model: 'ultra' }],
})
generated('malformed config: duplicate virtual model ids are rejected at registration', {
  config: { id: 'pooled', label: 'Pooled', models: [{ id: 'ultra', backends: [{ providerId: 'prov-a', modelId: 'model-a' }] }, { id: 'ultra', backends: [{ providerId: 'prov-b', modelId: 'model-b' }] }] },
  backing: std, ops: [...reg, { op: 'snapshot' }, { op: 'models' }],
})
generated('malformed config: empty model list has no providers or auth', {
  config: { id: 'pooled', label: 'Pooled', models: [] },
  ops: [...reg, { op: 'snapshot' }, { op: 'models' }, { op: 'auth' }, { op: 'integrations' }],
})
generated('malformed config: null optional fields fall back like nullish operators', {
  config: { id: 'pooled', label: 'Pooled', models: [
    { id: 'ultra', label: null, backends: [{ providerId: 'prov-a', modelId: 'model-a', enabled: null, weight: null }] },
    { id: 'empty', label: '', backends: [{ providerId: 'prov-b', modelId: 'model-b', enabled: true, weight: 2 }] },
  ] },
  backing: std, integrationOptions: { labels: { 'prov-a': 'Alpha' } },
  ops: [{ op: 'integrations' }, { op: 'register' }, { op: 'snapshot' }, { op: 'models' }],
})
generated('malformed config: null template is present and blocks healing', {
  config: pooled([{ providerId: 'prov-a', modelId: 'model-a', template: null }, { providerId: 'prov-b', modelId: 'model-b' }]),
  ops: [{ op: 'models' }, { op: 'heal', templates: { [`prov-b${SEP}model-b`]: captureVirtualModelTemplate(modelB) } }, { op: 'heal', templates: {} }],
})
generated('malformed config: nullish labels preserve provider id fallbacks', {
  config: pooled([{ providerId: '', modelId: '' }]),
  ops: [...reg, { op: 'integrations' }, { op: 'models' }],
})
generated('ambient resolution failure is ignored and caller apiKey is removed', {
  config: pooled(twoBackends), backing: std, ambient: { 'prov-a': { ok: false, error: 'vault locked' } },
  scripts: { 'prov-a': [ok('a')] },
  ops: [{ op: 'stream', slot: 's1', model: 'ultra', options: { apiKey: 'caller-key', maxTokens: 5, temperature: 0.2 } }],
})
generated('ambient headers and env merge over caller options with case-insensitive header names', {
  config: pooled(twoBackends), backing: std,
  ambient: { 'prov-a': { ok: true, apiKey: 'amb-a', headers: { authorization: 'amb' }, env: { REGION: 'west' }, baseUrl: 'https://ambient.invalid' } },
  scripts: { 'prov-a': [ok('a')] },
  ops: [{ op: 'stream', slot: 's1', model: 'ultra', options: { headers: { 'X-Trace': 'caller', Authorization: 'caller' }, env: { REGION: 'caller', KEEP: '1' } } }],
})
generated('ambient resolution without env keeps caller env and omits absent headers', {
  config: pooled(twoBackends), backing: std,
  ambient: { 'prov-a': { ok: true } },
  scripts: { 'prov-a': [ok('a')] },
  ops: [{ op: 'stream', slot: 's1', model: 'ultra', options: { env: { A: '1' }, reasoning: 'low' } }],
})
generated('ambient setup error that is transient falls over to the next backend', {
  config: pooled(twoBackends), backing: std, ambient: { 'prov-a': { throw: 'HTTP 503 vault' } }, scripts: { 'prov-b': [ok('b')] },
  ops: [{ op: 'stream', slot: 's1', model: 'ultra' }],
})
generated('ambient setup error that is fatal stops the request', {
  config: pooled(twoBackends), backing: std, ambient: { 'prov-a': { throw: 'bad vault configuration' } }, scripts: { 'prov-b': [ok('b')] },
  ops: [{ op: 'stream', slot: 's1', model: 'ultra' }],
})
generated('exhausted setup failures report the last setup error', {
  config: pooled(twoBackends), backing: std, ambient: { 'prov-a': { throw: 'HTTP 503 a' }, 'prov-b': { throw: 'HTTP 503 b' } },
  ops: [{ op: 'stream', slot: 's1', model: 'ultra' }],
})
generated('unavailable backend with bounded attempts throws instead of replaying', {
  config: pooled(twoBackends), backing: { 'prov-a': [modelA] }, service: { errorsBeforeSwitch: 1 }, providerOptions: { maxAccountAttempts: 2 },
  scripts: { 'prov-a': [fail('HTTP 500')] },
  ops: [{ op: 'stream', slot: 's1', model: 'ultra' }],
})
generated('bounded attempts replay the last terminal when the bound is reached', {
  config: pooled(twoBackends), backing: std, service: { errorsBeforeSwitch: 1 }, providerOptions: { maxAccountAttempts: 2 },
  scripts: { 'prov-a': [fail('HTTP 500')], 'prov-b': [fail('HTTP 503')] },
  ops: [{ op: 'stream', slot: 's1', model: 'ultra' }],
})
generated('forwarded terminal messages created after the stream started are stamped with durationMs', {
  config: pooled(twoBackends), backing: std,
  scripts: { 'prov-a': [fail('Connection error.'), { events: [{ type: 'done', reason: 'stop', message: { ...assistant('stop', { text: 'late' }), timestamp: '$NOW' } }] }] },
  ops: [{ op: 'stream', slot: 's1', model: 'ultra' }, { op: 'advance', ms: 5 }],
})
generated('attempt bound of one surfaces the first error without failover', {
  config: pooled(twoBackends), backing: std, service: { errorsBeforeSwitch: 1 }, providerOptions: { maxAccountAttempts: 1 },
  scripts: { 'prov-a': [fail('HTTP 500')], 'prov-b': [ok('b')] },
  ops: [{ op: 'stream', slot: 's1', model: 'ultra' }],
})
generated('onFailover throwing surfaces the callback error after the buffered terminal is set', {
  config: pooled(twoBackends), backing: std, service: { errorsBeforeSwitch: 1 }, onFailover: 'throw',
  scripts: { 'prov-a': [fail('HTTP 500')], 'prov-b': [ok('b')] },
  ops: [{ op: 'stream', slot: 's1', model: 'ultra' }],
})
generated('onFailover returning nothing rotates to the next backend', {
  config: pooled(twoBackends), backing: std, service: { errorsBeforeSwitch: 1 }, onFailover: 'void',
  scripts: { 'prov-a': [fail('HTTP 500')], 'prov-b': [ok('b')] },
  ops: [{ op: 'stream', slot: 's1', model: 'ultra' }],
})
generated('abort during the tolerance pause emits nothing and leaves health clean', {
  config: pooled(twoBackends), backing: std, abortOnPause: true,
  scripts: { 'prov-a': [fail('Connection error.')], 'prov-b': [ok('b')] },
  ops: [{ op: 'stream', slot: 's1', model: 'ultra', signal: true }, { op: 'snapshot' }],
})
generated('aborted terminal event is not tolerated', {
  config: pooled(twoBackends), backing: std,
  scripts: { 'prov-a': [{ events: [started('a'), abortedError()] }], 'prov-b': [ok('b')] },
  ops: [{ op: 'stream', slot: 's1', model: 'ultra', signal: true }],
})
generated('request aborted before the backend drains reports the backend abort', {
  config: pooled(twoBackends), backing: std,
  scripts: { 'prov-a': [{ events: [abortedError()] }], 'prov-b': [ok('b')] },
  ops: [{ op: 'open', slot: 's1', model: 'ultra', signal: true }, { op: 'abort', slot: 's1' }, { op: 'drain', slot: 's1' }],
})
generated('streamSimple kind is forwarded to the backing provider', {
  config: pooled(twoBackends), backing: std, scripts: { 'prov-a': [ok('a')] },
  ops: [{ op: 'stream', slot: 's1', model: 'ultra', kind: 'streamSimple' }],
})
generated('stream without a caller signal is never aborted', {
  config: pooled(twoBackends), backing: std, scripts: { 'prov-a': [ok('a')] },
  ops: [{ op: 'stream', slot: 's1', model: 'ultra' }],
})
generated('response status drives classification and the caller onResponse observes it', {
  config: pooled(twoBackends), backing: std, service: { errorsBeforeSwitch: 1 },
  scripts: { 'prov-a': [withResponse(fail('opaque'), 503, { 'retry-after': '2' })], 'prov-b': [ok('b')] },
  ops: [{ op: 'stream', slot: 's1', model: 'ultra', onResponse: true }],
})
generated('response receipt resets between physical streams', {
  config: pooled(twoBackends), backing: std,
  scripts: { 'prov-a': [withResponse(fail('opaque'), 503, { 'retry-after': '2' }), fail('opaque'), fail('opaque')], 'prov-b': [ok('b')] },
  ops: [{ op: 'stream', slot: 's1', model: 'ultra' }],
})
generated('status on the final tolerated attempt classifies the surfaced failure', {
  config: pooled(twoBackends), backing: std,
  scripts: { 'prov-a': [fail('opaque'), fail('opaque'), withResponse(fail('opaque'), 503, { 'retry-after': '3' })], 'prov-b': [ok('b')] },
  ops: [{ op: 'stream', slot: 's1', model: 'ultra' }],
})
generated('start event is buffered and only the latest start is emitted', {
  config: pooled(twoBackends), backing: std,
  scripts: { 'prov-a': [{ events: [started('first'), started('second'), { type: 'done', reason: 'stop', message: assistant('stop', { text: 'x' }) }] }], 'prov-b': [ok('b')] },
  ops: [{ op: 'stream', slot: 's1', model: 'ultra' }],
})
generated('terminal error after tolerance replays the buffered start of the last backend', {
  config: pooled(twoBackends), backing: std, service: { errorsBeforeSwitch: 1 },
  scripts: { 'prov-a': [{ events: [started('a'), { type: 'error', reason: 'error', error: assistant('error', { errorMessage: 'HTTP 500' }) }] }], 'prov-b': [{ events: [started('b'), { type: 'error', reason: 'error', error: assistant('error', { errorMessage: 'HTTP 503' }) }] }] },
  ops: [{ op: 'stream', slot: 's1', model: 'ultra' }],
})
generated('missing terminal after a fatal failure keeps the error text', {
  config: pooled(twoBackends), backing: std, scripts: { 'prov-a': [{ events: [] }], 'prov-b': [ok('b')] },
  ops: [{ op: 'stream', slot: 's1', model: 'ultra' }, { op: 'snapshot' }],
})
generated('rate limit cools the account down and the next session avoids it', {
  config: pooled(twoBackends), backing: std, service: { errorsBeforeSwitch: 1 },
  scripts: { 'prov-a': [fail('HTTP 429 slow down')], 'prov-b': [ok('b'), ok('b')] },
  ops: [{ op: 'stream', slot: 's1', model: 'ultra', affinityKey: 'session-1' }, { op: 'snapshot' }, { op: 'advance', ms: 1000 }, { op: 'stream', slot: 's2', model: 'ultra', affinityKey: 'session-2' }],
})
generated('fatal error is not failed over', {
  config: pooled(twoBackends), backing: std, service: { errorsBeforeSwitch: 1 },
  scripts: { 'prov-a': [fail('HTTP 400 invalid request')], 'prov-b': [ok('b')] },
  ops: [{ op: 'stream', slot: 's1', model: 'ultra' }],
})
generated('auth failure fails over with the account cooldown', {
  config: pooled(twoBackends), backing: std, service: { errorsBeforeSwitch: 1 },
  scripts: { 'prov-a': [fail('HTTP 401 unauthorized')], 'prov-b': [ok('b')] },
  ops: [{ op: 'stream', slot: 's1', model: 'ultra' }, { op: 'snapshot' }],
})
generated('explicit pin survives a surfaced failover', {
  config: pooled(twoBackends), backing: std, service: { errorsBeforeSwitch: 1 }, onFailover: 'surface',
  scripts: { 'prov-a': [fail('HTTP 500')], 'prov-b': [ok('b')] },
  ops: [{ op: 'pin', key: 'session-1', account: 'prov-a::model-a' }, { op: 'stream', slot: 's1', model: 'ultra', affinityKey: 'session-1' }, { op: 'affinity', key: 'session-1' }],
})
generated('implicit affinity hands off to the next backend after a surfaced failover', {
  config: pooled(twoBackends), backing: std, service: { errorsBeforeSwitch: 1 }, onFailover: 'surface',
  scripts: { 'prov-a': [fail('HTTP 500')], 'prov-b': [ok('b')] },
  ops: [{ op: 'stream', slot: 's1', model: 'ultra' }, { op: 'affinity', key: 'session-1' }, { op: 'stream', slot: 's2', model: 'ultra' }],
})
generated('disabled affinity leaves surfaced failovers unpinned', {
  config: pooled(twoBackends), backing: std, service: { errorsBeforeSwitch: 1, affinity: false }, onFailover: 'surface',
  scripts: { 'prov-a': [fail('HTTP 500'), ok('a2')], 'prov-b': [ok('b'), ok('b2')] },
  ops: [{ op: 'stream', slot: 's1', model: 'ultra' }, { op: 'affinity', key: 'session-1' }, { op: 'stream', slot: 's2', model: 'ultra' }],
})
generated('affinity key is read when the stream is created', {
  config: pooled(twoBackends), backing: std,
  scripts: { 'prov-a': [ok('a')], 'prov-b': [ok('b')] },
  ops: [
    { op: 'setAffinity', key: 'session-1' },
    { op: 'open', slot: 's1', model: 'ultra' },
    { op: 'setAffinity', key: 'session-2' },
    { op: 'drain', slot: 's1' },
    { op: 'stream', slot: 's2', model: 'ultra', affinityKey: 'session-2' },
  ],
})
generated('errors-before-switch is captured when the stream is created', {
  config: pooled(twoBackends), backing: std,
  scripts: { 'prov-a': [fail('HTTP 503'), fail('HTTP 503'), fail('HTTP 503')], 'prov-b': [ok('b')] },
  ops: [
    { op: 'open', slot: 's1', model: 'ultra' },
    { op: 'setSettings', settings: { errorsBeforeSwitch: 1 } },
    { op: 'drain', slot: 's1' },
  ],
})
generated('integration maxAccountAttempts is recorded but not enforced by the scheduler', {
  config: pooled(twoBackends), backing: std, integrationOptions: { maxAccountAttempts: 1 },
  scripts: { 'prov-a': [fail('HTTP 503'), fail('HTTP 503'), fail('HTTP 503')], 'prov-b': [ok('b')] },
  ops: [{ op: 'integrations' }, ...reg, { op: 'stream', slot: 's1', model: 'ultra' }],
})
generated('captured live backing metadata omits an undefined thinking map', {
  config: pooled(twoBackends),
  backing: { 'prov-x': [{ ...modelA, id: 'model-x', thinkingLevelMap: undefined }] },
  ops: [{ op: 'capture', provider: 'prov-x', model: 'model-x' }],
})
generated('classification sees the unavailable prefix and non-transport failures', {
  config: pooled(twoBackends),
  ops: [
    { op: 'classify', failure: { message: 'multiprovider: virtual backend unavailable: provider "x" is not registered', outputStarted: false } },
    { op: 'classify', failure: { message: 'NETWORK is down', outputStarted: false } },
    { op: 'classify', failure: { message: 'Connection\u{1F600}error', outputStarted: false } },
    { op: 'classify', failure: { message: 'HTTP 503 ok', outputStarted: false } },
  ],
})

// ---- run ----------------------------------------------------------------------------------------
const results = []
for (const { name, spec: specName, input } of cases) {
  // Every case registers its scheduler first unless it registers explicitly (registration itself is under test there).
  if (!input.ops.some(op => op.op === 'register')) input.ops.unshift({ op: 'register' })
  const result = await runCase(structuredClone(input))
  if (specName !== null) {
    specChecks[specName]({ ...result, ops: result.ops.filter((_, index) => input.ops[index].op !== 'register') })
  }
  results.push({ name, spec: specName, input, result })
}
const fixture = { reference: REFERENCE_COMMIT, referenceNode: process.versions.node, hashes, cases: results }
const text = `${JSON.stringify(fixture, null, 2)}\n`

if (mode === 'print') {
  process.stdout.write(text)
} else if (mode === 'write') {
  await writeFile(fixturePath, text)
  process.stdout.write(`wrote ${results.length} cases to ${fixturePath}\n`)
} else {
  const committed = await readFile(fixturePath, 'utf8')
  if (committed !== text) {
    process.stderr.write(`virtual-reference.json differs from regenerated output\n`)
    process.exitCode = 1
  } else {
    process.stdout.write(`check passed: ${results.length} cases byte-identical\n`)
  }
}
