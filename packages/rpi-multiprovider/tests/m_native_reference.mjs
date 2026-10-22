// Drives the unchanged upstream account store to prove the native credential
// store adapter returns exactly the credential object upstream persists.
// Usage: node tests/m_native_reference.mjs [root] [--write]
import assert from 'node:assert/strict'
import crypto, { createHash } from 'node:crypto'
import { syncBuiltinESMExports } from 'node:module'
import { mkdtempSync } from 'node:fs'
import { readFile, rm, writeFile } from 'node:fs/promises'
import { homedir } from 'node:os'
import { join, resolve } from 'node:path'
import { pathToFileURL } from 'node:url'

const root = resolve(process.argv[2] ?? '../provider-references/multiprovider')
const write = process.argv.includes('--write')
const hashes = {}
for (const name of ['auth-store.ts', 'types.ts', 'errors.ts']) {
  hashes[name] = createHash('sha256').update(await readFile(join(root, 'src', name))).digest('hex')
}
assert.equal(hashes['auth-store.ts'], '29916553aa160b3c676ab843891da2b202fadf536fa220980d564b4ed50d48c6')
assert.equal(hashes['types.ts'], 'aaf5e77ffc5738bfc5ee642a61238793ec0869123ceef12a9650c1384b0d181e')
assert.equal(hashes['errors.ts'], '1e4f189fc4aac4488c84912b29bfc7e78629600ead41ce2924c8d8c3a3bac4b7')

const now = 1700000000000
let sequence = 0
const OriginalDate = Date
class Clock extends OriginalDate {
  constructor(...args) { super(...(args.length ? args : [now])) }
  static now() { return now }
}
globalThis.Date = Clock
crypto.randomUUID = () => `account-${++sequence}`
syncBuiltinESMExports()

const { MultiAuthStore } = await import(pathToFileURL(join(root, 'src/auth-store.ts')))

const directory = mkdtempSync(join(homedir(), 'pi-multiprovider-native-'))
const path = join(directory, 'multiprovider-auth.json')
const store = new MultiAuthStore(path)

const accounts = [
  {
    label: 'Work',
    credential: { type: 'api_key', key: 'k-work', env: { ACCOUNT_REGION: 'west' } },
  },
  {
    label: 'Personal',
    credential: { type: 'api_key', key: 'k-personal' },
  },
  {
    label: 'OAuth',
    credential: { type: 'oauth', access: 'access-1', refresh: 'refresh-1', expires: now + 600_000 },
  },
  {
    label: 'OAuth expired',
    credential: { type: 'oauth', access: 'access-2', refresh: 'refresh-2', expires: 0 },
  },
]

for (const account of accounts) {
  await store.addAccount('example', account)
}

const persisted = async () => JSON.parse(await readFile(path, 'utf8'))
const first = await persisted()
const firstAccounts = first.providers.example.accounts.map(account => ({
  id: account.id,
  label: account.label,
  authKind: account.authKind,
  credential: account.credential,
}))

const publicPool = await store.getPool('example')
const inventory = publicPool.accounts.map(account => ({
  id: account.id,
  label: account.label,
  authKind: account.authKind,
  enabled: account.enabled,
  weight: account.weight,
  priority: account.priority,
}))

// One in-place credential replacement on the first account.
await store.replaceAccountCredential('example', firstAccounts[0].id, {
  type: 'oauth',
  access: 'replaced-access',
  refresh: 'replaced-refresh',
  expires: now + 60_000,
})
const replaced = await persisted()
const replacedAccount = replaced.providers.example.accounts.find(
  account => account.id === firstAccounts[0].id,
)

// A malformed record is refused by the upstream store and must be refused by the
// native adapter too.
const malformedPath = join(directory, 'malformed.json')
await writeFile(
  malformedPath,
  JSON.stringify({
    version: 1,
    providers: {
      example: {
        policy: 'round-robin',
        affinity: true,
        includeUpstream: true,
        accounts: [
          {
            id: 'bad',
            label: 'Bad',
            authKind: 'api-key',
            credential: { type: 'unsupported', key: 'nope' },
            enabled: true,
            weight: 1,
            priority: 1,
            createdAt: new Date(now).toISOString(),
            updatedAt: new Date(now).toISOString(),
          },
        ],
      },
    },
  }),
)

let malformedError
try {
  await new MultiAuthStore(malformedPath).getPool('example')
} catch (error) {
  malformedError = error instanceof Error ? error.message : String(error)
}

const fixture = {
  reference: 'bd97d65a36f541935dc91e33ba15d7910a30f9d7',
  upstreamHashes: hashes,
  provider: 'example',
  accounts: firstAccounts,
  inventory,
  state: first,
  replaced: {
    id: firstAccounts[0].id,
    account: {
      id: replacedAccount.id,
      label: replacedAccount.label,
      authKind: replacedAccount.authKind,
      credential: replacedAccount.credential,
    },
    state: replaced,
  },
  malformed: {
    error: malformedError,
  },
}

await rm(directory, { recursive: true, force: true })

const text = `${JSON.stringify(fixture, null, 2)}\n`
const target = join(import.meta.dirname, 'm_native-reference.json')
if (write) {
  await writeFile(target, text)
} else {
  const committed = await readFile(target, 'utf8')
  assert.equal(committed, text, 'm_native-reference.json is stale; run with --write')
}
