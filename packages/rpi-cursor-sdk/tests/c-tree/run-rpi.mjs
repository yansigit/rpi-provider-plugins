#!/usr/bin/env node
// Candidate runner for ledger C04.
//
// Drives the retained rpi Cursor worker (runtime/main.mjs) through its private
// peer protocol with an emulated native host, using the scenario-driven fake
// @cursor/sdk. The host answers the awaited `sessions` port authoritatively:
// every append returns a committed receipt, is verified against a fresh view,
// and is recorded with the exact entry id and durability. For every reference
// scenario the runner compares its normalized C04 entries and fake-SDK counters
// with reference-evidence.json.
//
//   node run-rpi.mjs            check mode (default): compare with evidence.json
//   node run-rpi.mjs --write    regenerate evidence.json
//   node run-rpi.mjs --keep DIR keep each run's raw trace/fake log
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
import { C04_TYPES, normalizeC04Entries } from "./normalize.mjs";

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
const DEBUG = process.env.RPI_C_TREE_DEBUG === "1";
const debug = (...parts) => { if (DEBUG) process.stderr.write(`[c-tree] ${parts.join(" ")}\n`); };

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

function createHost({ cwd, mode, model, session, sessionFile, options }) {
	const { baseUrl: _baseUrl, headers: _headers, ...publicModel } = model;
	const trace = [];
	const appends = [];
	const notifications = [];
	const origins = new Set();
	const failures = [];
	const readers = new Map();
	let originNumber = 0;
	let revision = 0;
	let toolSelection = { revision: "0", state: null };
	let active = [];
	let aborted = false;
	let identity = options.identity ? options.identity() : null;
	const faults = options.faults ?? {};
	let lateAnchor = null;
	let movedLeaf = null;
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
				const existing = existsSync(path) ? readFileSync(path) : Buffer.alloc(0);
				if (input.expectedDigest !== undefined && createHash("sha256").update(existing).digest("hex") !== input.expectedDigest) return { committed: false };
				writeFileSync(path, Buffer.concat([existing, bytes]), { mode: 0o600 });
				return { committed: true, size: existing.length + bytes.length, durability: "durable" };
			}
			default: throw new Error(`Unexpected native journal action ${input.action}`);
		}
	};
	const lane = () => session.branches[0];
	const view = () => ({
		snapshot: structuredClone(session), leafId: lane().leafId, branchRevision: lane().revision,
		projection: {
			model: null, thinkingLevel: "off",
			entries: session.entries.map((entry) => ({ sourceEntry: entry, messages: entry.type === "message" ? [entry.message] : [] })),
		},
	});
	const host = {
		trace,
		appends,
		notifications,
		failures,
		origins,
		session,
		abort() { aborted = true; },
		setIdentity(next) { identity = next; },
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
						if (aborted) throw new Error("native context operation was aborted");
						return { cwd, mode, hasUI: false, projectTrusted: true, projectTrustResolved: true,
							toolRegistration: { disabled: false, allow: null, deny: [] }, toolSelection: structuredClone(toolSelection),
							isIdle: true, hasPendingMessages: false, systemPrompt: "native prompt", sessionId: session.metadata.id,
							model: publicModel, tools: [...active], identity: identity ? structuredClone(identity) : null };
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
				case "auth": return null;
				case "ui":
					if (input.action === "notify") notifications.push({ kind: input.kind ?? "info", text: input.text });
					return { ok: true };
				case "dialogs": return null;
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
						case "sync": return { durability: options.durability ?? "durable" };
						case "release":
							if (faults.releaseFailure === input.origin) throw new Error("injected release failure");
							origins.delete(input.origin); return null;
						case "append": case "journalAppend": {
							if (faults.appendFailure && faults.appendFailure.types?.includes(input.customType)) {
								throw new Error(faults.appendFailure.message ?? "injected append failure");
							}
							const journal = input.action === "journalAppend";
							const isCleanupResult = input.customType === "cursor-sdk-agent-cleanup" && input.data?.phase === "result";
							const isCleanupIntent = input.customType === "cursor-sdk-agent-cleanup" && input.data?.phase === "intent";
							const late = journal && isCleanupResult && lateAnchor !== null;
							const durability = faults.ephemeralResult && isCleanupResult ? "ephemeral" : (options.durability ?? "durable");
							const entry = makeEntry(input, late ? lateAnchor : undefined);
							const advancedLane = !late;
							if (advancedLane) lane().leafId = entry.id;
							appends.push({ action: input.action, customType: input.customType,
								phase: input.data?.phase ?? null, durability,
								entryId: entry.id, parentId: entry.parentId, advancedLane });
							session.revision += 1; session.entries.push(entry);
							if (isCleanupIntent && faults.lateBranch) {
								// Simulate a tree navigation between the durable intent and its result.
								lateAnchor = entry.id;
								movedLeaf = faults.lateBranch.targetLeafId ?? session.entries.at(-2)?.id ?? entry.id;
								// The origin's navigation revision is unchanged: only the active
								// lane moved, so a later journalAppend must not advance it.
								lane().leafId = movedLeaf;
							}
							return { entry, revision: session.revision, durability, advancedLane };
						}
						default: throw new Error(`Unexpected native session action ${input.action}`);
					}
				default:
					throw new Error(`Unexpected native host port ${input.port}`);
			}
		},
	};
	function makeEntry(input, parentOverride) {
		const parentId = parentOverride !== undefined ? parentOverride : (session.entries.length > 0 ? session.entries[session.entries.length - 1].id : null);
		const entry = { type: "custom", id: `committed-${session.entries.length + 1}`, seq: session.entries.length + 1,
			parentId, timestamp: 1000 + session.entries.length, customType: input.customType,
			...(Object.hasOwn(input, "data") ? { data: input.data } : {}) };
		return entry;
	}
	return host;
}

// ---------------------------------------------------------------- scenario run

function nowMs() { return 1000 + sessionEntries.length; }
let sessionEntries = [];

function appendUser(session, text) {
	const timestamp = nowMs();
	const id = `user-${session.entries.length + 1}`;
	const message = { kind: "user", role: "user", content: text, timestamp };
	session.entries.push({ type: "message", id, seq: session.entries.length + 1, parentId: session.branches[0].leafId, timestamp, message });
	session.branches[0].leafId = id;
	session.revision += 1;
	sessionEntries.push(message);
	return message;
}

function appendAssistant(session, text) {
	const timestamp = nowMs();
	const id = `assistant-${session.entries.length + 1}`;
	const message = { kind: "assistant", role: "assistant", content: [{ type: "text", text }], timestamp,
		api: "cursor-sdk", provider: "cursor", model: "composer-2", stopReason: "stop",
		usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } } };
	session.entries.push({ type: "message", id, seq: session.entries.length + 1, parentId: session.branches[0].leafId, timestamp, message });
	session.branches[0].leafId = id;
	session.revision += 1;
	sessionEntries.push(message);
	return message;
}

function sortObject(object) {
	return Object.fromEntries(Object.entries(object).sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0)));
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
	return { records, totals };
}

function commandFrom(prompt) {
	const match = /^\/([^\s]+)\s*(.*)$/.exec(prompt.trim());
	return match ? { name: match[1], args: match[2] } : undefined;
}

function stepsOf(scenario) {
	if (Array.isArray(scenario.rpi?.steps)) return scenario.rpi.steps;
	const steps = [];
	const prompts = [scenario.run.prompt, ...(scenario.run.followUps ?? [])];
	for (const prompt of prompts) {
		const command = commandFrom(prompt);
		steps.push(command ? { kind: "command", name: command.name, args: command.args } : { kind: "turn", prompt });
	}
	return steps;
}

async function runScenario(file, label = "run") {
	const scenarioPath = join(SCENARIO_DIR, file);
	const scenario = JSON.parse(readFileSync(scenarioPath, "utf8"));
	const tmpRoot = mkdtempSync(join(homedir(), ".rpi-c-tree-"));
	const workspace = join(tmpRoot, "workspace");
	const home = join(tmpRoot, "home");
	const agentDir = join(tmpRoot, "agent");
	const stateHome = join(tmpRoot, "cursor-home");
	const tmpDir = join(tmpRoot, "tmp");
	const logPath = join(tmpRoot, "fake.jsonl");
	const sessionsDir = join(tmpRoot, "sessions");
	for (const dir of [workspace, home, agentDir, stateHome, tmpDir, sessionsDir]) mkdirSync(dir, { recursive: true, mode: 0o700 });
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
	let host;
	const problems = [];
	const session = { metadata: { id: "fixture-session", createdAt: 1000 }, revision: 0, name: null, labels: {}, entries: [], branches: [{ lane: "main", revision: 0, leafId: null }] };
	const sessionFile = join(sessionsDir, "fixture-session.jsonl");
	sessionEntries = [];
	try {
		peer = await connectWorker({ cwd: workspace, env });
		debug("connected");
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
		if (ready.kind !== "event" || ready.event.type !== "ready") throw new Error(`worker did not open: ${JSON.stringify(ready)}`);
		peer.send({ id: open.id, kind: "credit", sequence: ready.sequence });
		const manifest = ready.event.manifest;
		const model = manifest.provider.models.find((candidate) => candidate.id === scenario.run.model);
		if (!model) throw new Error(`scenario model ${scenario.run.model} is not in the worker manifest`);
		host = createHost({ cwd: workspace, mode: "print", model, session, sessionFile, options: scenario.rpi ?? {} });
		host.setActive(manifest.tools.map((tool) => tool.name));
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
		await begin("extension.event", { event: { type: "session_start", reason: "startup" } }).result();

		const collectStream = async (stream, label) => {
			const collected = [];
			for (;;) {
				const frame = await peer.take((candidate) => candidate.id === stream.id);
				if (frame.kind === "event") {
					collected.push(frame.event);
					peer.send({ id: stream.id, kind: "credit", sequence: frame.sequence });
				} else if (frame.kind === "reply") {
					debug(`${label} replied with ${collected.length} events`);
					return { collected, final: frame.result };
				} else {
					throw new Error(`${label} failed: ${frame.error}`);
				}
			}
		};
		const sendTurn = async (prompt, label) => {
			const user = appendUser(session, prompt);
			const stream = begin("provider.stream", {
				entry: "stream", authMode: "replace", model,
				context: { messages: sessionEntries.map((message) => ({ ...message })), systemPrompt: "native prompt" },
				options: { apiKey: API_KEY, env: {}, ...(scenario.streamOptions ?? {}) },
			});
			const { collected, final } = await collectStream(stream, label);
			const terminal = collected.filter((event) => event.type === "done" || event.type === "error").at(-1) ?? collected.at(-1);
			const text = terminal?.message?.content?.filter((part) => part.type === "text").map((part) => part.text).join("") ?? final?.content?.filter((part) => part.type === "text").map((part) => part.text).join("") ?? "";
			return { user, text, final, collected };
		};
		const appendAssistantSafe = (text) => appendAssistant(session, text);

		const steps = stepsOf(scenario);
		let index = 0;
		for (const step of steps) {
			index += 1;
			if (step.kind === "command") {
				await begin("extension.command", { name: step.name, args: step.args }).result();
				debug(`command ${step.name}`);
				continue;
			}
			if (step.kind === "seedCleanupCandidate") {
				const prior = session.entries.filter((entry) => entry.type === "custom" && entry.customType === "cursor-sdk-agent-resume").at(-1);
				if (!prior) throw new Error("seedCleanupCandidate requires a prior resume entry");
				const line = session.branches[0];
				const parentId = line.leafId;
				const entry = { type: "custom", id: `seed-${step.agentId}`, seq: session.entries.length + 1, parentId,
					timestamp: 1000 + session.entries.length, customType: "cursor-sdk-agent-resume",
					data: { ...structuredClone(prior.data), cleanupCandidateAgentIds: step.candidates ?? [step.agentId] } };
				session.entries.push(entry); line.leafId = entry.id; session.revision += 1;
				debug(`seeded cleanup candidate ${step.agentId}`);
				continue;
			}
			if (step.kind === "identity") {
				host.setIdentity(step.identity ?? null);
				debug(`identity ${JSON.stringify(step.identity ?? null)}`);
				continue;
			}
			if (step.kind === "event") {
				await begin("extension.event", { event: step.event }).result();
				debug(`event ${step.event.type}`);
				continue;
			}
			if (step.kind === "summary") {
				const stream = begin("provider.stream", {
					entry: "stream", authMode: "replace", model,
					context: { messages: sessionEntries.map((message) => ({ ...message })), systemPrompt: "native prompt" },
					options: { apiKey: API_KEY, env: {}, ...(scenario.streamOptions ?? {}) },
				});
				await collectStream(stream, `summary ${index}`);
				debug(`summary ${index}`);
				continue;
			}
			const { text } = await sendTurn(step.prompt, `turn ${index}`);
			appendAssistantSafe(text);
			if (scenario.rpi?.turnEnd !== false) {
				await begin("extension.event", { event: { type: "turn_end", message: sessionEntries.at(-1), toolResults: [] } }).result();
				debug(`turn_end ${index}`);
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
			const dir = join(KEEP, scenario.name, label);
			mkdirSync(dir, { recursive: true });
			writeFileSync(join(dir, "trace.json"), `${JSON.stringify(host.trace, null, 2)}\n`);
			writeFileSync(join(dir, "appends.json"), `${JSON.stringify(host.appends, null, 2)}\n`);
			writeFileSync(join(dir, "session.json"), `${JSON.stringify(session, null, 2)}\n`);
			writeFileSync(join(dir, "notifications.json"), `${JSON.stringify(host.notifications, null, 2)}\n`);
		}
	} catch (error) {
		problems.push(`run failed: ${error?.message ?? error}`);
	} finally {
		if (peer) await peer.close().catch(() => {});
	}
	const fakeLog = existsSync(logPath) ? readFileSync(logPath, "utf8") : "";
	const { totals } = aggregateFakeLog(fakeLog);
	rmSync(tmpRoot, { recursive: true, force: true });
	const entries = normalizeC04Entries(session.entries.filter((entry) => entry.type === "custom").map((entry) => ({ customType: entry.customType, data: entry.data })), tmpRoot);
	return {
		problems,
		totals,
		entries,
		appends: session.entries.filter((entry) => entry.type === "custom" && C04_TYPES.has(entry.customType)).map((entry) => entry.customType),
		appendDetails: host.appends.filter((item) => C04_TYPES.has(item.customType)),
		notifications: host.notifications,
		finalLeafId: session.branches[0].leafId,
	};
}

const C04_UPSTREAM = [
	"src/cursor-session-agent-lineage.ts",
	"src/cursor-session-agent-resume.ts",
	"src/cursor-session-agent-cleanup.ts",
	"src/cursor-session-store.ts",
	"src/cursor-session-scope.ts",
	"src/cursor-session-compaction-prep.ts",
	"src/cursor-provider-turn-summary.ts",
];

function upstreamProvenance() {
	const upstream = JSON.parse(readFileSync(join(HERE, "../../upstream.json"), "utf8"));
	const files = {};
	for (const name of C04_UPSTREAM) if (upstream.files[name]) files[name] = upstream.files[name];
	return { repository: upstream.repository, revision: upstream.revision, sdk: upstream.sdk, files };
}

function rpiProvenance() {
	const files = ["run-rpi.mjs", "run-reference.mjs", "normalize.mjs", "fake-sdk/sdk.mjs", "fake-sdk/hooks.mjs", "fake-sdk/register.mjs", "fake-sdk/sqlite.mjs"];
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
		const first = await withTimeout(runScenario(file, "run-1"), `rpi ${file} run 1`);
		const second = await withTimeout(runScenario(file, "run-2"), `rpi ${file} run 2`);
		const observed = {
			sdkCalls: sortObject(first.totals.sdkCalls), runs: sortObject(first.totals.runs),
			bridgeToolExecutions: first.totals.bridgeToolExecutions, toolExecutions: sortObject(first.totals.toolExecutions),
			nativeToolCalls: sortObject(first.totals.nativeToolCalls), violations: first.totals.violations,
		};
		const problems = [...first.problems];
		if (JSON.stringify(first.totals) !== JSON.stringify(second.totals)) problems.push("fake counters differ between two runs");
		if (JSON.stringify(first.entries) !== JSON.stringify(second.entries)) problems.push("normalized C04 entries differ between two runs");
		const want = scenario.expect ?? {};
		const check = (label, expected, got) => { if (JSON.stringify(expected) !== JSON.stringify(got)) problems.push(`${label}: expected ${JSON.stringify(expected)}, observed ${JSON.stringify(got)}`); };
		check("sdkCalls", sortObject(want.sdkCalls ?? {}), observed.sdkCalls);
		check("runs", sortObject(want.runs ?? {}), observed.runs);
		check("bridgeToolExecutions", want.bridgeToolExecutions ?? 0, observed.bridgeToolExecutions);
		check("toolExecutions", sortObject(want.toolExecutions ?? {}), observed.toolExecutions);
		check("nativeToolCalls", sortObject(want.nativeToolCalls ?? {}), observed.nativeToolCalls);
		check("violations", want.violations ?? 0, observed.violations);
		let referenceScenario;
		if (!scenario.rpiOnly) {
			const reference = existsSync(REFERENCE_EVIDENCE_PATH) ? JSON.parse(readFileSync(REFERENCE_EVIDENCE_PATH, "utf8")) : undefined;
			referenceScenario = reference?.scenarios?.find((candidate) => candidate.name === scenario.name);
			if (!reference) problems.push("reference evidence is missing; run tests/c-tree/run-reference.mjs --write");
			else if (!referenceScenario) problems.push(`reference evidence has no scenario ${scenario.name}`);
			else {
				if (JSON.stringify(referenceScenario.c04Entries) !== JSON.stringify(first.entries)) {
					problems.push(`rpi C04 entries differ from the Pi 1.3.0 reference: reference ${JSON.stringify(referenceScenario.c04Entries)}, rpi ${JSON.stringify(first.entries)}`);
				}
				const comparable = (value) => ({ sdkCalls: sortObject(value.sdkCalls ?? {}), runs: sortObject(value.runs ?? {}),
					bridgeToolExecutions: value.bridgeToolExecutions ?? 0, toolExecutions: sortObject(value.toolExecutions ?? {}),
					nativeToolCalls: sortObject(value.nativeToolCalls ?? {}), violations: value.violations ?? 0 });
				if (JSON.stringify(comparable(referenceScenario.observed)) !== JSON.stringify(comparable(observed))) {
					problems.push(`rpi observed differs from the Pi 1.3.0 reference: reference ${JSON.stringify(comparable(referenceScenario.observed))}, rpi ${JSON.stringify(comparable(observed))}`);
				}
			}
		}
		if (Array.isArray(scenario.rpi?.expectEntries)) {
			check("c04Entries", scenario.rpi.expectEntries, first.entries);
		}
		if (scenario.rpi?.expectAppends) check("appends", scenario.rpi.expectAppends, first.appends);
		if (scenario.rpi?.expectAppendDetails) check("appendDetails", scenario.rpi.expectAppendDetails, first.appendDetails);
		if (scenario.rpi?.expectNotifications) check("notifications", scenario.rpi.expectNotifications, first.notifications);
		if (scenario.rpi?.expectLateResult) {
			const cleanup = first.appendDetails.filter((item) => item.customType === "cursor-sdk-agent-cleanup");
			const result = cleanup.at(-1);
			if (!result) problems.push("late result: no cleanup append was recorded");
			else {
				if (result.action !== "journalAppend") problems.push(`late result: expected journalAppend, observed ${result.action}`);
				if (result.advancedLane !== false) problems.push("late result: journalAppend must not advance the newly selected branch");
				if (first.finalLeafId === result.entryId) problems.push("late result: the branch leaf moved to the late result entry");
			}
			const intent = cleanup.find((item) => item.phase === "intent");
			if (!intent) problems.push("late result: no durable intent was recorded before deletion");
			else if (intent.action !== "append" || intent.durability !== "durable") problems.push("late result: intent must be an ordinary durable append");
		}
		results.push({
			name: scenario.name,
			scenarioFile: `tests/c-tree/scenarios/${file}`,
			scenarioSha256: sha256File(join(SCENARIO_DIR, file)),
			observed,
			c04Entries: first.entries,
			appendDetails: first.appendDetails,
			notifications: first.notifications,
			...(referenceScenario ? { referenceEntries: referenceScenario.c04Entries } : {}),
			problems,
			status: problems.length === 0 ? "pass" : "fail",
		});
	}
	const reference = existsSync(REFERENCE_EVIDENCE_PATH) ? JSON.parse(readFileSync(REFERENCE_EVIDENCE_PATH, "utf8")) : undefined;
	const evidence = {
		status: results.every((result) => result.status === "pass") ? "pass" : "fail",
		schema: "rpi-cursor-c04-rpi-evidence/1",
		purpose: "Offline scenario runs of the retained rpi Cursor worker (private peer protocol) against the fake @cursor/sdk@1.0.37 with an authoritative awaited session-port host. No Cursor network calls.",
		runtime: { node: nodeVersion, platform: process.platform, arch: process.arch },
		toolchains: { node: nodeVersion, rustMinimum: "1.78.0", rustDefault: "1.95.0", sdk: "1.0.37" },
		...(reference ? { reference: reference.reference, referenceEvidenceSha256: sha256File(REFERENCE_EVIDENCE_PATH) } : {}),
		upstream: upstreamProvenance(),
		files: rpiProvenance(),
		comparison: "Whole-value comparison of the ordered C04 session entries and fake-SDK counters against reference-evidence.json for reference scenarios; rpi-only scenarios check recorded entry/append/notification expectations including durable intent ordering, journalAppend late-branch behavior and exact cleanup ids.",
		scenarios: results,
		counts: {
			referenceScenarios: results.filter((result) => result.referenceEntries).length,
			rpiOnly: results.filter((result) => !result.referenceEntries).length,
			c04Entries: results.reduce((total, result) => total + result.c04Entries.length, 0),
		},
		notTested: [
			"Pi's built-in /compact command is not dispatched in --print/--json, so compaction summary agents are exercised only by the rpi-only scripted scenario.",
			"Image prompts and cloud agents are outside C04.",
			"The retained worker is driven directly, not through the rpi host CLI/cdylib; end-to-end host wiring remains required.",
		],
		commands: ["node tests/c-tree/run-rpi.mjs", "node tests/c-tree/run-rpi.mjs --write"],
	};
	const text = `${JSON.stringify(evidence, null, 2)}\n`;
	if (WRITE) {
		writeFileSync(EVIDENCE_PATH, text);
		process.stdout.write(`wrote ${EVIDENCE_PATH} (${evidence.status})\n`);
	} else {
		if (!existsSync(EVIDENCE_PATH)) throw new Error("evidence.json missing; run with --write");
		const committed = JSON.parse(readFileSync(EVIDENCE_PATH, "utf8"));
		const regenerated = JSON.parse(text);
		// Compare everything except the host runtime triple, so a changed
		// normalization rule, count or provenance also fails the check.
		const diffs = [];
		for (const key of new Set([...Object.keys(committed), ...Object.keys(regenerated)])) {
			if (key === "runtime") continue;
			if (JSON.stringify(committed[key]) !== JSON.stringify(regenerated[key])) diffs.push(key);
		}
		if (diffs.length > 0) { process.stdout.write(`evidence.json is stale: ${diffs.join(", ")}\n`); process.exitCode = 1; }
	}
	for (const result of results) {
		process.stdout.write(`${result.status.toUpperCase()} ${result.name}: ${JSON.stringify(result.observed.sdkCalls)} entries=${result.c04Entries.length}\n`);
		for (const problem of result.problems) process.stdout.write(`  - ${problem}\n`);
		if (result.status !== "pass") process.exitCode = 1;
	}
}

main().catch((error) => { process.stderr.write(`${error?.stack ?? error}\n`); process.exit(1); });
