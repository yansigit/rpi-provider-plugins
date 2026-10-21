import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { createHash } from "node:crypto";
import { resolve, join } from "node:path";
import { pathToFileURL } from "node:url";

const root = resolve(process.argv[2] ?? "../provider-references/multiprovider");
for (const [file, expected] of Object.entries({
  "service.ts": "2eafa61662b3ee75a96a7b07a6ccad62ca600bbb2139a407b81cd9983564a026",
  "session-pins.ts": "45b1ff1fbfcc6853a717b917aaede404f32fe96a64208c75283a53e954bdc005",
  "types.ts": "aaf5e77ffc5738bfc5ee642a61238793ec0869123ceef12a9650c1384b0d181e",
  "errors.ts": "1e4f189fc4aac4488c84912b29bfc7e78629600ead41ce2924c8d8c3a3bac4b7",
})) assert.equal(createHash("sha256").update(await readFile(join(root, "src", file))).digest("hex"), expected);
const { MultiProviderService } = await import(pathToFileURL(join(root, "src/service.ts")));
const pins = await import(pathToFileURL(join(root, "src/session-pins.ts")));
const accounts = [
  { id: "a", label: "Work", authKind: "api-key", credentialRef: "secret-work", weight: 3, priority: 1 },
  { id: "b", label: "Personal", authKind: "oauth", credentialRef: "secret-personal", weight: 1, priority: 2 },
];
const register = (extra = {}) => ({ op: "register", id: "example", label: "Example", accounts, ...extra });
const acquire = (extra = {}) => ({ op: "acquire", providerId: "example", ...extra });
const release = (lease, outcome = { status: "success" }) => ({ op: "release", lease, outcome });
const failure = (status, extra = {}) => ({ status: "failure", error: { message: `HTTP ${status}`, status, outputStarted: false, ...extra } });
const snapshot = { op: "snapshot" };
const pin = (accountId = "b") => ({ op: "pin", providerId: "example", key: "session", accountId });
const affinity = { op: "affinity", providerId: "example", key: "session" };
const pool = patch => ({ op: "pool", providerId: "example", patch });
const select = (n, extra = {}) => Array.from({ length: n }, (_, i) => [acquire(extra), release(`lease-${i + 1}`)]).flat();
const cases = [];
const add = (name, actions, options = {}) => cases.push({ name, options, actions });
add("settings", [{ op: "errors" }, { op: "settings", patch: { errorsBeforeSwitch: 5 } }, { op: "errors" }, { op: "settings", patch: { errorsBeforeSwitch: 0 } }, { op: "errors" }, { op: "settings", patch: { rateLimitCooldownMs: -1 } }, { op: "settings", patch: { errorsBeforeSwitch: 2.9 } }, { op: "errors" }]);
add("biased selection and private snapshot", [register(), ...select(3), snapshot, { op: "enabled", providerId: "example" }, { op: "preference", providerId: "example" }]);
add("concurrent fresh session", [register(), acquire({ affinityKey: "fresh" }), acquire({ affinityKey: "fresh" }), snapshot, release("lease-1"), release("lease-2"), snapshot]);
add("implicit spillover", [register(), acquire({ affinityKey: "session", excludeAccountIds: ["a"] }), release("lease-1"), acquire({ affinityKey: "session" }), acquire({ affinityKey: "other" }), snapshot]);
add("inventory order", [register({ accounts: [...accounts].reverse() }), ...select(2)]);
for (const policy of ["round-robin", "weighted-round-robin", "least-inflight", "priority"]) {
  add(`policy ${policy}`, [register({ selectionBias: "none" }), ...Array.from({ length: 8 }, () => acquire()), snapshot, ...Array.from({ length: 8 }, (_, i) => release(`lease-${i + 1}`)), snapshot], { defaultPolicy: policy, affinity: false });
}
add("equal weight offset", [register({ selectionBias: "none", accounts: ["zeta", "alpha", "mid"].map(id => ({ id, label: id, authKind: "custom", credentialRef: id })) }), ...select(6)], { randomOffset: 1, affinity: false });
for (const policy of ["round-robin", "weighted-round-robin"]) {
  for (let offset = 0; offset < 10; offset++) {
    add(`weighted ${policy} offset ${offset}`, [register({ selectionBias: "none", accounts: accounts.map((account, index) => ({ ...account, weight: index ? 4 : 6 })) }), ...select(20)], { randomOffset: offset, affinity: false, defaultPolicy: policy });
  }
}
add("explicit pin cooldown exclusion and return", [register(), pin(), affinity, acquire({ affinityKey: "session" }), release("lease-1", failure(429)), acquire({ affinityKey: "session" }), release("lease-2"), affinity, { op: "now", value: 61000 }, acquire({ affinityKey: "session", excludeAccountIds: ["b"] }), release("lease-3"), affinity, acquire({ affinityKey: "session" }), release("lease-4"), affinity]);
add("pins without affinity", [register(), acquire({ affinityKey: "session" }), release("lease-1"), affinity, pin(), acquire({ affinityKey: "session" }), affinity, { op: "clear", providerId: "example", key: "session" }, affinity, acquire({ affinityKey: "session" })], { affinity: false });
add("disabled stale and unknown pins", [register(), pin("missing"), pin("a"), pool({ accounts: [{ accountId: "a", enabled: false, weight: 1, priority: 0 }] }), pin("a"), acquire({ affinityKey: "session" }), affinity, { op: "clear" }, affinity, snapshot]);
add("release idempotence", [register(), acquire(), snapshot, release("lease-1", failure(429)), release("lease-1"), snapshot, { op: "now", value: 1500 }, acquire({ excludeAccountIds: ["b"] })], { rateLimitCooldownMs: 500 });
add("preferences health independence", [register(), pool({ policy: "priority", affinity: false, accounts: [{ accountId: "a", enabled: false, weight: 1, priority: 0 }, { accountId: "b", enabled: true, weight: 1, priority: -1 }] }), acquire(), { op: "reset", providerId: "example", accountId: "a" }, snapshot, { op: "preference", providerId: "example" }]);
add("cooldown settings live", [register(), { op: "settings", patch: { rateLimitCooldownMs: 250 } }, acquire(), release("lease-1", failure(429)), snapshot, { op: "now", value: 1250 }, acquire()]);
const modelFailure = scope => failure(429, { cause: { disposition: { kind: "rate-limit", retryable: true, cooldownMs: 60000, scope } } });
add("account cooldown blocks all models", [register({ classify: true }), acquire(), release("lease-1", modelFailure("account")), acquire({ modelId: "x" }), acquire({ modelId: "y" }), snapshot]);
add("model cooldown isolation success and omitted model", [register({ classify: true }), acquire({ modelId: "x" }), release("lease-1", modelFailure("account-model")), acquire({ modelId: "x" }), acquire({ modelId: "y" }), release("lease-3"), acquire({ modelId: "x" }), acquire(), snapshot]);
add("same model success clears health", [register({ classify: true }), acquire({ modelId: "x" }), acquire({ modelId: "x" }), release("lease-1", modelFailure("account-model")), release("lease-2"), acquire({ modelId: "x" }), snapshot]);
add("inventory drift prunes model evidence", [register({ classify: true }), acquire({ modelId: "x" }), release("lease-1", modelFailure("account-model")), { op: "inventory", providerId: "example", accounts: [accounts[1]] }, snapshot, { op: "inventory", providerId: "example", accounts }, acquire({ modelId: "x" }), snapshot]);
add("model reset unregister and stale unregister", [register({ classify: true }), acquire({ modelId: "x" }), release("lease-1", modelFailure("account-model")), { op: "reset", providerId: "example", accountId: "a" }, acquire({ modelId: "x" }), release("lease-2", modelFailure("account-model")), { op: "unregister", registration: 0 }, register(), { op: "unregister", registration: 0 }, acquire({ modelId: "x" }), snapshot]);
add("late release and retained account health", [register(), acquire(), pool({ affinity: false }), { op: "unregister", registration: 0 }, register(), release("lease-1", failure(401)), acquire(), snapshot]);
add("model cooldown expires in snapshot", [register({ classify: true }), acquire({ modelId: "x" }), release("lease-1", modelFailure("account-model")), snapshot, { op: "now", value: 61000 }, snapshot]);
add("no accounts earliest boundary", [register({ classify: true }), acquire({ modelId: "x" }), acquire({ modelId: "x", excludeAccountIds: ["a"] }), release("lease-1", modelFailure("account-model")), release("lease-2", failure(429)), acquire({ modelId: "x" }), acquire({ excludeAccountIds: ["a", "b"] }), snapshot]);
add("model classifier without model falls back to account", [register({ classify: true }), acquire(), release("lease-1", modelFailure("account-model")), acquire({ modelId: "other" }), snapshot]);
add("overlapping cooldown reports earliest boundary", [register({ classify: true, accounts: [accounts[0]] }), acquire({ modelId: "x" }), acquire({ modelId: "x" }), release("lease-1", modelFailure("account-model")), release("lease-2", failure(401)), acquire({ modelId: "x" }), snapshot]);
add("cancelled leases preserve health", [register(), acquire(), acquire(), release("lease-1", failure(503)), release("lease-2", { status: "cancelled" }), snapshot]);
add("negative weights and signed zero priority", [register({ accounts: accounts.map((account, i) => ({ ...account, weight: i ? -1 : 0, priority: i ? -0 : 0 })) }), ...select(4)], { defaultPolicy: "priority", affinity: false });
add("classifier exception", [register({ classify: true }), acquire(), release("lease-1", failure(429, { cause: { throws: true } })), snapshot]);
add("registration validation", [register({ id: " " }), register({ id: "\uFEFF" }), register(), register(), acquire({ providerId: "unknown" }), pin("missing"), { op: "inventory", providerId: "example", accounts: [...accounts, accounts[0]] }, snapshot, { op: "inventory", providerId: "example", accounts: [{ ...accounts[0], id: "" }] }, snapshot]);
add("backoff and maximum", [register(), ...Array.from({ length: 13 }, () => acquire()), ...Array.from({ length: 13 }, (_, i) => [release(`lease-${i + 1}`, failure(503)), snapshot]).flat()], { maxCooldownMs: 5000 });
for (const message of ["401: authentication rejected", "status code = 408", "HTTP 425", "503 unavailable", "rate-limit", "overloaded", "usage limit", "out of credits", "token expired", "invalid API key", "something else", "status: 4290", "４２９:", "HTTP\uFEFF429", "rate\u2028limit", "limit\rreached", "rate😀limit", "status: 429é", "ſtatus: 429", "HTTP\u0085429"]) {
  add(`classification ${message}`, [register(), acquire(), release("lease-1", { status: "failure", error: { message, outputStarted: false } }), snapshot]);
}
add("pin replay pending stale disabled and automatic", [register({ accounts: [accounts[0], { ...accounts[1], enabled: false }] }), { op: "pins", pins: [
  { pool: "later", key: "session", accountId: "a" },
  { pool: "example", key: "session", accountId: "gone", label: "Removed" },
  { pool: "example", key: "session", accountId: "b", label: "Personal" },
  { pool: "example", key: "session", accountId: "a" },
  { pool: "example", key: "session" },
] }, affinity, register({ id: "later" }), { op: "pins", pins: [{ pool: "later", key: "session", accountId: "a" }] }, acquire({ providerId: "later", affinityKey: "session" })]);
for (const boundary of [1000.5, 951782400000, 253402300799999, 253402300800000, 8640000000000000, 8640000000000001, 1e308]) {
  add(`cooldown date ${boundary}`, [register({ accounts: [accounts[0]] }), acquire(), release("lease-1", failure(429)), acquire()], { rateLimitCooldownMs: boundary - 1000, maxCooldownMs: 1e308 });
}
for (const weight of [1.5, 281474976710656, 9007199254740992, 1e20, 1e308]) {
  add(`random range ${weight}`, [register({ accounts: [{ ...accounts[0], weight }] }), acquire(), acquire(), snapshot], { defaultPolicy: "weighted-round-robin", nativeRandom: true });
}
for (const scenario of cases) {
  let now = 1000, nextId = 0;
  const { randomOffset = 0, nativeRandom, ...options } = scenario.options;
  const service = new MultiProviderService({ ...options, now: () => now, randomId: () => `lease-${++nextId}`, ...(nativeRandom ? {} : { randomInt: max => randomOffset % max }) });
  const inventory = new Map(), registrations = [], leases = new Map(), results = [];
  for (const action of scenario.actions) {
    try {
      let result;
      switch (action.op) {
        case "register": {
          const value = { current: action.accounts }; inventory.set(action.id, value);
          registrations.push(service.registerProvider({ ...action, accounts: () => value.current,
            classifyFailure: action.classify ? failure => { if (failure.cause?.throws) throw new Error("fixture failure"); return failure.cause?.disposition; } : undefined }));
          break;
        }
        case "pins": {
          const events = [];
          const pending = await pins.applySessionPins(action.pins, { hasPool: id => service.hasProvider(id), pin: (...args) => service.pinAccount(...args), clear: (...args) => service.clearAffinity(...args) },
            (pin, error) => events.push({ error: error.message, pin }), pin => events.push({ applied: pin }));
          result = { pending, events }; break;
        }
        case "inventory": inventory.get(action.providerId).current = action.accounts; break;
        case "unregister": registrations[action.registration](); break;
        case "acquire": {
          const lease = await service.acquire(action); leases.set(lease.id, lease);
          result = { id: lease.id, providerId: lease.providerId, accountId: lease.accountId, credentialRef: lease.credentialRef, acquiredAt: lease.acquiredAt }; break;
        }
        case "release": result = leases.get(action.lease).release(action.outcome); break;
        case "snapshot": result = await service.snapshot(); assert(!JSON.stringify(result).includes("secret-")); break;
        case "now": now = action.value; break;
        case "pin": await service.pinAccount(action.providerId, action.key, action.accountId); break;
        case "affinity": result = service.getAffinity(action.providerId, action.key); break;
        case "clear": service.clearAffinity(action.providerId, action.key); break;
        case "reset": service.resetHealth(action.providerId, action.accountId); break;
        case "pool": result = await service.updatePool(action.providerId, action.patch); break;
        case "preference": result = service.getPoolPreference(action.providerId); break;
        case "settings": service.updateSchedulerDefaults(action.patch); break;
        case "errors": result = service.getErrorsBeforeSwitch(); break;
        case "enabled": result = await service.hasEnabledAccounts(action.providerId); break;
        default: throw new Error(`unknown fixture action ${action.op}`);
      }
      results.push({ result: result ?? null });
    } catch (error) {
      results.push({ error: { name: error.name, message: error.message, ...(error.nextAvailableAt === undefined ? {} : { nextAvailableAt: error.nextAvailableAt }) } });
    }
  }
  scenario.results = results;
}
const pinInputs = [null, {}, [], [null, 4, "text", {}, { pool: "" }, { pool: "ok", accountId: null }, { pool: "ok", accountId: 1 }, { pool: "ok", accountId: " " }],
  [{ pool: "p", key: "s", accountId: "a", label: "Work" }, { pool: "q", key: "s", accountId: "b" }, { pool: "p", key: "s" }, { pool: "p", key: "t", accountId: "c", label: "  " }],
  [{ pool: "\uFEFF p ", key: " s\uFEFF", accountId: " a\uFEFF", label: " Personal " }, { pool: "\u0085", key: "s" }, { pool: "p", key: "\uFEFF" }],
];
const pinCases = pinInputs.map(input => {
  const entries = Array.isArray(input) ? input.map(data => ({ type: "custom", customType: pins.SESSION_PIN_ENTRY_TYPE, data })) : [];
  entries.unshift(null, { type: "custom", customType: "foreign", data: { pool: "foreign", key: "s" } }, { type: "message", customType: pins.SESSION_PIN_ENTRY_TYPE, data: { pool: "foreign", key: "s" } });
  return { input, entries, inherited: pins.inheritedSessionPinsFromUnknown(input), serialized: pins.serializeInheritedSessionPins(input), journal: pins.sessionPinsFromEntries(entries) };
});
const envCases = [undefined, null, 7, "", "  ", "{", JSON.stringify(pinInputs[4])].map(value => {
  const env = value === undefined ? {} : { [pins.SESSION_PIN_ENV]: value };
  return { env, result: pins.inheritedSessionPinsFromEnv(env) };
});
let entered, resume;
const started = new Promise(resolve => { entered = resolve; });
const gate = new Promise(resolve => { resume = resolve; });
const changing = new MultiProviderService({ now: () => 1000 });
const original = changing.registerProvider({ id: "a", label: "Old", accounts: async () => { entered(); await gate; return accounts; } });
const removed = changing.registerProvider({ id: "b", label: "Removed", accounts: () => accounts });
const changingSnapshot = changing.snapshot();
await started;
removed(); original();
changing.registerProvider({ id: "c", label: "Added", accounts: () => accounts });
changing.registerProvider({ id: "a", label: "New", selectionBias: "none", accounts: () => accounts });
resume();
const asyncChecks = { snapshotMutation: await changingSnapshot };
process.stdout.write(`${JSON.stringify({ asyncChecks, pinCases, envCases, reference: "bd97d65a36f541935dc91e33ba15d7910a30f9d7", cases }, null, 2)}\n`);
