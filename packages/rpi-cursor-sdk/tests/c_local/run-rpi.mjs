#!/usr/bin/env node
// Drives the retained rpi Cursor worker (runtime/main.mjs) through its private
// peer protocol with an emulated native host, using the scenario-driven fake
// @cursor/sdk. For every scenario in scenarios/ it records the fake SDK call
// counters and the worker's provider.stream outcome, then compares those to the
// unchanged Pi 1.3.0 reference recorded by run-reference.mjs.
//
//   node run-rpi.mjs            check mode (default): compare with evidence.json
//   node run-rpi.mjs --write    regenerate evidence.json
//   node run-rpi.mjs --keep DIR keep each run's raw fake log and stream events
//   node run-rpi.mjs --only NAME run one scenario
//
// Never touches ~/.pi or ~/.rpi: HOME, TMPDIR and every fake path live in a
// temporary directory created under $HOME and removed afterwards.
import { spawn } from "node:child_process";
import { createHash, randomBytes } from "node:crypto";
import { once } from "node:events";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { createServer } from "node:net";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const PACKAGE_ROOT = resolve(HERE, "../..");
const WORKER_ENTRY = join(PACKAGE_ROOT, "runtime/main.mjs");
const SCENARIO_DIR = join(HERE, "scenarios");
const REGISTER = join(HERE, "fake-sdk/register.mjs");
const EVIDENCE_PATH = join(HERE, "evidence.json");
const REFERENCE_EVIDENCE_PATH = join(HERE, "reference-evidence.json");
const TIMEOUT_MS = 120_000;

const args = process.argv.slice(2);
const WRITE = args.includes("--write");
const KEEP = args.includes("--keep") ? resolve(args[args.indexOf("--keep") + 1]) : undefined;
const ONLY = args.includes("--only") ? args[args.indexOf("--only") + 1] : undefined;

const sha256 = (text) => createHash("sha256").update(text).digest("hex");
const sha256File = (path) => sha256(readFileSync(path));
const API_KEY = process.env.CURSOR_API_KEY || "offline-fixture";
const DEBUG = process.env.RPI_C_LOCAL_DEBUG === "1";
const debug = (...parts) => { if (DEBUG) process.stderr.write(`[c-local] ${parts.join(" ")}\n`); };

function encodeFrame(value) {
	const json = JSON.stringify(value);
	const size = Buffer.byteLength(json);
	const frame = Buffer.allocUnsafe(size + 4);
	frame.writeUInt32BE(size, 0);
	frame.write(json, 4);
	return frame;
}

// ---------------------------------------------------------------- peer client

function connectWorker({ cwd, env }) {
	return new Promise((resolvePromise, reject) => {
		const server = createServer();
		server.listen(0, "127.0.0.1", () => {
			const config = { version: 1, port: server.address().port, generation: randomBytes(16).toString("hex"), token: randomBytes(32).toString("hex") };
			const connection = once(server, "connection");
			const child = spawn(process.execPath, [WORKER_ENTRY], { cwd, env, stdio: ["pipe", "pipe", "pipe"] });
			let output = "";
			child.stdout.on("data", (chunk) => { output += chunk; });
			child.stderr.on("data", (chunk) => { output += chunk; if (DEBUG) process.stderr.write(chunk); });
			const exited = once(child, "exit");
			child.stdin.end(JSON.stringify(config));
			let authenticated = () => {};
			const authenticatedPromise = new Promise((resolve) => { authenticated = resolve; });
			const timer = setTimeout(() => reject(new Error(`worker did not connect within 20s:\n${output}`)), 20_000);
			exited.then(() => { clearTimeout(timer); reject(new Error(`worker exited before connecting:\n${output}`)); });
			connection.then(([socket]) => {
				clearTimeout(timer);
				const frames = [];
				const waiters = [];
				let hello = false;
				let prefix = Buffer.alloc(0);
				socket.on("error", (error) => { debug("socket error", error.message); });
				socket.on("data", (chunk) => {
					let buffer = Buffer.concat([prefix, chunk]);
					for (;;) {
						if (buffer.length < 4) break;
						const size = buffer.readUInt32BE(0);
						if (buffer.length < size + 4) break;
						const frame = JSON.parse(buffer.subarray(4, size + 4).toString("utf8"));
						buffer = buffer.subarray(size + 4);
						if (!hello) {
							hello = true;
							debug("handshake frame", JSON.stringify(frame).slice(0, 120));
							socket.write(encodeFrame({ version: 1, generation: config.generation, ready: true }));
							authenticated();
							continue;
						}
						debug("recv", JSON.stringify(frame).slice(0, 160));
						frames.push(frame);
						for (const wake of waiters.splice(0)) wake();
					}
					prefix = buffer;
				});
				const send = (frame) => { debug("send", JSON.stringify(frame).slice(0, 120)); socket.write(encodeFrame({ version: 1, generation: config.generation, ...frame })); };
				const take = async (predicate) => {
					for (;;) {
						const index = frames.findIndex(predicate);
						if (index >= 0) return frames.splice(index, 1)[0];
						await new Promise((wake) => waiters.push(wake));
					}
				};
				authenticatedPromise.then(() => resolvePromise({
					send, take, config, output: () => output, exited,
					close: async () => {
						socket.end();
						socket.destroy();
						server.close();
						await Promise.race([exited, new Promise((wake) => setTimeout(wake, 5000).unref?.())]);
						if (child.exitCode === null) child.kill();
						await exited;
					},
				}), reject);
			}, reject);
		});
	});
}

// ---------------------------------------------------------------- emulated host

function createHost({ root, cwd, mode, model, session, sessionFile }) {
	const { baseUrl: _baseUrl, headers: _headers, ...publicModel } = model;
	const trace = [];
	const origins = new Set();
	const failures = [];
	const readers = new Map();
	let originNumber = 0;
	let revision = 0;
	let toolSelection = { revision: "0", state: null };
	let active = [];
	const journalFile = (kind) => {
		const digest = createHash("sha256").update(session.metadata.id).digest("hex").slice(0, 32);
		return join(dirname(sessionFile), kind === "cloud" ? `.cursor-cloud-lifecycle-${digest}.journal` : `cursor-usage-${digest}.journal`);
	};
	const journals = (input) => {
		switch (input.action) {
			case "open": {
				const path = journalFile(input.kind);
				const bytes = existsSync(path) ? readFileSync(path) : Buffer.alloc(0);
				const reader = randomBytes(16).toString("hex");
				readers.set(reader, { bytes, offset: 0, hash: createHash("sha256") });
				return { reader, size: bytes.length };
			}
			case "next": {
				const state = readers.get(input.reader);
				if (!state) throw new Error("unknown native journal reader");
				const count = Math.min(state.bytes.length - state.offset, 1024 * 1024);
				const chunk = state.bytes.subarray(state.offset, state.offset + count);
				state.hash.update(chunk);
				state.offset += count;
				const done = state.offset === state.bytes.length;
				if (done) readers.delete(input.reader);
				return { bytes: chunk.toString("base64"), digest: done ? state.hash.digest("hex") : null };
			}
			case "close": readers.delete(input.reader); return null;
			case "append": {
				if ((input.kind === "usage") !== (input.expectedDigest !== undefined)) throw new Error("invalid native journal append contract");
				const bytes = Buffer.from(input.bytes, "base64");
				const path = journalFile(input.kind);
				if (input.expectedDigest !== undefined) {
					const existing = existsSync(path) ? readFileSync(path) : Buffer.alloc(0);
					if (createHash("sha256").update(existing).digest("hex") !== input.expectedDigest) return { committed: false };
					writeFileSync(path, Buffer.concat([existing, bytes]), { mode: 0o600 });
					return { committed: true, size: existing.length + bytes.length, durability: "durable" };
				}
				const existing = existsSync(path) ? readFileSync(path) : Buffer.alloc(0);
				writeFileSync(path, Buffer.concat([existing, bytes]), { mode: 0o600 });
				return { committed: true, size: existing.length + bytes.length, durability: "durable" };
			}
			default: throw new Error(`Unexpected native journal action ${input.action}`);
		}
	};
	const view = () => ({
		snapshot: structuredClone(session), leafId: session.branches[0].leafId, branchRevision: 0,
		projection: { model: null, thinkingLevel: "off", entries: session.entries.map((entry) => ({ sourceEntry: entry, messages: entry.type === "message" ? [entry.message] : [] })) },
	});
	const host = {
		trace,
		failures,
		origins,
		setActive(names) { active = [...names]; },
		activeTools: () => [...active],
		async handle(method, input) {
			if (method === "scoped") {
				if (!origins.has(input.scope)) throw new Error("scoped call must retain its origin until release acknowledgement");
				if (input.method === "journals") return journals(input.input);
				return host.handle(input.method, input.input);
			}
			trace.push({ method, input: structuredClone(input) });
			if (method !== "native") throw new Error(`Unexpected host method ${method}`);
			switch (input.port) {
				case "context":
					if (input.action === "flags") return {};
					if (input.action === "snapshot") {
						return { cwd, mode, hasUI: false, projectTrusted: true, projectTrustResolved: true,
							toolRegistration: { disabled: false, allow: null, deny: [] }, toolSelection: structuredClone(toolSelection),
							isIdle: true, hasPendingMessages: false, systemPrompt: "native prompt", sessionId: session.metadata.id,
							model: publicModel, tools: [...active], identity: null };
					}
					return null;
				case "tools":
					if (input.action === "inspect") return [];
					if (input.action === "publish") {
						for (const tool of input.tools) if (!active.includes(tool.name)) active.push(tool.name);
						toolSelection = { revision: String(BigInt(toolSelection.revision) + 1n), state: null };
						return { registered: input.tools.map((tool) => tool.name), tools: [...active], selection: structuredClone(toolSelection) };
					}
					if (input.action === "setActive") {
						active = [...input.tools];
						toolSelection = { revision: String(BigInt(toolSelection.revision) + 1n), state: structuredClone(input.state) };
						return { tools: [...active], selection: structuredClone(toolSelection) };
					}
					return null;
				case "auth":
					return null;
				case "ui":
					return { ok: true };
				case "dialogs":
					return null;
				case "providers":
					if (input.action === "publish") return { revision: ++revision };
					if (input.action === "refresh") return { revision };
					return { revision };
				case "sessions":
					if (input.action === "capture") {
						const origin = `origin-${++originNumber}`;
						origins.add(origin);
						return { origin, snapshot: structuredClone(session) };
					}
					if (!origins.has(input.origin)) throw new Error("unknown native session origin");
					switch (input.action) {
						case "location": return { path: sessionFile };
						case "view": return view();
						case "snapshot": return structuredClone(session);
						case "sync": return { durability: "durable" };
						case "release": origins.delete(input.origin); return null;
						case "append": case "journalAppend": {
							const entry = { type: "custom", id: `committed-${++session.revision}`, seq: session.entries.length + 1,
								parentId: session.branches[0].leafId, timestamp: 1000 + session.revision, customType: input.customType,
								...(Object.hasOwn(input, "data") ? { data: input.data } : {}) };
							session.entries.push(entry); session.branches[0].leafId = entry.id;
							return { entry, revision: session.revision, durability: "durable", advancedLane: true };
						}
						default: throw new Error(`Unexpected native session action ${input.action}`);
					}
				default:
					throw new Error(`Unexpected native host port ${input.port}`);
			}
		},
	};
	return host;
}

// ---------------------------------------------------------------- scenario run

function appendUser(session, text) {
	const id = `user-${session.entries.length + 1}`;
	session.entries.push({ type: "message", id, seq: session.entries.length + 1, parentId: session.branches[0].leafId, timestamp: 1000 + session.entries.length, message: { kind: "user", role: "user", content: text, timestamp: 1000 + session.entries.length } });
	session.branches[0].leafId = id;
	return { role: "user", content: text, timestamp: 1000 + session.entries.length };
}

function appendAssistant(session, text, usage) {
	const id = `assistant-${session.entries.length + 1}`;
	const message = { kind: "assistant", role: "assistant", content: [{ type: "text", text }], timestamp: 1000 + session.entries.length,
		api: "cursor-sdk", provider: "cursor", model: "composer-2", stopReason: "stop",
		usage: usage ?? { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } } };
	session.entries.push({ type: "message", id, seq: session.entries.length + 1, parentId: session.branches[0].leafId, timestamp: 1000 + session.entries.length, message });
	session.branches[0].leafId = id;
	return { role: "assistant", content: [{ type: "text", text }], timestamp: 1000 + session.entries.length, usage: message.usage, api: "cursor-sdk", provider: "cursor", model: "composer-2", stopReason: "stop" };
}

async function runScenario(file) {
	const scenarioPath = join(SCENARIO_DIR, file);
	const scenario = JSON.parse(readFileSync(scenarioPath, "utf8"));
	const tmpRoot = mkdtempSync(join(homedir(), ".rpi-c-local-"));
	const workspace = join(tmpRoot, "workspace");
	const home = join(tmpRoot, "home");
	const agentDir = join(tmpRoot, "agent");
	const stateHome = join(tmpRoot, "cursor-home");
	const tmpDir = join(tmpRoot, "tmp");
	const logPath = join(tmpRoot, "fake.jsonl");
	const sessionsDir = join(tmpRoot, "sessions");
	for (const dir of [workspace, home, agentDir, stateHome, tmpDir, sessionsDir]) mkdirSync(dir, { recursive: true, mode: 0o700 });
	// User-scoped Cursor config, read by the retained adapter for subagents / settings.
	if (scenario.config) writeFileSync(join(agentDir, "cursor-sdk.json"), `${JSON.stringify(scenario.config, null, 2)}\n`);
	const env = {
		PATH: `${dirname(process.execPath)}:/usr/local/bin:/usr/bin:/bin`,
		HOME: home, TMPDIR: tmpDir, LC_ALL: "en_US.UTF-8", TERM: "xterm-256color",
		CURSOR_API_KEY: API_KEY,
		PI_CURSOR_SETTING_SOURCES: "all",
		NODE_OPTIONS: `--import ${REGISTER}`,
		RPI_CURSOR_FAKE_SCENARIO: scenarioPath,
		RPI_CURSOR_FAKE_LOG: logPath,
		RPI_CURSOR_FAKE_LOG_ROOT: tmpRoot,
		RPI_CURSOR_FAKE_HOME: stateHome,
		...(scenario.env ?? {}),
	};
	let peer;
	let events = [];
	const problems = [];
	try {
		peer = await connectWorker({ cwd: workspace, env });
		debug("connected");
		peer.exited.then(() => debug("worker exited:", peer.output().slice(-2000)));
		let next = 0;
		const begin = (method, input) => {
			const id = `h${++next}`;
			peer.send({ id, kind: "request", method, input });
			return { id, result: async () => {
				const frame = await peer.take((candidate) => candidate.id === id && ["reply", "error"].includes(candidate.kind));
				if (frame.kind === "error") throw new Error(frame.error);
				return frame.result;
			} };
		};
		const open = begin("runtime.open", { cwd: workspace, agentDir, sessionDir: sessionsDir });
		const ready = await peer.take((frame) => frame.id === open.id);
		debug("opened", JSON.stringify(ready).slice(0, 200));
		if (ready.kind !== "event" || ready.event.type !== "ready") throw new Error(`worker did not open: ${JSON.stringify(ready)}`);
		peer.send({ id: open.id, kind: "credit", sequence: ready.sequence });
		const manifest = ready.event.manifest;
		const model = manifest.provider.models.find((candidate) => candidate.id === scenario.run.model);
		if (!model) throw new Error(`scenario model ${scenario.run.model} is not in the worker manifest`);
		const session = { metadata: { id: "fixture-session", createdAt: 1000 }, revision: 0, name: null, labels: {}, entries: [], branches: [{ lane: "main", revision: 0, leafId: null }] };
		const sessionFile = join(sessionsDir, "fixture-session.jsonl");
		const host = createHost({ root: tmpRoot, cwd: workspace, mode: "print", model, session, sessionFile });
		host.setActive(manifest.tools.map((tool) => tool.name));
		// Reply to every worker->host callback until the peer closes.
		let pumping = true;
		void (async () => {
			while (pumping) {
				const request = await peer.take((frame) => frame.kind === "request");
				void host.handle(request.method, request.input).then(
					(result) => peer.send({ id: request.id, kind: "reply", result: result ?? null, lastEvent: 0 }),
					(error) => { host.failures.push(error.message); peer.send({ id: request.id, kind: "error", error: error.message, lastEvent: 0 }); },
				);
			}
		})().catch(() => {});
		await begin("runtime.configure", {}).result();
		debug("configured");
		await begin("extension.event", { event: { type: "session_start", reason: "startup" } }).result();
		debug("session_start");

		const prompts = [scenario.run.prompt, ...(scenario.run.followUps ?? [])];
		const messages = [];
		const sendStream = () => begin("provider.stream", {
			entry: "stream", authMode: "replace", model,
			context: { messages: messages.map((message) => ({ ...message })), systemPrompt: "native prompt" },
			options: { apiKey: API_KEY, env: {}, ...(scenario.streamOptions ?? {}) },
		});
		const collectStream = async (stream, index) => {
			const collected = [];
			if (scenario.cancelAfterMs !== undefined) {
				await new Promise((wake) => setTimeout(wake, scenario.cancelAfterMs));
				peer.send({ id: stream.id, kind: "cancel" });
			}
			for (;;) {
				const frame = await peer.take((candidate) => candidate.id === stream.id);
				if (frame.kind === "event") {
					collected.push(frame.event);
					peer.send({ id: stream.id, kind: "credit", sequence: frame.sequence });
				} else if (frame.kind === "reply") {
					events = collected;
					debug(`turn ${index} replied with ${collected.length} events`);
					return { collected, final: frame.result };
				} else {
					throw new Error(`provider.stream failed: ${frame.error}`);
				}
			}
		};
		if (scenario.concurrent) {
			// Issue every stream before collecting any, so the adapter's per-scope turn
			// queue and pooled-agent ownership are exercised with overlapping callers.
			const issued = [];
			for (let index = 0; index < scenario.turns.length; index++) {
				const prompt = prompts[index];
				if (prompt === undefined) throw new Error(`scenario ${scenario.name} has no prompt for turn ${index}`);
				messages.push(appendUser(session, prompt));
				issued.push({ stream: sendStream(), index });
			}
			for (const { stream, index } of issued) {
				const { final } = await collectStream(stream, index);
				debug(`turn ${index} final`, JSON.stringify(final).slice(0, 500));
				const text = final?.content?.filter((part) => part.type === "text").map((part) => part.text).join("") ?? "";
				messages.push(appendAssistant(session, text, final?.usage));
			}
		} else {
			for (let index = 0; index < scenario.turns.length; index++) {
				const prompt = prompts[index];
				if (prompt === undefined) throw new Error(`scenario ${scenario.name} has no prompt for turn ${index}`);
				messages.push(appendUser(session, prompt));
				debug(`turn ${index} sending`);
				const { collected, final } = await collectStream(sendStream(), index);
				const terminal = collected.filter((event) => event.type === "done" || event.type === "error").at(-1) ?? collected.at(-1);
				debug(`turn ${index} final`, JSON.stringify(terminal).slice(0, 500));
				const text = terminal?.message?.content?.filter((part) => part.type === "text").map((part) => part.text).join("") ?? "";
				messages.push(appendAssistant(session, text, terminal?.message?.usage ?? final?.usage));
			}
		}
		peer.send({ id: open.id, kind: "cancel" });
		await Promise.race([open.result().catch(() => {}), new Promise((wake) => setTimeout(wake, 5000).unref?.())]);
		const deadline = Date.now() + 5000;
		while (host.origins.size > 0 && Date.now() < deadline) await new Promise((wake) => setTimeout(wake, 20));
		pumping = false;
		await peer.close();
		if (host.failures.length) problems.push(`host failures: ${host.failures.join("; ")}`);
		if (host.origins.size !== 0) problems.push("native session origins were not released");
		if (KEEP) {
			const dir = join(KEEP, scenario.name);
			mkdirSync(dir, { recursive: true });
			writeFileSync(join(dir, "events.json"), `${JSON.stringify(events, null, 2)}\n`);
			writeFileSync(join(dir, "trace.json"), `${JSON.stringify(host.trace, null, 2)}\n`);
		}
	} catch (error) {
		problems.push(`run failed: ${error?.message ?? error}`);
	} finally {
		if (peer) await peer.close().catch(() => {});
	}
	const fakeLog = existsSync(logPath) ? readFileSync(logPath, "utf8") : "";
	const { totals } = aggregateFakeLog(fakeLog);
	rmSync(tmpRoot, { recursive: true, force: true });
	return { problems, totals, eventTypes: countEventTypes(events) };
}

function countEventTypes(events) {
	const counts = {};
	for (const event of events) counts[event.type] = (counts[event.type] ?? 0) + 1;
	return Object.fromEntries(Object.entries(counts).sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0)));
}

function aggregateFakeLog(logText) {
	const records = logText.split("\n").filter(Boolean).map((line) => JSON.parse(line));
	const totals = { sdkCalls: {}, runs: {}, bridgeToolExecutions: 0, toolExecutions: {}, nativeToolCalls: {}, violations: 0 };
	for (const record of records) {
		if (record.kind !== "process_counters") continue;
		const counters = record.counters;
		for (const [api, n] of Object.entries(counters.sdkCalls)) totals.sdkCalls[api] = (totals.sdkCalls[api] ?? 0) + n;
		for (const [status, n] of Object.entries(counters.runs)) {
			if (status === "cancel_requested") continue;
			totals.runs[status] = (totals.runs[status] ?? 0) + n;
		}
		for (const [name, n] of Object.entries(counters.toolExecutions)) { totals.toolExecutions[name] = (totals.toolExecutions[name] ?? 0) + n; totals.bridgeToolExecutions += n; }
		for (const [name, n] of Object.entries(counters.nativeToolCalls)) totals.nativeToolCalls[name] = (totals.nativeToolCalls[name] ?? 0) + n;
		totals.violations += counters.violations;
	}
	return { totals };
}

const sortObject = (object) => Object.fromEntries(Object.entries(object).sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0)));

function rpiProvenance() {
	const files = ["run-rpi.mjs", "run-reference.mjs", "fake-sdk/sdk.mjs", "fake-sdk/hooks.mjs", "fake-sdk/register.mjs", "fake-sdk/sqlite.mjs"];
	return Object.fromEntries(files.map((name) => [name, sha256File(join(HERE, name))]));
}

async function main() {
	if (!existsSync(WORKER_ENTRY)) throw new Error(`worker entry missing at ${WORKER_ENTRY}`);
	const nodeVersion = process.versions.node;
	if (Number(nodeVersion.split(".")[0]) < 24) throw new Error(`Node 24+ required, found ${nodeVersion}`);
	const files = readdirSync(SCENARIO_DIR).filter((name) => name.endsWith(".json")).sort();
	const selected = ONLY ? files.filter((name) => name === `${ONLY}.json`) : files;
	if (selected.length === 0) throw new Error("no scenarios selected");
	const results = [];
	for (const file of selected) {
		process.stdout.write(`running rpi ${file} ...\n`);
		const scenario = JSON.parse(readFileSync(join(SCENARIO_DIR, file), "utf8"));
		const withTimeout = (promise, label) => Promise.race([
			promise,
			new Promise((_, reject) => setTimeout(() => reject(new Error(`${label} timed out after ${TIMEOUT_MS}ms`)), TIMEOUT_MS).unref?.()),
		]);
		const first = await withTimeout(runScenario(file), `rpi ${file} run 1`);
		const second = await withTimeout(runScenario(file), `rpi ${file} run 2`);
		const observed = { sdkCalls: sortObject(first.totals.sdkCalls), runs: sortObject(first.totals.runs),
			bridgeToolExecutions: first.totals.bridgeToolExecutions, toolExecutions: sortObject(first.totals.toolExecutions),
			nativeToolCalls: sortObject(first.totals.nativeToolCalls), violations: first.totals.violations };
		const problems = [...first.problems];
		const want = scenario.expect ?? {};
		const check = (label, expected, got) => { if (JSON.stringify(expected) !== JSON.stringify(got)) problems.push(`${label}: expected ${JSON.stringify(expected)}, observed ${JSON.stringify(got)}`); };
		check("sdkCalls", sortObject(want.sdkCalls ?? {}), observed.sdkCalls);
		check("runs", sortObject(want.runs ?? {}), observed.runs);
		check("bridgeToolExecutions", want.bridgeToolExecutions ?? 0, observed.bridgeToolExecutions);
		check("toolExecutions", sortObject(want.toolExecutions ?? {}), observed.toolExecutions);
		check("nativeToolCalls", sortObject(want.nativeToolCalls ?? {}), observed.nativeToolCalls);
		check("violations", want.violations ?? 0, observed.violations);
		if (JSON.stringify(first.totals) !== JSON.stringify(second.totals)) problems.push("fake counters differ between two runs");
		// The rpi worker must produce the same observable SDK interactions as unchanged Pi 1.3.0.
		const reference = existsSync(REFERENCE_EVIDENCE_PATH) ? JSON.parse(readFileSync(REFERENCE_EVIDENCE_PATH, "utf8")) : undefined;
		const referenceScenario = scenario.rpiOnly ? undefined : reference?.scenarios?.find((candidate) => candidate.name === scenario.name);
		if (!scenario.rpiOnly) {
			if (!reference) problems.push("reference evidence is missing; run tests/c_local/run-reference.mjs --write");
			else if (!referenceScenario) problems.push(`reference evidence has no scenario ${scenario.name}`);
			else {
				const comparable = (value) => ({ sdkCalls: sortObject(value.sdkCalls ?? {}), runs: sortObject(value.runs ?? {}),
					bridgeToolExecutions: value.bridgeToolExecutions ?? 0, toolExecutions: sortObject(value.toolExecutions ?? {}),
					nativeToolCalls: sortObject(value.nativeToolCalls ?? {}), violations: value.violations ?? 0 });
				if (JSON.stringify(comparable(referenceScenario.observed)) !== JSON.stringify(comparable(observed))) {
					problems.push(`rpi observed differs from the Pi 1.3.0 reference: reference ${JSON.stringify(comparable(referenceScenario.observed))}, rpi ${JSON.stringify(comparable(observed))}`);
				}
			}
		}
		results.push({
			name: scenario.name,
			scenarioFile: `tests/c_local/scenarios/${file}`,
			scenarioSha256: sha256File(join(SCENARIO_DIR, file)),
			observed,
			...(referenceScenario ? { referenceObserved: referenceScenario.observed, referenceStatus: referenceScenario.status } : {}),
			eventTypes: first.eventTypes,
			problems,
			status: problems.length === 0 ? "pass" : "fail",
		});
	}
	const reference = existsSync(REFERENCE_EVIDENCE_PATH) ? JSON.parse(readFileSync(REFERENCE_EVIDENCE_PATH, "utf8")) : undefined;
	const evidence = {
		status: results.every((result) => result.status === "pass") ? "pass" : "fail",
		schema: "rpi-cursor-c-local-rpi-evidence/1",
		purpose: "Offline scenario runs of the retained rpi Cursor worker (private peer protocol) against the fake @cursor/sdk@1.0.37. No Cursor network calls.",
		runtime: { node: nodeVersion, platform: process.platform, arch: process.arch },
		...(reference ? { reference: reference.reference, referenceEvidenceSha256: sha256File(REFERENCE_EVIDENCE_PATH) } : {}),
		files: rpiProvenance(),
		scenarios: results,
		notTested: [
			"Image prompts (Pi @image.png attachments) are not compared.",
			"Cloud agents, tools/bridge/MCP execution and the cursor-usage command are outside C03.",
			"The retained worker is driven directly, not through the rpi host CLI/cdylib; end-to-end host ports remain to be wired.",
			"local-cancel records the SDK-level cancellation and the worker reply, not a full ordered abort trace.",
		],
		commands: ["node tests/c_local/run-rpi.mjs", "node tests/c_local/run-rpi.mjs --write"],
	};
	const text = `${JSON.stringify(evidence, null, 2)}\n`;
	if (WRITE) {
		writeFileSync(EVIDENCE_PATH, text);
		process.stdout.write(`wrote ${EVIDENCE_PATH} (${evidence.status})\n`);
	} else {
		if (!existsSync(EVIDENCE_PATH)) throw new Error("evidence.json missing; run with --write");
		const committed = JSON.parse(readFileSync(EVIDENCE_PATH, "utf8"));
		const regenerated = JSON.parse(text);
		const diffs = [];
		if (committed.status !== regenerated.status) diffs.push("status");
		for (const result of regenerated.scenarios) {
			const old = committed.scenarios?.find((candidate) => candidate.name === result.name);
			if (!old) diffs.push(`scenario ${result.name} missing from evidence.json`);
			else if (JSON.stringify(old) !== JSON.stringify(result)) diffs.push(`scenario ${result.name} differs from evidence.json`);
		}
		if (JSON.stringify(committed.files) !== JSON.stringify(regenerated.files)) diffs.push("file provenance");
		if (diffs.length > 0) { process.stdout.write(`evidence.json is stale: ${diffs.join(", ")}\n`); process.exitCode = 1; }
	}
	for (const result of results) {
		process.stdout.write(`${result.status.toUpperCase()} ${result.name}: ${JSON.stringify(result.observed.sdkCalls)} runs=${JSON.stringify(result.observed.runs)}\n`);
		for (const problem of result.problems) process.stdout.write(`  - ${problem}\n`);
		if (result.status !== "pass") process.exitCode = 1;
	}
}

main().catch((error) => { process.stderr.write(`${error?.stack ?? error}\n`); process.exit(1); });
