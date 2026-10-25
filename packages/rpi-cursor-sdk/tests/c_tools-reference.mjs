#!/usr/bin/env node
// Differential tool-scenario reference for ledger C05/H08.
//
// Runs the SAME scripted tool scenario through two runtimes and compares exact
// execution counts:
//   reference: unchanged Pi 1.3.0 + the pinned pi-cursor-sdk@0.5.3 adapter, driven
//              by the unchanged fake @cursor/sdk@1.0.37 scenario harness;
//   candidate: the rpi retained worker (packaged runtime/main.mjs) driven through
//              its real peer protocol by a scripted rpi host, with the same fake
//              SDK injected.
//
// The fake SDK harness and its fixtures live in the sibling c-fake lane
// (packages/rpi-cursor-sdk/tests/fake-sdk) and are never edited here. After the
// c-fake lane is integrated they are read from tests/fake-sdk.
//
//   node tests/c_tools-reference.mjs             check mode (default)
//   node tests/c_tools-reference.mjs --write     regenerate evidence.json
//   node tests/c_tools-reference.mjs --only NAME run one scenario
//   node tests/c_tools-reference.mjs --keep DIR  keep raw runs under DIR
//
// Never touches ~/.pi, ~/.rpi or the network: HOME, TMPDIR and every fake path
// live inside a temporary directory under $HOME and are removed afterwards.
import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const PACKAGE_ROOT = resolve(HERE, "..");
const SCENARIO_DIR = join(HERE, "c_tools/scenarios");
const EVIDENCE_PATH = join(HERE, "c_tools-evidence.json");
const WORKER_ENTRY = join(PACKAGE_ROOT, "runtime/main.mjs");
// Lane worktrees sit directly under the Projects directory.
const REF_ROOT = resolve(process.env.RPI_PROVIDER_REFERENCES ?? join(HERE, "../../../../provider-references"), "integrated");
const PI_DIR = join(REF_ROOT, "node_modules/@earendil-works/pi-coding-agent");
const PI_CLI = join(PI_DIR, "dist/bundle/cli.js");
const PINNED_ADAPTER = join(REF_ROOT, "node_modules/pi-cursor-sdk");
// The fake SDK is tracked at tests/fake-sdk; RPI_CURSOR_FAKE_SDK overrides it and the
// historical c-fake lane path stays as a last resort for older checkouts.
const FAKE_DIR = process.env.RPI_CURSOR_FAKE_SDK
	? resolve(process.env.RPI_CURSOR_FAKE_SDK)
	: existsSync(join(HERE, "fake-sdk"))
		? join(HERE, "fake-sdk")
		: resolve(HERE, "../../../../rpi-package--c-fake/packages/rpi-cursor-sdk/tests/fake-sdk");
const REGISTER = join(FAKE_DIR, "register.mjs");
const REPLAY_ACTIVITY_TOOL = "cursor";
const RECORDED_TOOL_NAMES = new Set(["read", "bash", "edit", "write", "grep", "find", "ls", REPLAY_ACTIVITY_TOOL]);
const TIMEOUT_MS = 180_000;

const args = process.argv.slice(2);
const WRITE = args.includes("--write");
const KEEP = args.includes("--keep") ? resolve(args[args.indexOf("--keep") + 1]) : undefined;
const ONLY = args.includes("--only") ? args[args.indexOf("--only") + 1] : undefined;

const sha256 = (text) => createHash("sha256").update(text).digest("hex");
// Scenario extensions resolve against the fake SDK fixtures first, then this lane's
// own fixtures under tests/c_tools/fixtures.
function resolveExtension(extension) {
	const fake = join(FAKE_DIR, extension);
	return existsSync(fake) ? fake : join(HERE, extension);
}
const sha256File = (path) => sha256(readFileSync(path));

function sortObject(object) {
	return Object.fromEntries(Object.entries(object).sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0)));
}
function readJsonLines(text) {
	return text.split("\n").filter((line) => line.length > 0);
}
// The fake log is not normalized beyond paths that embed per-run identifiers:
// the temporary root and the rpi per-session SDK store hash derived from it.
function normalizeFakeLog(text, tmpRoot) {
	return text
		.split(tmpRoot).join("<root>")
		.replace(/stores\/[0-9a-f]{64}/g, "stores/<store>")
		.replace(/pi-sessions\/[0-9a-f]{32}/g, "pi-sessions/<scope>");
}

// ---------------------------------------------------------------- shared counts

function fakeRecords(text) {
	return readJsonLines(text).map((line) => JSON.parse(line));
}
/** The three ledger counts plus the host tool surface, derived from fake-log records only. */
function ledgerCountsFromFake(records) {
	const nativeToolCalls = {};
	const hostFromBridge = {};
	let bridgeToolExecutions = 0;
	let bridgeToolFailures = 0;
	let violations = 0;
	for (const record of records) {
		if (record.kind === "native_tool" && record.phase === "started") {
			nativeToolCalls[record.name] = (nativeToolCalls[record.name] ?? 0) + 1;
		}
		if (record.kind === "tool_execution" && record.source === "bridge") {
			// A bridge execution is an MCP tools/call that returned; `failed` marks a
			// transport/JSON-RPC failure, `isError` a tool-level error result.
			bridgeToolExecutions += 1;
			if (record.failed !== undefined || record.isError === true) bridgeToolFailures += 1;
			const name = String(record.tool).replace(/^pi__/, "");
			hostFromBridge[name] = (hostFromBridge[name] ?? 0) + 1;
		}
		if (record.kind === "violation") violations += 1;
	}
	return { nativeToolCalls: sortObject(nativeToolCalls), bridgeToolExecutions, bridgeToolFailures, violations, bridgeTools: sortObject(hostFromBridge) };
}

// ---------------------------------------------------------------- reference run

function normalizeJsonLine(line, tmpRoot) {
	const value = JSON.parse(line);
	const walk = (node, key) => {
		if (Array.isArray(node)) return node.map((item) => walk(item));
		if (node && typeof node === "object") {
			const out = {};
			for (const [name, child] of Object.entries(node)) out[name] = walk(child, name);
			return out;
		}
		if (typeof node === "number" && (key === "timestamp" || key === "durationMs" || key === "executionTime")) return 0;
		if (typeof node === "string") {
			if (key === "id" || key === "parentId" || key === "anchorId") {
				if (/^[0-9a-f]{8}$/.test(node)) return "<entry>";
			}
			return node
				.split(tmpRoot).join("<root>")
				.replace(/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/g, "<uuid>")
				.replace(/\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z/g, "<iso>")
				// Session file names carry the same clock in a dash-separated form.
				.replace(/\d{4}-\d{2}-\d{2}T\d{2}-\d{2}-\d{2}-\d{3}Z/g, "<iso>")
				// The retained adapter derives its on-disk store id from the fake agent.
				.replace(/(pi-sessions\/)[0-9a-f]{32}/g, "$1<store>")
				.replace(/cursor-replay-\d+-\d+-/g, "cursor-replay-<n>-<n>-")
				.replace(/cursor-pi-bridge-[0-9a-f]{32}-t\d+/g, "cursor-pi-bridge-<run>-t<n>");
		}
		return node;
	};
	return JSON.stringify(walk(value));
}
function summarizePiStream(stdout) {
	const events = readJsonLines(stdout).map((line) => JSON.parse(line));
	const counts = {};
	const invocations = {};
	const replayExecutions = {};
	for (const event of events) {
		counts[event.type] = (counts[event.type] ?? 0) + 1;
		if (event.type === "tool_execution_start") invocations[event.toolName] = (invocations[event.toolName] ?? 0) + 1;
		if (event.type === "tool_execution_end" && event.toolName === REPLAY_ACTIVITY_TOOL) {
			replayExecutions.performedWork = (replayExecutions.performedWork ?? 0) + (event.isError === true ? 0 : 1);
		}
	}
	return { eventCounts: sortObject(counts), invocations: sortObject(invocations), replayWork: replayExecutions.performedWork ?? 0 };
}

function runPi(scenario, scenarioPath) {
	const tmpRoot = mkdtempSync(join(homedir(), ".rpi-c-tools-"));
	const workspace = join(tmpRoot, "workspace");
	const home = join(tmpRoot, "home");
	const agentDir = join(tmpRoot, "agent");
	const tmpDir = join(tmpRoot, "tmp");
	const logPath = join(tmpRoot, "fake.jsonl");
	for (const dir of [workspace, home, agentDir, tmpDir]) mkdirSync(dir, { recursive: true });
	const extensionArgs = [];
	for (const extension of scenario.run.extensions ?? []) extensionArgs.push("-e", resolveExtension(extension));
	const argv = [
		PI_CLI, "-ne", "-e", PINNED_ADAPTER, ...extensionArgs,
		"--provider", "cursor", "--model", scenario.run.model, "--mode", "json",
		"--session-dir", join(tmpRoot, "sessions"),
		...(scenario.run.args ?? []),
		scenario.run.prompt,
		...(scenario.run.followUps ?? []),
	];
	const env = {
		PATH: `${dirname(process.execPath)}:/usr/local/bin:/usr/bin:/bin`,
		HOME: home, TMPDIR: tmpDir, LC_ALL: "en_US.UTF-8", TERM: "dumb",
		PI_CODING_AGENT_DIR: agentDir, CURSOR_API_KEY: "fake",
		NODE_OPTIONS: `--import ${REGISTER}`,
		RPI_CURSOR_FAKE_SCENARIO: scenarioPath, RPI_CURSOR_FAKE_LOG: logPath,
		RPI_CURSOR_FAKE_LOG_ROOT: tmpRoot, RPI_CURSOR_FAKE_HOME: join(tmpRoot, "cursor-home"),
	};
	return new Promise((resolvePromise, reject) => {
		const child = spawn(process.execPath, argv, { cwd: workspace, env, stdio: ["ignore", "pipe", "pipe"] });
		let stdout = "";
		let stderr = "";
		child.stdout.setEncoding("utf8").on("data", (chunk) => { stdout += chunk; });
		child.stderr.setEncoding("utf8").on("data", (chunk) => { stderr += chunk; });
		const timer = setTimeout(() => child.kill("SIGKILL"), TIMEOUT_MS);
		child.on("error", reject);
		child.on("close", (code) => {
			clearTimeout(timer);
			const fakeLog = existsSync(logPath) ? readFileSync(logPath, "utf8") : "";
			resolvePromise({ code, stdout, stderr, fakeLog, tmpRoot, cleanup: () => rmSync(tmpRoot, { recursive: true, force: true }) });
		});
	});
}

async function runReference(scenarioPath) {
	const scenario = JSON.parse(readFileSync(scenarioPath, "utf8"));
	const run = await runPi(scenario, scenarioPath);
	try {
		const records = fakeRecords(run.fakeLog);
		const ledger = ledgerCountsFromFake(records);
		const pi = summarizePiStream(run.stdout);
		const invocations = sortObject({ ...ledger.bridgeTools, ...pi.invocations });
		const observed = {
			nativeToolCalls: ledger.nativeToolCalls,
			bridgeToolExecutions: ledger.bridgeToolExecutions,
			bridgeToolFailures: ledger.bridgeToolFailures,
			hostToolInvocations: invocations,
			replayToolExecutions: pi.replayWork,
			violations: ledger.violations,
		};
		return {
			observed,
			exitCode: run.code,
			fakeLogRecords: records.length,
			fakeLogSha256: sha256(normalizeFakeLog(run.fakeLog, run.tmpRoot)),
			fakeLogRaw: normalizeFakeLog(run.fakeLog, run.tmpRoot),
			normalizedPiSha256: sha256(`${readJsonLines(run.stdout).map((line) => normalizeJsonLine(line, run.tmpRoot)).join("\n")}\n`),
			piEventCounts: pi.eventCounts,
			stderrLines: run.stderr.split("\n").filter(Boolean).length,
		};
	} finally {
		run.cleanup();
	}
}

// ---------------------------------------------------------------- candidate run

async function runCandidate(scenarioPath) {
	const { start } = await import("../worker/test/rpi/peer-harness.mjs");
	const scenario = JSON.parse(readFileSync(scenarioPath, "utf8"));
	const tmpRoot = mkdtempSync(join(homedir(), ".rpi-c-tools-"));
	const cwd = join(tmpRoot, "workspace");
	mkdirSync(cwd, { recursive: true });
	const logPath = join(tmpRoot, "fake.jsonl");
	process.env.NODE_OPTIONS = `--import ${REGISTER}`;
	process.env.RPI_CURSOR_FAKE_SCENARIO = scenarioPath;
	process.env.RPI_CURSOR_FAKE_LOG = logPath;
	process.env.RPI_CURSOR_FAKE_LOG_ROOT = tmpRoot;
	process.env.RPI_CURSOR_FAKE_HOME = join(tmpRoot, "cursor-home");
	process.env.CURSOR_API_KEY = "fake";

	const cleanups = [];
	const peer = await start({ after: (fn) => cleanups.push(fn) }, { entry: new URL(`file://${WORKER_ENTRY}`), cwd });
	try {
		let next = 0;
		const begin = (method, input) => { const id = `h${++next}`; peer.send({ id, kind: "request", method, input }); return id; };
		const requestOnce = async (method, input) => {
			const id = begin(method, input);
			const frame = await peer.take((f) => f.id === id && ["reply", "error"].includes(f.kind));
			if (frame.kind === "error") throw new Error(`${method}: ${frame.error}`);
			return frame.result;
		};

		const open = begin("runtime.open", { cwd, agentDir: join(tmpRoot, "agent"), sessionDir: join(tmpRoot, "sessions") });
		const ready = await peer.take((f) => f.id === open);
		if (ready.kind !== "event" || ready.event.type !== "ready") throw new Error(`worker did not open: ${JSON.stringify(ready).slice(0, 300)}`);
		peer.send({ id: open, kind: "credit", sequence: ready.sequence });
		const manifest = ready.event.manifest;

		const extensionFixtures = (scenario.run.extensions ?? []).map((extension) => extension.split("/").pop());
		const hostTools = [
			...manifest.tools,
			{ name: "fake_echo", description: "Echo", parameters: { type: "object", properties: { text: { type: "string" } }, required: ["text"] }, sourceInfo: { source: "extension", path: "fixture", scope: "temporary", origin: "top-level" } },
			...(extensionFixtures.includes("failing-extension.mjs") ? [{ name: "fake_fail", description: "Always throws", parameters: { type: "object", properties: { text: { type: "string" } }, required: ["text"] }, sourceInfo: { source: "extension", path: "fixture", scope: "temporary", origin: "top-level" } }] : []),
			...["read", "bash", "edit", "write"].map((name) => ({ name, description: name, parameters: { type: "object" }, sourceInfo: { source: "builtin", path: name, scope: "temporary", origin: "top-level" } })),
		];
		let active = hostTools.map((tool) => tool.name);
		const { baseUrl: _base, headers: _headers, ...publicModel } = manifest.provider.models[0];
		const session = { metadata: { id: "fixture-session", createdAt: 1000 }, revision: 0, name: null, labels: {}, entries: [], branches: [{ lane: "main", revision: 0, leafId: null }] };
		const origins = new Set();
		let originNumber = 0;
		let revision = 0;
		let operation = null;
		let toolSelection = { revision: "0", state: null };
		const guards = new Map();
		const recordedBindings = {};
		const recordedPrefixes = {};
		const executed = [];
		const hostInvocations = {};
		const journals = { cloud: Buffer.alloc(0), usage: Buffer.alloc(0) };
		const readers = new Map();
		let readerNumber = 0;
		const journal = (input) => {
			const kind = input.kind;
			if (input.action === "open") { const reader = `r${++readerNumber}`; readers.set(reader, kind); return { reader, size: journals[kind].length }; }
			if (input.action === "next") { const buffer = journals[readers.get(input.reader)]; return { bytes: buffer.toString("base64"), digest: createHash("sha256").update(buffer).digest("hex") }; }
			if (input.action === "close") { readers.delete(input.reader); return null; }
			if (input.action === "append") { journals[kind] = Buffer.concat([journals[kind], Buffer.from(input.bytes, "base64")]); return { committed: true, durability: "durable", size: journals[kind].length }; }
			throw new Error(`unexpected journal action ${input.action}`);
		};
		const view = () => ({ snapshot: structuredClone(session), leafId: session.branches[0].leafId, branchRevision: 0,
			projection: { model: null, thinkingLevel: "off", entries: session.entries.map((entry) => ({ sourceEntry: entry, messages: [] })) } });
		async function host(method, input) {
			if (method === "scoped") { if (!origins.has(input.scope)) throw new Error("scoped call without an origin"); return host(input.method, input.input); }
			if (method === "journals") return journal(input);
			switch (input.port) {
				case "context":
					if (input.action === "flags") return { "cursor-no-fast": true };
					if (input.action !== "snapshot") throw new Error(`unexpected context action ${input.action}`);
					return { cwd, mode: "json", hasUI: false, projectTrusted: true, projectTrustResolved: true,
						toolRegistration: { disabled: false, allow: null, deny: [] }, toolSelection: structuredClone(toolSelection),
						isIdle: operation === null, hasPendingMessages: false, systemPrompt: "native prompt",
						sessionId: session.metadata.id, model: publicModel, tools: [...active], identity: operation };
				case "tools":
					if (input.action === "publish") {
						for (const tool of input.tools) {
							if (tool.recordedCallPrefix) { guards.set(tool.name, { prefix: tool.recordedCallPrefix, error: tool.recordedError }); recordedPrefixes[tool.name] = tool.recordedCallPrefix; }
							if (tool.binding) recordedBindings[tool.name] = tool.binding.kind;
							const index = hostTools.findIndex((item) => item.name === tool.name);
							const metadata = { ...tool, ownedBinding: true, exposure: "direct", sourceInfo: { source: "extension", path: "cursor", scope: "temporary", origin: "top-level" } };
							if (index < 0) { hostTools.push(metadata); active.push(tool.name); } else hostTools[index] = metadata;
						}
						toolSelection = { revision: String(BigInt(toolSelection.revision) + 1n), state: null };
						return { registered: input.tools.map((tool) => tool.name), tools: [...active], selection: structuredClone(toolSelection) };
					}
					if (input.action === "setActive") { active = [...input.tools]; toolSelection = { revision: String(BigInt(toolSelection.revision) + 1n), state: structuredClone(input.state) }; return { tools: [...active], selection: structuredClone(toolSelection) }; }
					if (input.action === "inspect") return hostTools;
					throw new Error(`unexpected tools action ${input.action}`);
				case "auth": return { apiKey: "fake", source: "fixture" };
				case "ui": return { ok: true };
				case "providers":
					if (input.action === "publish") return { revision: ++revision };
					if (input.action === "refresh") return { revision };
					throw new Error(`unexpected providers action ${input.action}`);
				case "sessions":
					if (input.action === "capture") { const origin = `origin-${++originNumber}`; origins.add(origin); return { origin, snapshot: structuredClone(session) }; }
					switch (input.action) {
						case "location": return { path: join(tmpRoot, "sessions", "fixture.jsonl") };
						case "view": return view();
						case "snapshot": return structuredClone(session);
						case "sync": return { durability: "durable" };
						case "release": origins.delete(input.origin); return null;
						case "append": case "journalAppend": {
							const entry = { type: "custom", id: `c-${++session.revision}`, seq: session.revision, parentId: session.branches[0].leafId,
								timestamp: 1000 + session.revision, customType: input.customType, ...(Object.hasOwn(input, "data") ? { data: input.data } : {}) };
							session.entries.push(entry); session.branches[0].leafId = entry.id;
							return { entry, revision: session.revision, durability: "durable", advancedLane: true };
						}
					}
					throw new Error(`unexpected sessions action ${input.action}`);
			}
			throw new Error(`unexpected host request ${method}/${input.port}/${input.action}`);
		}
		const failures = [];
		let pumpError;
		const pump = async () => {
			for (;;) {
				const request = await peer.take((frame) => frame.kind === "request");
				void host(request.method, request.input).then(
					(result) => peer.send({ id: request.id, kind: "reply", result: result ?? null, lastEvent: 0 }),
					(error) => { failures.push(error.message); peer.send({ id: request.id, kind: "error", error: error.message, lastEvent: 0 }); },
				);
			}
		};
		// The pump outlives individual awaits; a dead worker must surface as this
		// scenario's own failure, not as an unhandled rejection that hides it.
		pump().catch((error) => { pumpError = error.message; process.stderr.write(`worker pump stopped: ${error.message}\n`); });
		await requestOnce("runtime.configure", {});
		await requestOnce("extension.event", { event: { type: "session_start", reason: "startup" } });

		const contextTools = [...active].map((name) => ({ name, description: name, parameters: { type: "object" } }));
		const messages = [{ role: "user", content: scenario.run.prompt, timestamp: 1 }];
		for (let turn = 0; turn < 8; turn++) {
			operation = { operationId: `op-${turn}`, purpose: "prompt" };
			const id = begin("provider.stream", { entry: "stream", model: manifest.provider.models[0], context: { messages, tools: contextTools }, options: { apiKey: "fake", env: {} }, authMode: "replace" });
			const events = [];
			for (;;) {
				const frame = await peer.take((f) => f.id === id);
				if (frame.kind === "event") { events.push(frame.event); peer.send({ id, kind: "credit", sequence: frame.sequence }); }
				else { if (frame.kind === "error") throw new Error(`provider.stream: ${frame.error}`); break; }
			}
			const calls = events.filter((event) => event.type === "toolcall_end").map((event) => event.toolCall);
			const done = events.filter((event) => event.type === "done").at(-1);
			if (calls.length === 0 || done?.reason !== "toolUse") break;
			messages.push({ role: "assistant", content: calls.map((call) => ({ type: "toolCall", id: call.id, name: call.name, arguments: call.arguments })),
				api: publicModel.api, provider: publicModel.provider, model: publicModel.id,
				usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } },
				stopReason: "toolUse", timestamp: 2 });
			for (const call of calls) {
				hostInvocations[call.name] = (hostInvocations[call.name] ?? 0) + 1;
				const guard = guards.get(call.name);
				// The rpi host refuses a recorded replay id instead of executing the tool.
				if (guard && call.id.startsWith(guard.prefix)) {
					messages.push({ role: "toolResult", toolCallId: call.id, toolName: call.name, content: [{ type: "text", text: guard.error }], isError: true, timestamp: 3 });
					continue;
				}
				executed.push(call.name);
				const fails = call.name === "fake_fail";
				messages.push({ role: "toolResult", toolCallId: call.id, toolName: call.name,
					content: [{ type: "text", text: fails ? "fake_fail: intentional bridge failure" : call.name === "fake_echo" ? "fake_echo:bridge-check" : "simulated\n" }], isError: fails, timestamp: 3 });
			}
		}
		await requestOnce("extension.event", { event: { type: "session_shutdown", reason: "quit" } });
		const fakeLog = existsSync(logPath) ? readFileSync(logPath, "utf8") : "";
		const records = fakeRecords(fakeLog);
		const ledger = ledgerCountsFromFake(records);
		// Recorded activity never executes work: a replay-guarded invocation is inert.
		const replayWork = executed.filter((name) => RECORDED_TOOL_NAMES.has(name)).length;
		const observed = {
			nativeToolCalls: ledger.nativeToolCalls,
			bridgeToolExecutions: ledger.bridgeToolExecutions,
			bridgeToolFailures: ledger.bridgeToolFailures,
			hostToolInvocations: sortObject(hostInvocations),
			replayToolExecutions: replayWork,
			violations: ledger.violations,
		};
		return {
			observed,
			exitCode: failures.length === 0 ? 0 : 1,
			fakeLogRecords: records.length,
			fakeLogSha256: sha256(normalizeFakeLog(fakeLog, tmpRoot)),
			fakeLogRaw: normalizeFakeLog(fakeLog, tmpRoot),
			hostFailures: [...failures, ...(pumpError ? [`worker pump: ${pumpError}`] : [])],
			workerOutputLines: peer.output().split("\n").filter(Boolean).length,
			recordedBindings: sortObject(recordedBindings),
			recordedPrefixes: sortObject(recordedPrefixes),
		};
	} finally {
		for (const fn of cleanups) await fn();
		rmSync(tmpRoot, { recursive: true, force: true });
	}
}

// ---------------------------------------------------------------- comparison

// Stream hashes embed per-run identifiers; the compared values are the counts,
// expectations, determinism and status. Hashes stay in the evidence as provenance.
function stableScenario(result) {
	const { referenceDetail, candidateDetail, ...stable } = result;
	return {
		...stable,
		referenceDetail: referenceDetail && { exitCode: referenceDetail.exitCode, fakeLogRecords: referenceDetail.fakeLogRecords, stderrLines: referenceDetail.stderrLines },
		candidateDetail: candidateDetail && { exitCode: candidateDetail.exitCode, fakeLogRecords: candidateDetail.fakeLogRecords, hostFailures: candidateDetail.hostFailures, workerOutputLines: candidateDetail.workerOutputLines, recordedBindings: candidateDetail.recordedBindings, recordedPrefixes: candidateDetail.recordedPrefixes },
	};
}

function compareExpect(expect, observed) {
	const problems = [];
	const check = (label, want, got) => {
		if (JSON.stringify(want) !== JSON.stringify(got)) problems.push(`${label}: expected ${JSON.stringify(want)}, observed ${JSON.stringify(got)}`);
	};
	check("nativeToolCalls", expect.nativeToolCalls ?? {}, observed.nativeToolCalls);
	check("bridgeToolExecutions", expect.bridgeToolExecutions ?? 0, observed.bridgeToolExecutions);
	check("bridgeToolFailures", expect.bridgeToolFailures ?? 0, observed.bridgeToolFailures);
	check("hostToolInvocations", expect.hostToolInvocations ?? {}, observed.hostToolInvocations);
	check("replayToolExecutions", expect.replayToolExecutions ?? 0, observed.replayToolExecutions);
	check("violations", expect.violations ?? 0, observed.violations);
	return problems;
}

async function runScenario(file) {
	const scenarioPath = join(SCENARIO_DIR, file);
	const scenario = JSON.parse(readFileSync(scenarioPath, "utf8"));
	const sides = {};
	const attempts = {};
	for (const side of ["reference", "candidate"]) {
		attempts[side] = [];
		for (let attempt = 0; attempt < 2; attempt++) {
			const result = side === "reference" ? await runReference(scenarioPath) : await runCandidate(scenarioPath);
			attempts[side].push(result);
			if (KEEP) {
				const dir = join(KEEP, scenario.name, `${side}-${attempt + 1}`);
				mkdirSync(dir, { recursive: true });
				writeFileSync(join(dir, "observed.json"), `${JSON.stringify({ ...result, fakeLogRaw: undefined }, null, 2)}\n`);
				writeFileSync(join(dir, "fake.jsonl"), result.fakeLogRaw ?? "");
			}
		}
		const [first, second] = attempts[side];
		sides[side] = first;
		if (JSON.stringify(first.observed) !== JSON.stringify(second.observed)) {
			sides[`${side}Determinism`] = false;
		} else {
			sides[`${side}Determinism`] = true;
		}
	}
	const problems = [];
	for (const [side, deterministic] of [["reference", sides.referenceDeterminism], ["candidate", sides.candidateDeterminism]]) {
		if (!deterministic) problems.push(`${side} counts differ between two runs`);
	}
	if (JSON.stringify(sides.reference.observed) !== JSON.stringify(sides.candidate.observed)) {
		problems.push(`reference and candidate counts differ: ${JSON.stringify(sides.reference.observed)} vs ${JSON.stringify(sides.candidate.observed)}`);
	}
	problems.push(...compareExpect(scenario.cToolsExpect, sides.reference.observed));
	return {
		name: scenario.name,
		scenarioFile: `tests/c_tools/scenarios/${file}`,
		scenarioSha256: sha256File(scenarioPath),
		expect: scenario.cToolsExpect,
		reference: sides.reference.observed,
		candidate: sides.candidate.observed,
		determinism: { reference: sides.referenceDeterminism, candidate: sides.candidateDeterminism },
		referenceDetail: { exitCode: sides.reference.exitCode, fakeLogRecords: sides.reference.fakeLogRecords, fakeLogSha256: sides.reference.fakeLogSha256, normalizedPiSha256: sides.reference.normalizedPiSha256, stderrLines: sides.reference.stderrLines },
		candidateDetail: { exitCode: sides.candidate.exitCode, fakeLogRecords: sides.candidate.fakeLogRecords, fakeLogSha256: sides.candidate.fakeLogSha256, hostFailures: sides.candidate.hostFailures, workerOutputLines: sides.candidate.workerOutputLines, recordedBindings: sides.candidate.recordedBindings, recordedPrefixes: sides.candidate.recordedPrefixes },
		problems,
		status: problems.length === 0 ? "pass" : "fail",
	};
}

function provenance() {
	const files = {
		"@cursor/sdk/package.json": join(REF_ROOT, "node_modules/@cursor/sdk/package.json"),
		"@earendil-works/pi-coding-agent/package.json": join(PI_DIR, "package.json"),
		"pi-cursor-sdk/package.json": join(PINNED_ADAPTER, "package.json"),
		"integrated/package-lock.json": join(REF_ROOT, "package-lock.json"),
		"rpi-worker/runtime/main.mjs": WORKER_ENTRY,
		"rpi-worker/runtime/dist/rpi/runtime.js": join(PACKAGE_ROOT, "runtime/dist/rpi/runtime.js"),
		"rpi-worker/runtime/dist/rpi/tool-surface.js": join(PACKAGE_ROOT, "runtime/dist/rpi/tool-surface.js"),
	};
	const out = {};
	for (const [name, path] of Object.entries(files)) {
		if (!existsSync(path)) throw new Error(`required file missing: ${path}`);
		out[name] = sha256File(path);
	}
	const read = (path) => JSON.parse(readFileSync(path, "utf8")).version;
	const fakeFiles = ["sdk.mjs", "hooks.mjs", "register.mjs", "sqlite.mjs", "fixtures/echo-extension.mjs"];
	const fake = Object.fromEntries(fakeFiles.map((name) => [name, sha256File(join(FAKE_DIR, name))]));
	return {
		files: out,
		piVersion: read(join(PI_DIR, "package.json")),
		cursorSdkVersion: read(join(REF_ROOT, "node_modules/@cursor/sdk/package.json")),
		piCursorSdkVersion: read(join(PINNED_ADAPTER, "package.json")),
		fakeSdkDir: FAKE_DIR.split(homedir()).join("<home>"),
		fake,
	};
}

async function main() {
	if (!existsSync(PI_CLI)) throw new Error(`Pi CLI missing at ${PI_CLI}`);
	if (!existsSync(REGISTER)) throw new Error(`fake SDK harness missing at ${FAKE_DIR} (set RPI_CURSOR_FAKE_SDK; the tracked copy is tests/fake-sdk)`);
	if (!existsSync(join(PACKAGE_ROOT, "runtime/dist/rpi/runtime.js"))) throw new Error("staged worker assets missing; run `scripts/ensure-runtime-deps.sh --worker`, then `npm run build` in worker/ and `node tests/stage-runtime.mjs`");
	const node = process.versions.node;
	if (Number(node.split(".")[0]) < 24) throw new Error(`Node 24+ required, found ${node}`);
	const files = readdirSync(SCENARIO_DIR).filter((name) => name.endsWith(".json")).sort();
	const selected = ONLY ? files.filter((name) => name === `${ONLY}.json`) : files;
	if (selected.length === 0) throw new Error("no scenarios selected");
	const results = [];
	for (const file of selected) {
		process.stdout.write(`running ${file} ...\n`);
		results.push(await runScenario(file));
	}
	const evidence = {
		status: results.every((result) => result.status === "pass") ? "pass" : "fail",
		schema: "rpi-cursor-c-tools-evidence/1",
		purpose: "Differential tool scenarios for ledger C05/H08: unchanged Pi 1.3.0 + pinned pi-cursor-sdk@0.5.3 adapter versus the rpi retained worker (packaged runtime/main.mjs) driven through its real peer protocol, both against the unchanged fake @cursor/sdk@1.0.37. No Cursor network access.",
		runtime: { node, platform: process.platform, arch: process.arch },
		provenance: provenance(),
		normalization: {
			referencePiJson: "Timestamps/durations zeroed, UUIDs to <uuid>, ISO times to <iso> (including the dash-separated session file stamp), derived pi-sessions/<hex> store ids to <store>, cursor-replay-<ms>-<n>- to cursor-replay-<n>-<n>-, bridge run IDs to cursor-pi-bridge-<run>-t<n>, temporary root to <root>. Used only for the recorded hashes; counts are not normalized.",
			fakeLog: "No normalization beyond the temporary root; the fake writes no clocks, PIDs or random IDs. Counts are read from fake-log records, which flush immediately (unlike process_counters, which need a clean exit).",
			counts: "nativeToolCalls, bridgeToolExecutions and violations come from fake-log records on both sides. hostToolInvocations are the tool names the host was asked to run (reference: Pi tool_execution_start; candidate: worker toolcall_end events). replayToolExecutions is the number of recorded-activity invocations that performed work (must be zero).",
		},
		scenarios: results,
		notTested: [
			"Live Cursor inference, billing and cloud resources.",
			"The rpi host binary itself is not run; the candidate side drives the packaged worker through a scripted rpi host over the real peer protocol. Real host tool invocation through the host's own hooks/context/cancellation is therefore exercised at the worker boundary, not through the compiled host.",
			"Interactive/TUI rendering of replay cards and the visual transcript.",
			"Windows/macOS worker startup.",
		],
		commands: [
			"scripts/ensure-runtime-deps.sh --worker   (offline npm ci for runtime/ and worker/; needs a populated npm cache)",
			"cd worker && npm run build",
			"node tests/stage-runtime.mjs",
			"node tests/c_tools-reference.mjs        (check mode)",
			"node tests/c_tools-reference.mjs --write",
		],
	};
	const text = `${JSON.stringify(evidence, null, 2)}\n`;
	if (WRITE) {
		writeFileSync(EVIDENCE_PATH, text);
		process.stdout.write(`wrote ${EVIDENCE_PATH} (${evidence.status})\n`);
	} else {
		if (!existsSync(EVIDENCE_PATH)) throw new Error("c_tools-evidence.json missing; run with --write");
		const committed = JSON.parse(readFileSync(EVIDENCE_PATH, "utf8"));
		const diffs = [];
		if (committed.status !== evidence.status) diffs.push("status");
		for (const result of results) {
			const old = committed.scenarios?.find((candidate) => candidate.name === result.name);
			if (!old) diffs.push(`scenario ${result.name} missing from evidence.json`);
			else if (JSON.stringify(stableScenario(old)) !== JSON.stringify(stableScenario(result))) diffs.push(`scenario ${result.name} differs from evidence.json`);
		}
		if (JSON.stringify(committed.provenance) !== JSON.stringify(evidence.provenance)) diffs.push("provenance");
		for (const result of results) {
			process.stdout.write(`${result.status.toUpperCase()} ${result.name}: reference=${JSON.stringify(result.reference)} candidate=${JSON.stringify(result.candidate)}\n`);
			for (const problem of result.problems) process.stdout.write(`  - ${problem}\n`);
		}
		if (diffs.length > 0) {
			process.stdout.write(`c_tools-evidence.json is stale: ${diffs.join(", ")}\n`);
			process.exitCode = 1;
		}
	}
	for (const result of results) if (result.status !== "pass") process.exitCode = 1;
}

main().catch((error) => {
	process.stderr.write(`${error?.stack ?? error}\n`);
	process.exitCode = 1;
});
