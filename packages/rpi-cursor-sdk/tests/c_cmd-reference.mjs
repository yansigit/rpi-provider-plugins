#!/usr/bin/env node
// Differential command/config/trust/provenance reference for ledger C08.
//
// Runs the SAME scripted scenarios through two module graphs and compares COMPLETE
// values:
//   reference: the UNCHANGED pinned upstream `pi-cursor-sdk@0.5.3` sources under
//              `provider-references/cursor` (sha256 asserted before import);
//   candidate: the retained rpi worker port under `worker/src` (the C08 port files).
//
// Covered: config precedence/trust/safety caps, config-file parsing, environment
// parsing, fast-default resolution, store-root resolution, config merge, setting
// sources, environment booleans, API-key normalization, request-provenance digest
// and resolution, and command/flag/provider identity registered through a scripted
// Pi extension API (the host-facade identity the native registrars consume).
//
//   node --experimental-transform-types --import ./tests/c_cmd/register.mjs \
//        tests/c_cmd-reference.mjs            # check mode (default)
//   ... tests/c_cmd-reference.mjs --write    # regenerate tests/c_cmd-evidence.json
//
// Never touches ~/.pi, ~/.rpi, real credentials or the network.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const PACKAGE_ROOT = resolve(HERE, "..");
const EVIDENCE_PATH = join(HERE, "c_cmd-evidence.json");
const WRITE = process.argv.includes("--write");

// Isolate the run from the real home: `discoverModels` and the default agent-dir
// helpers must not read ~/.pi, and every write stays inside this sandbox.
const SANDBOX = mkdtempSync(join(homedir(), ".rpi-c-cmd-"));
process.env.HOME = SANDBOX;
process.env.PI_CODING_AGENT_DIR = join(SANDBOX, "agent");
process.env.TMPDIR = join(SANDBOX, "tmp");
mkdirSync(process.env.TMPDIR, { recursive: true });

// Lane worktrees sit directly under the Projects directory; provider-references is
// their sibling.
const REFERENCES_ROOT = resolve(
	process.env.RPI_PROVIDER_REFERENCES ?? join(HERE, "../../../../provider-references"),
);
const REFERENCE_ROOT = process.env.RPI_CURSOR_REFERENCE
	? resolve(process.env.RPI_CURSOR_REFERENCE)
	: join(REFERENCES_ROOT, "cursor");
const UPSTREAM_SRC = join(REFERENCE_ROOT, "src");
const UPSTREAM_SHARED = join(REFERENCE_ROOT, "shared");
const WORKER_SRC = join(PACKAGE_ROOT, "worker/src");
const WORKER_SHARED = join(PACKAGE_ROOT, "worker/shared");

const sha256File = (path) => createHash("sha256").update(readFileSync(path)).digest("hex");

// Upstream files this script imports, with their pinned sha256 (also recorded in
// pi-rust/docs/provider-parity/file-mapping.json). A mismatch aborts before import.
const UPSTREAM_FILES = {
	"src/cursor-config.ts": "1721a93ae3266d87b78ebf406c46cf71cb4d63b7fc476cded717fd43ec341117",
	"src/cursor-setting-sources.ts": "aa14f857e1825cb6589bc4683105b691ee6212dc631034feb7623bb47d53e4a9",
	"shared/cursor-setting-sources.mjs": "c2a6a0d78c2308c51176f90d85fa46bd86a8f5719992e99c5a761d04b8f024fe",
	"src/cursor-env-boolean.ts": "b7a9b58f5e85f6f1ebea64a0c11afa298542debbacff2a4ae1636624ef6a34ba",
	"src/cursor-request-provenance.ts": "c1b230ccde1cf5cc8a71778b4653b1500a103c35076f6b954a8e17d0e5aa6494",
	"src/cursor-api-key.ts": "228262af27698d38169b048d4388c34975e2d66265938dc4abbf040e4e0faa67",
	"src/index.ts": "42924f74cb383e761a2fd547d5b1c14e7167b125777794f955ea4103118b2607",
	"src/cursor-state.ts": "9ed0f11214eb6df5c704963eff5bcca7b321070f7c94289e3e9dc256f33ebc00",
	"src/cursor-runtime-state.ts": "b7689529e221039f6084f721e49c990fa7354f00f0bda7c67a7eb8c89b606d62",
	"src/cursor-usage-command.ts": "7d7a74fdff69bc9fbb63d124d26adcee2454115361496861d3912d4680dcd98e",
};
const upstreamHashes = {};
for (const [relative, expected] of Object.entries(UPSTREAM_FILES)) {
	const actual = sha256File(join(REFERENCE_ROOT, relative));
	assert.equal(actual, expected, `upstream ${relative} changed`);
	upstreamHashes[relative] = actual;
}
// The retained worker port files are recorded (not pinned) so drift is visible.
const PORT_FILES = [
	"src/cursor-config.ts",
	"src/cursor-setting-sources.ts",
	"src/cursor-env-boolean.ts",
	"src/cursor-request-provenance.ts",
	"src/cursor-api-key.ts",
	"src/index.ts",
	"src/cursor-state.ts",
	"src/cursor-runtime-state.ts",
	"src/cursor-usage-command.ts",
	"shared/cursor-setting-sources.mjs",
];
const portHashes = {};
for (const relative of PORT_FILES) {
	const isShared = relative.startsWith("shared/");
	const path = isShared
		? join(WORKER_SHARED, relative.slice("shared/".length))
		: join(WORKER_SRC, relative.slice("src/".length));
	portHashes[relative] = sha256File(path);
}

function sortValue(value) {
	if (Array.isArray(value)) return value.map(sortValue);
	if (value && typeof value === "object") {
		return Object.fromEntries(
			Object.keys(value)
				.sort((a, b) => (a < b ? -1 : a > b ? 1 : 0))
				.map((key) => [key, sortValue(value[key])]),
		);
	}
	return value;
}
const stable = (value) => JSON.stringify(sortValue(value), null, 2);

// ---------------------------------------------------------------- module loading

async function loadRuntime(kind) {
	const src = kind === "reference" ? UPSTREAM_SRC : WORKER_SRC;
	const shared =
		kind === "reference"
			? pathToFileURL(join(UPSTREAM_SHARED, "cursor-setting-sources.mjs")).href
			: pathToFileURL(join(WORKER_SHARED, "cursor-setting-sources.mjs")).href;
	const config = await import(pathToFileURL(join(src, "cursor-config.ts")).href);
	const settingSources = await import(pathToFileURL(join(src, "cursor-setting-sources.ts")).href);
	const envBoolean = await import(pathToFileURL(join(src, "cursor-env-boolean.ts")).href);
	const apiKey = await import(pathToFileURL(join(src, "cursor-api-key.ts")).href);
	const provenance = await import(pathToFileURL(join(src, "cursor-request-provenance.ts")).href);
	const sharedModule = await import(shared);
	const entry = await import(pathToFileURL(join(src, "index.ts")).href);
	return { config, settingSources, envBoolean, apiKey, provenance, shared: sharedModule, entry };
}

// ---------------------------------------------------------------- scripted cases

const ENV_BOOL_INPUTS = [
	undefined, "", " ", "0", "false", "off", "none", "no", "disabled",
	"1", "true", "on", "yes", "enabled", "TRUE", "On", "Yes", "maybe", "2", "-1", " true ",
];
const SETTING_SOURCE_INPUTS = [
	undefined, "", "  ", "all", "ALL", "1", "true", "on", "0", "false", "off", "none",
	"omit", "disabled", "user", "project", "user,project", " user , project ", ",",
	"user,", "USER", "all,user", "user,project,all", "  user  ", "user,user",
];
const API_KEY_INPUTS = [
	undefined, "", "  ", "CURSOR_API_KEY", "$CURSOR_API_KEY", "${CURSOR_API_KEY}",
	"pi-cursor-sdk-cursor-api-key-placeholder", " real-key ", "secret", " CURSOR_API_KEY ",
];

function configPrecedenceCases() {
	const cases = [];
	const add = (name, options) => cases.push({ name, options });
	add("empty", {});
	add("cli runtime local", { cli: { runtime: "local" } });
	add("cli runtime cloud", { cli: { runtime: "cloud" } });
	add("env runtime cloud", { env: { PI_CURSOR_RUNTIME: "cloud" } });
	add("env runtime invalid ignored", { env: { PI_CURSOR_RUNTIME: "spaceship" } });
	add("session runtime cloud", { session: { runtime: "cloud" } });
	add("project runtime cloud", { project: { runtime: "cloud" } });
	add("user runtime cloud", { user: { runtime: "cloud" } });
	add("cli runtime beats env/session/project/user", {
		cli: { runtime: "local" },
		env: { PI_CURSOR_RUNTIME: "cloud" },
		session: { runtime: "cloud" },
		project: { runtime: "cloud" },
		user: { runtime: "cloud" },
	});
	add("user cloud cannot loosen a safer project local", {
		project: { runtime: "local" },
		user: { runtime: "cloud" },
	});
	add("user local caps a riskier project cloud", {
		project: { runtime: "cloud" },
		user: { runtime: "local" },
	});
	add("cli cloud stays uncapped", {
		cli: { runtime: "cloud" },
		user: { runtime: "local" },
		project: { runtime: "local" },
	});
	add("builtin runtime default local", { builtIn: {} });
	add("builtin runtime override cloud", { builtIn: { runtime: "cloud" } });
	add("context handoff cli never", { cli: { cloud: { contextHandoff: "never" } } });
	add("context handoff env bootstrap", { env: { PI_CURSOR_CLOUD_CONTEXT: "bootstrap" } });
	add("context handoff session fresh", { session: { cloud: { contextHandoff: "fresh" } } });
	add("context handoff project is omitted from cloud precedence (user never)", {
		project: { cloud: { contextHandoff: "bootstrap" } },
		user: { cloud: { contextHandoff: "never" } },
	});
	add("context handoff project never cannot loosen user bootstrap", {
		project: { cloud: { contextHandoff: "never" } },
		user: { cloud: { contextHandoff: "bootstrap" } },
	});
	add("cloud repo/branch skip project layer", {
		project: { cloud: { repo: "project/repo", branch: "project-branch" } },
		user: { cloud: { repo: "user/repo" } },
		env: { PI_CURSOR_CLOUD_REPO: "env/repo", PI_CURSOR_CLOUD_BRANCH: "env-branch" },
	});
	add("cloud booleans across layers", {
		cli: { cloud: { directPush: true, autoCreatePR: true } },
		env: { PI_CURSOR_CLOUD_ALLOW_LOCAL_STATE: "1", PI_CURSOR_CLOUD_ACK: "yes" },
		session: { cloud: { skipReviewerRequest: true } },
		user: { cloud: { directPush: false } },
	});
	add("cloud user false caps session true", {
		session: { cloud: { directPush: true, autoCreatePR: true, allowLocalState: true } },
		user: { cloud: { directPush: false, autoCreatePR: false, allowLocalState: false } },
	});
	add("cloud user true does not loosen session false", {
		session: { cloud: { directPush: false } },
		user: { cloud: { directPush: true } },
	});
	add("cloud envNames cli list", { cli: { cloud: { envNames: ["A", "B", "A"] } } });
	add("cloud envNames env comma list", { env: { PI_CURSOR_CLOUD_ENV: "A, B , CURSOR_SECRET,9BAD" } });
	add("cloud envNames session then user cap filters", {
		session: { cloud: { envNames: ["A", "B", "C"] } },
		user: { cloud: { envNames: ["A"] } },
	});
	add("cloud envNames user cannot cap user base", {
		user: { cloud: { envNames: ["A", "B"] } },
	});
	add("cloud environment type/name", {
		env: { PI_CURSOR_CLOUD_ENV_TYPE: "pool", PI_CURSOR_CLOUD_ENV_NAME: "my-pool" },
	});
	add("cloud envFromFiles and acknowledged", {
		env: { PI_CURSOR_CLOUD_ENV_FROM_FILES: "1", PI_CURSOR_CLOUD_ACK: "0" },
	});
	add("local autoReview/sandbox/resume across layers", {
		env: { PI_CURSOR_AUTO_REVIEW: "1", PI_CURSOR_SANDBOX: "1" },
		session: { local: { autoReview: false } },
		project: { local: { resume: false, sandboxOptions: { enabled: true } } },
		user: { local: { autoReview: true, resume: true } },
	});
	add("local skip session layer", {
		session: { local: { autoReview: true, useHttp1ForAgent: true } },
		project: { local: { autoReview: false } },
	});
	add("local sandbox scalar vs sandboxOptions", {
		cli: { local: { sandbox: true } },
		env: { PI_CURSOR_SANDBOX: "0" },
		user: { local: { sandboxOptions: { enabled: true } } },
	});
	add("local force skips project and session", {
		session: { local: { force: true } },
		project: { local: { force: true } },
		env: { PI_CURSOR_LOCAL_FORCE: "1" },
	});
	add("local useHttp1 order session/env/user/builtin", {
		env: { PI_CURSOR_HTTP_1_1: "1" },
		user: { local: { useHttp1ForAgent: false } },
	});
	add("local storeRoot env beats user", {
		env: { PI_CURSOR_SDK_STATE_ROOT: "  /env/store  " },
		user: { local: { storeRoot: "/user/store" } },
	});
	add("subagents project over user", {
		project: { subagents: { alpha: { description: "pa", prompt: "pp" } } },
		user: { subagents: { alpha: { description: "ua", prompt: "up" }, beta: { description: "b", prompt: "p" } } },
	});
	add("subagents invalid entries dropped", {
		user: { subagents: { "1bad": { description: "d", prompt: "p" }, good: { description: "d", prompt: "p", thinking: "medium", fast: true } } },
	});
	add("fastDefaults merged", {
		project: { fastDefaults: { a: true, b: "x" } },
		user: { fastDefaults: { b: false, c: true } },
	});
	add("builtin cloud override merges", {
		builtIn: { cloud: { repo: "builtin/repo", directPush: true } },
	});
	return cases;
}

function parseConfigCases() {
	const raws = [
		{},
		null,
		[],
		{ runtime: "cloud", fastDefaults: { a: true, b: 1 }, subagents: {} },
		{ runtime: "spaceship", local: { storeRoot: "  ", autoReview: 1, sandbox: true, sandboxOptions: { enabled: false } } },
		{ cloud: { repo: "  r  ", branch: "", contextHandoff: "bootstrap", directPush: "yes", envNames: ["A", "", "CURSOR_X", "9b"], environment: { type: "machine", name: "n", extra: 1 }, acknowledged: true } },
		{ cloud: { envNames: [], environment: {} }, local: { resume: true, useHttp1ForAgent: false } },
		{ subagents: { ok: { description: "d", prompt: "p", model: "m", thinking: "off", fast: false }, bad: { description: "", prompt: "p" } } },
	];
	return raws.map((raw, index) => ({ name: `parse ${index}`, raw }));
}

function envConfigCases() {
	const envs = [
		{},
		{ PI_CURSOR_RUNTIME: " cloud ", PI_CURSOR_CLOUD_REPO: " r ", PI_CURSOR_CLOUD_BRANCH: " b " },
		{ PI_CURSOR_RUNTIME: "invalid", PI_CURSOR_CLOUD_CONTEXT: "nope" },
		{ PI_CURSOR_CLOUD_DIRECT_PUSH: "1", PI_CURSOR_CLOUD_AUTO_CREATE_PR: "off", PI_CURSOR_CLOUD_SKIP_REVIEWER_REQUEST: "yes" },
		{ PI_CURSOR_CLOUD_ENV: "A,B", PI_CURSOR_CLOUD_ENV_FROM_FILES: "1", PI_CURSOR_CLOUD_ACK: "true" },
		{ PI_CURSOR_CLOUD_ENV_TYPE: "cloud", PI_CURSOR_CLOUD_ENV_NAME: " name " },
		{ PI_CURSOR_AUTO_REVIEW: "1", PI_CURSOR_SANDBOX: "0", PI_CURSOR_LOCAL_FORCE: "on", PI_CURSOR_LOCAL_RESUME: "no", PI_CURSOR_HTTP_1_1: "enabled" },
		{ PI_CURSOR_CLOUD_ENV: "CURSOR_ONLY,9BAD" },
	];
	return envs.map((env, index) => ({ name: `env ${index}`, env }));
}

function mergeCases() {
	return [
		{ name: "merge empty", base: {}, patch: {} },
		{ name: "merge nested", base: { cloud: { repo: "r", directPush: true }, local: { sandboxOptions: { enabled: true } }, keep: 1 }, patch: { cloud: { branch: "b" }, local: { sandboxOptions: { enabled: false }, resume: true } } },
		{ name: "merge preserves unrelated", base: { custom: { x: 1 } }, patch: { runtime: "cloud" } },
	].map((entry) => entry);
}

function fastDefaultMatrix() {
	const cases = [];
	for (const cliForceNoFast of [false, true]) {
		for (const cliForceFast of [false, true]) {
			for (const aliasOverride of [undefined, false, true]) {
				for (const sessionValue of [undefined, false, true]) {
					for (const userValue of [undefined, false, true]) {
						for (const modelDefault of [false, true]) {
							cases.push({
								name: `fast noFast=${cliForceNoFast} fast=${cliForceFast} alias=${aliasOverride} session=${sessionValue} user=${userValue} default=${modelDefault}`,
								options: { cliForceNoFast, cliForceFast, aliasOverride, sessionValue, userValue, modelDefault },
							});
						}
					}
				}
			}
		}
	}
	return cases;
}

// ---------------------------------------------------------------- provenance

function provenanceManager() {
	const assistant = (id, tokens, stopReason, api, provider, model) => ({
		role: "assistant", api, provider, model, stopReason,
		usage: { input: 1, output: 1, cacheRead: 0, cacheWrite: 0, totalTokens: tokens, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } },
		content: [{ type: "text", text: id }], timestamp: 1,
	});
	const branch = [
		{ id: "e1", type: "message" },
		{ id: "e2", type: "compaction" },
		{ id: "e3", type: "message" },
		{ id: "e4", type: "message" },
	];
	const projection = {
		entries: [
			{ sourceEntry: { id: "e1" }, messages: [assistant("before", 900, "stop", "cursor-sdk", "cursor", "composer-2.5")] },
			{ sourceEntry: { id: "e2" }, messages: [] },
			{ sourceEntry: { id: "e3" }, messages: [assistant("after-a", 500, "stop", "cursor-sdk", "cursor", "composer-2.5")] },
			{ sourceEntry: { id: "e4" }, messages: [assistant("after-b", 700, "stop", "cursor-sdk", "cursor", "composer-2.5")] },
		],
		messages: [
			{ role: "system", content: "sys", timestamp: 0 },
			{ role: "user", content: "hi", timestamp: 1 },
			assistant("after-b", 700, "stop", "cursor-sdk", "cursor", "composer-2.5"),
		],
	};
	return { getBranch: () => branch, buildSessionProjection: () => projection };
}

function provenanceCases(provenance, manager) {
	const cases = [];
	const snapshot = provenance.captureCursorRequestProjection(manager);
	cases.push({ name: "capture snapshot", value: snapshot });
	const context = { messages: manager.buildSessionProjection().messages };
	const model = (overrides) => ({ id: "composer-2.5", name: "Composer", api: "cursor-sdk", provider: "cursor", contextWindow: 1000, ...overrides });
	cases.push({ name: "resolve normal matching", value: provenance.resolveCursorRequestProvenance(snapshot, model({}), context, "normal") });
	cases.push({ name: "resolve normal window excludes all", value: provenance.resolveCursorRequestProvenance(snapshot, model({ contextWindow: 100 }), context, "normal") });
	cases.push({ name: "resolve normal picks later within window", value: provenance.resolveCursorRequestProvenance(snapshot, model({ contextWindow: 600 }), context, "normal") });
	cases.push({ name: "resolve normal model mismatch", value: provenance.resolveCursorRequestProvenance(snapshot, model({ id: "other" }), context, "normal") });
	cases.push({ name: "resolve compaction skips occupancy", value: provenance.resolveCursorRequestProvenance(snapshot, model({}), context, "compaction") });
	cases.push({ name: "resolve tree skips occupancy", value: provenance.resolveCursorRequestProvenance(snapshot, model({}), context, "tree") });
	cases.push({ name: "resolve digest mismatch", value: provenance.resolveCursorRequestProvenance(snapshot, model({}), { messages: [{ role: "user", content: "changed", timestamp: 2 }] }, "normal") });
	cases.push({ name: "digest is property-order stable", value: provenance.captureCursorRequestProjection({
		getBranch: () => [],
		buildSessionProjection: () => ({ entries: [], messages: [{ timestamp: 1, content: "x", role: "user" }] }),
	}).digest });
	return cases;
}

// ---------------------------------------------------------------- extension identity

function fakePi() {
	const record = {
		commands: [],
		flags: [],
		handlers: [],
		tools: [],
		providers: [],
		entries: [],
	};
	return {
		record,
		registerCommand: (name, options) => record.commands.push({ name, description: options.description }),
		registerFlag: (name, options) => record.flags.push({ name, description: options.description, type: options.type, default: options.default }),
		registerTool: (tool) => record.tools.push(tool.name),
		registerProvider: (provider) => record.providers.push({
			id: provider.id,
			name: provider.name,
			baseUrl: provider.baseUrl,
			authName: provider.auth?.apiKey?.name,
			authKind: provider.auth?.apiKey ? "api_key" : undefined,
		}),
		on: (name) => record.handlers.push(name),
		appendEntry: (kind) => record.entries.push(kind),
		getFlag: () => undefined,
		getActiveTools: () => [],
		getAllTools: () => [],
		setActiveTools: () => {},
		events: { on: () => () => {}, emit: () => {} },
	};
}

async function extensionIdentity(runtime) {
	const pi = fakePi();
	await runtime.entry.default(pi);
	const record = pi.record;
	return {
		commands: record.commands,
		flags: record.flags,
		handlers: [...new Set(record.handlers)].sort(),
		tools: [...record.tools].sort(),
		providers: record.providers,
	};
}

// ---------------------------------------------------------------- trust and loaders

/**
 * Config-file trust/loader/atomic-update behavior. Each runtime writes under its
 * own temporary directory (inside $HOME, never /tmp) and the emitted values carry
 * `<root>` instead of the per-run path so the fixture is byte-stable.
 */
function trustCases(config, root) {
	const cases = [];
	const normalize = (value) =>
		typeof value === "string" ? value.split(root).join("<root>") : value;
	const attempt = (name, operation) => {
		try {
			cases.push({ name, value: operation() });
		} catch (error) {
			cases.push({ name, value: { error: normalize(error.message) } });
		}
	};
	const seedFile = (path, text) => {
		mkdirSync(dirname(path), { recursive: true });
		writeFileSync(path, text);
		return path;
	};

	attempt("user config missing", () =>
		config.loadCursorSdkUserConfig(config.getCursorSdkUserConfigPath(join(root, "missing", "agent"))));
	attempt("user config valid", () =>
		config.loadCursorSdkUserConfig(seedFile(
			config.getCursorSdkUserConfigPath(join(root, "valid", "agent")),
			JSON.stringify({ runtime: "cloud", local: { resume: false } }),
		)));
	attempt("user config invalid JSON", () =>
		config.loadCursorSdkUserConfig(seedFile(
			config.getCursorSdkUserConfigPath(join(root, "invalid", "agent")),
			"{not json",
		)));
	attempt("user config non-object", () =>
		config.loadCursorSdkUserConfig(seedFile(
			config.getCursorSdkUserConfigPath(join(root, "array", "agent")),
			"[]",
		)));
	attempt("project config untrusted", () => {
		const cwd = join(root, "untrusted", "work");
		seedFile(config.getCursorSdkProjectConfigPath(cwd), JSON.stringify({ runtime: "cloud" }));
		return config.loadCursorSdkProjectConfig(cwd, false);
	});
	attempt("project config trusted", () => {
		const cwd = join(root, "trusted", "work");
		seedFile(config.getCursorSdkProjectConfigPath(cwd), JSON.stringify({ runtime: "cloud", cloud: { repo: "r" } }));
		return config.loadCursorSdkProjectConfig(cwd, true);
	});
	attempt("project config trusted missing file", () =>
		config.loadCursorSdkProjectConfig(join(root, "no-file", "work"), true));
	attempt("load config untrusted", () =>
		config.loadCursorSdkConfig({
			cwd: join(root, "load-untrusted", "work"),
			projectTrusted: false,
			agentDir: join(root, "load-untrusted", "agent"),
		}));
	attempt("load config trusted", () => {
		const agentDir = join(root, "load-trusted", "agent");
		const cwd = join(root, "load-trusted", "work");
		seedFile(config.getCursorSdkUserConfigPath(agentDir), JSON.stringify({ local: { autoReview: true } }));
		seedFile(config.getCursorSdkProjectConfigPath(cwd), JSON.stringify({ runtime: "cloud" }));
		return config.loadCursorSdkConfig({ cwd, projectTrusted: true, agentDir });
	});
	attempt("update creates private file", () => {
		const path = config.getCursorSdkUserConfigPath(join(root, "update-new", "agent"));
		const updated = config.updateCursorSdkConfig(path, (current) => ({ ...current, runtime: "cloud" }), { newFileMode: 0o600 });
		return { updated, content: readFileSync(path, "utf8"), mode: statSync(path).mode & 0o777 };
	});
	attempt("update preserves existing mode", () => {
		const path = config.getCursorSdkUserConfigPath(join(root, "update-existing", "agent"));
		seedFile(path, JSON.stringify({ keep: true }));
		chmodSync(path, 0o640);
		const updated = config.updateCursorSdkConfig(path, (current) => config.mergeCursorSdkConfigForUpdate(current, { local: { resume: false } }));
		return { updated, content: readFileSync(path, "utf8"), mode: statSync(path).mode & 0o777 };
	});
	attempt("update rejects invalid JSON", () => {
		const path = config.getCursorSdkUserConfigPath(join(root, "update-invalid", "agent"));
		seedFile(path, "{broken");
		return config.updateCursorSdkConfig(path, (current) => current);
	});
	return cases;
}

// ---------------------------------------------------------------- per-runtime compute

function computeRuntime(runtime, root) {
	const { config, settingSources, envBoolean, apiKey, provenance, shared } = runtime;
	const groups = {};
	const push = (group, name, value) => (groups[group] ??= []).push({ name, value });

	for (const scenario of configPrecedenceCases()) {
		let value;
		try {
			value = config.resolveCursorSdkConfig(scenario.options);
		} catch (error) {
			value = { error: error.message };
		}
		push("config-precedence", scenario.name, value);
	}
	push("config-precedence", "cli invalid runtime throws", (() => {
		try { return { value: config.resolveCursorSdkConfig({ cli: { runtime: "spaceship" } }) }; }
		catch (error) { return { error: error.message }; }
	})());
	push("config-precedence", "cli invalid context throws", (() => {
		try { return { value: config.resolveCursorSdkConfig({ cli: { cloud: { contextHandoff: "later" } } }) }; }
		catch (error) { return { error: error.message }; }
	})());
	for (const scenario of parseConfigCases()) {
		let value;
		try { value = config.parseCursorSdkConfig(scenario.raw); } catch (error) { value = { error: error.message }; }
		push("config-parse", scenario.name, value);
	}
	for (const scenario of envConfigCases()) {
		let value;
		try { value = config.cursorSdkConfigFromEnv(scenario.env); } catch (error) { value = { error: error.message }; }
		push("config-env", scenario.name, value);
	}
	push("config-env", "invalid envNames throws", (() => {
		try { return { value: config.cursorSdkConfigFromEnv({ PI_CURSOR_CLOUD_ENV: "CURSOR_X,9BAD" }) }; }
		catch (error) { return { error: error.message }; }
	})());
	for (const scenario of mergeCases()) {
		push("config-merge", scenario.name, config.mergeCursorSdkConfigForUpdate(scenario.base, scenario.patch));
	}
	push("config-merge", "fastDefaults map", Object.fromEntries(config.cursorFastDefaultsFromConfig({ fastDefaults: { b: false, a: true } })));
	for (const scenario of fastDefaultMatrix()) {
		push("fast-default", scenario.name, config.resolveCursorFastDefault(scenario.options));
	}
	for (const [name, options] of [
		["store root empty", {}],
		["store root env", { env: { PI_CURSOR_SDK_STATE_ROOT: " /env " } }],
		["store root env blank falls to user", { env: { PI_CURSOR_SDK_STATE_ROOT: "   " }, user: { local: { storeRoot: " /user " } } }],
		["store root user", { user: { local: { storeRoot: " /user " } } }],
		["store root user blank is undefined", { user: { local: { storeRoot: "   " } } }],
	]) {
		push("store-root", name, config.resolveCursorLocalStoreRoot(options));
	}
	for (const [name, cwd, dirName] of [
		["project path default", "/work/project", undefined],
		["project path explicit", "/work/project", ".pi"],
	]) {
		push("config-paths", name, dirName === undefined ? config.getCursorSdkProjectConfigPath(cwd) : config.getCursorSdkProjectConfigPath(cwd, dirName));
	}
	push("config-paths", "user path explicit", config.getCursorSdkUserConfigPath("/home/u/.pi"));
	push("config-paths", "config file name", config.CURSOR_SDK_CONFIG_FILE);

	for (const scenario of trustCases(config, join(root, "trust"))) {
		push("config-trust", scenario.name, scenario.value);
	}

	for (const value of ["local", "cloud", "spaceship", "", undefined, null, 1]) {
		push("config-guards", `isCursorRuntime ${JSON.stringify(value)}`, config.isCursorRuntime(value));
	}
	for (const value of ["never", "fresh", "bootstrap", "later"]) {
		push("config-guards", `isCursorCloudContextHandoff ${JSON.stringify(value)}`, config.isCursorCloudContextHandoff(value));
	}
	for (const value of ["cloud", "pool", "machine", "later"]) {
		push("config-guards", `isCursorCloudEnvironmentType ${JSON.stringify(value)}`, config.isCursorCloudEnvironmentType(value));
	}
	for (const value of ["A", "A,B", " A , B ", "CURSOR_X", "9BAD", "", "A,,B"]) {
		let outcome;
		try {
			outcome = { value: config.parseExplicitCursorCloudEnvNames(value, "PI_CURSOR_CLOUD_ENV") };
		} catch (error) {
			outcome = { error: error.message };
		}
		push("config-guards", `parseExplicitCursorCloudEnvNames ${JSON.stringify(value)}`, outcome);
	}

	// Exported constant identity: the environment names that flags/config aliases use.
	const constants = {};
	for (const [name, value] of Object.entries(config)) {
		if (typeof value === "string" && (name.startsWith("CURSOR_") || name.startsWith("PI_"))) constants[name] = value;
	}
	for (const [name, value] of Object.entries(settingSources)) {
		if (typeof value === "string" && (name.startsWith("CURSOR_") || name.startsWith("PI_"))) constants[name] = value;
	}
	constants.CURSOR_API_KEY_ENV_VAR = apiKey.CURSOR_API_KEY_ENV_VAR;
	constants.CURSOR_SETTING_SOURCES_ENV_SHARED = shared.CURSOR_SETTING_SOURCES_ENV;
	push("config-constants", "exported environment names", constants);

	for (const raw of SETTING_SOURCE_INPUTS) {
		push("setting-sources", `resolve ${JSON.stringify(raw)}`, {
			resolved: settingSources.resolveCursorSettingSources(raw),
			effective: settingSources.getEffectiveCursorSettingSources(raw),
			serialized: shared.serializeCursorSettingSources(settingSources.resolveCursorSettingSources(raw)),
			user: settingSources.cursorSettingSourcesIncludes(settingSources.resolveCursorSettingSources(raw), "user"),
			project: settingSources.cursorSettingSourcesIncludes(settingSources.resolveCursorSettingSources(raw), "project"),
		});
	}
	push("setting-sources", "default frozen", shared.DEFAULT_CURSOR_SETTING_SOURCES);
	push("setting-sources", "env name", settingSources.CURSOR_SETTING_SOURCES_ENV);

	for (const raw of ENV_BOOL_INPUTS) {
		push("env-boolean", `parse ${JSON.stringify(raw)}`, {
			optional: envBoolean.parseOptionalEnvBoolean(raw),
			withDefaultTrue: envBoolean.parseEnvBoolean(raw, true),
			withDefaultFalse: envBoolean.parseEnvBoolean(raw, false),
		});
	}

	const savedKey = process.env.CURSOR_API_KEY;
	process.env.CURSOR_API_KEY = "env-fixture-key";
	try {
		for (const raw of API_KEY_INPUTS) {
			push("api-key", `normalize ${JSON.stringify(raw)}`, {
				normalize: apiKey.normalizeCursorApiKey(raw),
				resolve: apiKey.resolveCursorApiKey(raw),
			});
		}
	} finally {
		if (savedKey === undefined) delete process.env.CURSOR_API_KEY;
		else process.env.CURSOR_API_KEY = savedKey;
	}
	push("api-key", "env var name", apiKey.CURSOR_API_KEY_ENV_VAR);

	const manager = provenanceManager();
	for (const scenario of provenanceCases(provenance, manager)) {
		push("provenance", scenario.name, scenario.value);
	}

	return groups;
}

// ---------------------------------------------------------------- run and compare

const reference = await loadRuntime("reference");
const candidate = await loadRuntime("candidate");

const referenceRoot = mkdtempSync(join(homedir(), ".rpi-c-cmd-reference-"));
const candidateRoot = mkdtempSync(join(homedir(), ".rpi-c-cmd-candidate-"));
let referenceGroups;
let candidateGroups;
let determinism;
try {
	referenceGroups = computeRuntime(reference, referenceRoot);
	candidateGroups = computeRuntime(candidate, candidateRoot);

	const groups = [...new Set([...Object.keys(referenceGroups), ...Object.keys(candidateGroups)])].sort();
	const cases = [];
	let failures = 0;
	for (const group of groups) {
		const referenceCases = referenceGroups[group] ?? [];
		const candidateCases = candidateGroups[group] ?? [];
		const names = [...new Set([...referenceCases.map((entry) => entry.name), ...candidateCases.map((entry) => entry.name)])];
		assert.equal(referenceCases.length, candidateCases.length, `${group} case count`);
		for (const name of names) {
			const ref = referenceCases.find((entry) => entry.name === name)?.value;
			const cand = candidateCases.find((entry) => entry.name === name)?.value;
			const status = stable(ref) === stable(cand) ? "pass" : "fail";
			if (status === "fail") failures += 1;
			cases.push({ group, name, status, reference: ref ?? null, candidate: cand ?? null });
		}
	}

	// Command/flag/provider identity from the extension entry point.
	const referenceIdentity = await extensionIdentity(reference);
	const candidateIdentity = await extensionIdentity(candidate);
	const identityStatus = stable(referenceIdentity) === stable(candidateIdentity) ? "pass" : "fail";
	if (identityStatus === "fail") failures += 1;
	cases.push({ group: "extension-identity", name: "commands/flags/handlers/tools/provider", status: identityStatus, reference: referenceIdentity, candidate: candidateIdentity });

	// Determinism: a second compute must be byte-identical for both runtimes.
	determinism = {
		reference: stable(computeRuntime(reference, join(referenceRoot, "again"))) === stable(referenceGroups),
		candidate: stable(computeRuntime(candidate, join(candidateRoot, "again"))) === stable(candidateGroups),
	};

	const counts = {};
	for (const entry of cases) counts[entry.group] = (counts[entry.group] ?? 0) + 1;

	// Versions actually resolved by the two module graphs (the pinned adapter's own
	// dependency set), read from the installed package metadata.
	const readVersion = (path) => JSON.parse(readFileSync(path, "utf8")).version;
	const piCodingAgentVersion = readVersion(join(REFERENCE_ROOT, "node_modules/@earendil-works/pi-coding-agent/package.json"));
	const candidatePiVersion = readVersion(join(PACKAGE_ROOT, "worker/node_modules/@earendil-works/pi-coding-agent/package.json"));
	assert.equal(piCodingAgentVersion, candidatePiVersion, "Pi package version differs between graphs");
	const piAiVersion = readVersion(join(REFERENCE_ROOT, "node_modules/@earendil-works/pi-ai/package.json"));
	const cursorSdkVersion = readVersion(join(REFERENCE_ROOT, "node_modules/@cursor/sdk/package.json"));
	const piCursorSdkVersion = readVersion(join(REFERENCE_ROOT, "package.json"));

	const evidence = {
		status: failures === 0 && determinism.reference && determinism.candidate ? "pass" : "fail",
		schema: "rpi-cursor-c-cmd-evidence/1",
		runtime: { node: process.versions.node },
		provenance: {
			piCodingAgentVersion,
			piAiVersion,
			cursorSdkVersion,
			piCursorSdkVersion,
			referenceRoot: "provider-references/cursor",
			upstream: upstreamHashes,
			port: portHashes,
		},
		determinism,
		counts,
		cases,
		notTested: [
			"Interactive command handlers and their UI notifications (only registration identity is compared).",
			"Config-file locking, atomic replace and concurrent writers (owned by the store lanes).",
			"Native DLL registration and the packaged worker (owned by the install/host lanes).",
			"Live Cursor SDK inference, cloud execution and real credentials.",
			"macOS/Windows execution.",
		],
	};

	const serialized = `${stable(evidence)}\n`;
	if (WRITE) {
		writeFileSync(EVIDENCE_PATH, serialized);
		console.log(`wrote ${EVIDENCE_PATH} (${cases.length} cases, status ${evidence.status})`);
	} else {
		const committed = readFileSync(EVIDENCE_PATH, "utf8");
		if (committed !== serialized) {
			console.error("c_cmd-evidence.json differs from a fresh reference run");
			const committedJson = JSON.parse(committed);
			for (const entry of cases) {
				const previous = committedJson.cases.find((candidateEntry) => candidateEntry.group === entry.group && candidateEntry.name === entry.name);
				if (!previous || stable(previous) !== stable(entry)) {
					console.error(`  case ${entry.group} / ${entry.name}: ${previous ? "changed" : "missing"}`);
				}
			}
			process.exitCode = 1;
		} else {
			console.log(`c_cmd reference check passed (${cases.length} cases, status ${evidence.status})`);
		}
	}
	if (failures !== 0) {
		console.error(`${failures} cases disagree between the unchanged upstream and the retained worker port`);
		process.exitCode = 1;
	}
} finally {
	rmSync(referenceRoot, { recursive: true, force: true });
	rmSync(candidateRoot, { recursive: true, force: true });
	rmSync(SANDBOX, { recursive: true, force: true });
}
