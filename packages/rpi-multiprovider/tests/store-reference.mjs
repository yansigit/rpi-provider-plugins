import assert from "node:assert/strict";
import crypto, { createHash } from "node:crypto";
import { syncBuiltinESMExports } from "node:module";
import { mkdtemp, readFile, writeFile, rm, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";

const root = resolve(process.argv[2] ?? "../provider-references/multiprovider");
const file = join(root, "src/auth-store.ts");
assert.equal(createHash("sha256").update(await readFile(file)).digest("hex"), "29916553aa160b3c676ab843891da2b202fadf536fa220980d564b4ed50d48c6");
let sequence = 0, now = 1700000000000;
const DateOriginal = Date;
class Clock extends DateOriginal {
  constructor(...args) { super(...(args.length ? args : [now])); }
  static now() { return now; }
}
globalThis.Date = Clock;
crypto.randomUUID = () => `fixture-${++sequence}`;
syncBuiltinESMExports();
const { MultiAuthStore } = await import(pathToFileURL(file));
const credential = { type: "api_key", key: "fixture-secret", env: { ACCOUNT_REGION: "west" } };
const add = (input = {}, provider = "example") => ({ op: "add", provider, input: { label: "Work", credential, ...input } });
const template = { api: "test-api", baseUrl: "https://example.invalid", reasoning: true,
  thinkingLevelMap: { high: "high-effort", off: null }, input: ["text"],
  cost: { input: 1, output: 2, cacheRead: .5, cacheWrite: 0 }, contextWindow: 1000, maxTokens: 100 };
const virtual = { id: "pooled", label: " Pooled ", models: [{ id: "ultra", backends: [{ providerId: "prov-a", modelId: "model-a", weight: 2, template }, { providerId: "prov-b", modelId: "model-b" }] }] };
const cases = [
  { name: "empty", actions: [{ op: "list" }, { op: "pool" }, { op: "has" }, { op: "getScheduler" }, { op: "listVirtual" }, { op: "getVirtual", id: "none" }, { op: "remove", account: "none" }, { op: "removeVirtual", id: "none" }] },
  { name: "account lifecycle", actions: [add({ weight: 3, enabled: false }), { op: "pool" }, { op: "has" }, { op: "time", value: 1700000001234 }, { op: "account", input: { label: " Personal ", weight: 4.9, priority: -2.1, enabled: true } }, { op: "has" }, { op: "replace", credential: { type: "oauth", refresh: "fixture-refresh", access: "fixture-access", expires: 1700000061234, project: "fixture-project" } }, { op: "pool" }, { op: "remove" }, { op: "list" }, { op: "remove" }] },
  { name: "pool settings", actions: [add({ pool: { policy: "priority", upstream: { label: " Home ", weight: 3 } } }), add({ label: "Other", pool: { upstream: { priority: -1.1 } } }), { op: "setPool", input: { policy: "weighted-round-robin", affinity: false, includeUpstream: false, upstream: { label: " Team key ", weight: 4.9, priority: 2.7 } } }, { op: "pool" }, { op: "setPool", input: { upstream: {} } }, { op: "pool" }] },
  { name: "numeric normalization", actions: [add({ weight: .5, priority: 2.9 }), add({ label: "Other", weight: -3, priority: -1.5 }), { op: "account", input: { weight: 0, priority: null } }, { op: "pool" }] },
  { name: "scheduler overrides", actions: [{ op: "getScheduler" }, { op: "setScheduler", input: { rateLimitCooldownMs: 5000, quotaCooldownMs: 60000 } }, { op: "setScheduler", input: { authCooldownMs: 30000 }, clear: ["rateLimitCooldownMs"] }, { op: "getScheduler" }, { op: "setScheduler", clear: ["quotaCooldownMs", "authCooldownMs"] }, { op: "setScheduler", input: { rateLimitCooldownMs: -1 } }, { op: "setScheduler", input: { errorsBeforeSwitch: 0, unknown: 4 } }, { op: "getScheduler" }] },
  { name: "virtual lifecycle", actions: [{ op: "saveVirtual", input: virtual }, { op: "getVirtual", id: "pooled" }, { op: "listVirtual" }, { op: "saveVirtual", input: { ...virtual, models: [{ id: "ultra", label: " Fast ", backends: [{ providerId: "prov-a", modelId: "model-a" }] }] } }, { op: "getVirtual", id: "pooled" }, { op: "removeVirtual", id: "pooled" }, { op: "listVirtual" }, { op: "removeVirtual", id: "pooled" }] },
  { name: "safe keys and missing accounts", actions: [add({}, "__proto__"), add({}, "constructor"), add({}, "prototype"), add({}, "\uFEFF"), add({ label: " " }), add({ credential: null }), add({ credential: { type: "custom" } }), { op: "replace", credential }, { op: "account", input: { label: "x" } }, { op: "setPool", input: {} }, add(), { op: "account", input: { label: " " } }, { op: "replace", credential: { type: "custom" } }] },
  { name: "identity order and whitespace", actions: [add({}, "zeta"), add({}, "alpha"), add({}, "10"), add({}, "2"), { op: "list" }, { op: "pool", provider: "2" }, add({ label: "\uFEFF Label \uFEFF" }, " \u0085 "), { op: "list" }] },
];
for (const [name, state] of [
  ["root null", null], ["root number", 7], ["root array", []], ["version", { version: 2, providers: {} }],
  ["providers null", { version: 1, providers: null }], ["providers array", { version: 1, providers: [] }],
  ["pool null", { version: 1, providers: { p: null } }], ["accounts object", { version: 1, providers: { p: { accounts: {} } } }],
  ["upstream type", { version: 1, providers: { p: { accounts: [], upstream: [] } } }],
  ["upstream weight", { version: 1, providers: { p: { accounts: [], upstream: { weight: "heavy" } } } }],
  ["account null", { version: 1, providers: { p: { accounts: [null] } } }],
  ["account label", { version: 1, providers: { p: { accounts: [{ id: "a", label: 7 }] } } }],
  ["credential", { version: 1, providers: { p: { accounts: [{ id: "a", label: "A", credential: {} }] } } }],
  ["scheduler", { version: 1, providers: {}, scheduler: { errorsBeforeSwitch: -1 } }],
  ["unknown fields retained", { version: 1, providers: { p: { policy: "round-robin", affinity: true, includeUpstream: true, accounts: [{ id: "a", label: "A", credential, note: "retained" }], extra: 5 } }, extra: "retained", scheduler: { unknown: 3 } }],
]) cases.push({ name: `load ${name}`, initial: state, actions: [{ op: "pool", provider: "p" }, { op: "getScheduler" }, { op: "setScheduler", input: { authCooldownMs: 1 } }] });
for (const [name, input] of [
  ["id empty", { ...virtual, id: "" }], ["id separator", { ...virtual, id: "bad::id" }],
  ["label", { ...virtual, label: " " }], ["models", { ...virtual, models: [] }],
  ["backends", { ...virtual, models: [{ id: "ultra", backends: [] }] }],
  ["duplicate backend", { ...virtual, models: [{ id: "ultra", backends: [virtual.models[0].backends[0], virtual.models[0].backends[0]] }] }],
  ["enabled", { ...virtual, models: [{ id: "ultra", backends: [{ providerId: "p", modelId: "m", enabled: "yes" }] }] }],
  ["template", { ...virtual, models: [{ id: "ultra", backends: [{ providerId: "p", modelId: "m", template: { reasoning: true } }] }] }],
]) cases.push({ name: `virtual validation ${name}`, actions: [{ op: "saveVirtual", input }] });

cases.push({ name: "virtual locale order", actions: [
  ...["z", "Z", "a", "A", "ä", "å", "é", "e", "e\u0301", "Æ", "œ", "10", "2", "東京", "😀", "_first", "-first"].map(id => ({ op: "saveVirtual", input: { ...virtual, id } })),
  { op: "listVirtual" },
] });
cases.push({ name: "null upstream inputs", actions: [add({ pool: { upstream: null } }), { op: "setPool", input: { upstream: null } }, { op: "pool" }] });
for (const value of [false, 0, "", null, []]) cases.push({ name: `virtual storage ${JSON.stringify(value)}`, initial: { version: 1, providers: {}, virtuals: value }, actions: [{ op: "listVirtual" }, { op: "getVirtual", id: "pooled" }, { op: "saveVirtual", input: virtual }, { op: "removeVirtual", id: "pooled" }] });

for (const scenario of cases) {
  sequence = 0; now = 1700000000000;
  const directory = await mkdtemp(join(tmpdir(), "multiprovider-store-reference-"));
  const path = join(directory, "multiprovider-auth.json");
  const store = new MultiAuthStore(path);
  let last = "missing";
  const results = [];
  try {
    if ("initial" in scenario) await writeFile(path, `${JSON.stringify(scenario.initial, null, 2)}\n`, { mode: 0o600 });
    for (const action of scenario.actions) {
      let result;
      try {
        const provider = action.provider ?? "example", account = action.account ?? last;
        switch (action.op) {
          case "time": now = action.value; break;
          case "add": result = await store.addAccount(provider, action.input); last = result.id; break;
          case "replace": result = await store.replaceAccountCredential(provider, account, action.credential); break;
          case "account": result = await store.updateAccount(provider, account, action.input); break;
          case "setPool": result = await store.updatePool(provider, action.input); break;
          case "pool": result = await store.getPool(provider); break;
          case "has": result = await store.hasAccounts(provider); break;
          case "list": result = await store.listProviderIds(); break;
          case "remove": result = await store.removeAccount(provider, account); break;
          case "getScheduler": result = await store.getSchedulerSettings(); break;
          case "setScheduler": result = await store.updateSchedulerSettings({ ...action.input, ...Object.fromEntries((action.clear ?? []).map(key => [key, undefined])) }); break;
          case "saveVirtual": result = await store.saveVirtualProvider(action.input); break;
          case "getVirtual": result = await store.getVirtualProvider(action.id); break;
          case "listVirtual": result = await store.listVirtualProviders(); break;
          case "removeVirtual": result = await store.removeVirtualProvider(action.id); break;
          default: throw new Error(`unknown action ${action.op}`);
        }
        assert(!JSON.stringify(result ?? null).includes("fixture-secret"));
        result = { result: result ?? null };
      } catch (error) { result = { error: { name: error.name, message: error.message.replaceAll(path, "$STORE") } }; }
      let persisted = null, mode = null;
      try { persisted = await readFile(path, "utf8"); mode = (await stat(path)).mode & 0o777; } catch (error) { assert.equal(error.code, "ENOENT"); }
      results.push({ ...result, persisted, mode });
    }
    scenario.results = results;
  } finally { await rm(directory, { recursive: true, force: true }); }
}
process.stdout.write(`${JSON.stringify({ reference: "bd97d65a36f541935dc91e33ba15d7910a30f9d7", cases }, null, 2)}\n`);
