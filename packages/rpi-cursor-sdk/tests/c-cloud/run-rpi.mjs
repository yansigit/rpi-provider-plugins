#!/usr/bin/env node
// Candidate half of the ledger C06 cloud differential.
//
// Drives the retained rpi Cursor worker (packaged runtime/main.mjs) through its
// private peer protocol with an emulated native host, using the scenario-driven
// fake `@cursor/sdk`. For every scenario it runs the same cloud prompt and
// `/cursor-cloud` commands as the Pi reference, then records the complete cloud
// SDK interactions, the ordered `cursor-cloud-lifecycle` session entries and the
// durable journal entries. The host implements the native journal port the worker
// routes cloud lifecycle IO through.
//
//   node tests/c-cloud/run-rpi.mjs             check mode
//   node tests/c-cloud/run-rpi.mjs --write     regenerate evidence.json
//   node tests/c-cloud/run-rpi.mjs --only NAME run one scenario
//   node tests/c-cloud/run-rpi.mjs --keep DIR  keep raw runs under DIR
//
// Never touches ~/.pi, ~/.rpi or the network: HOME, TMPDIR and every fake path
// live inside a temporary directory under $HOME and are removed afterwards.
import { spawn } from "node:child_process";
import { createHash, randomBytes } from "node:crypto";
import { once } from "node:events";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { createServer } from "node:net";
import { fileURLToPath } from "node:url";
import {
	FROZEN_CLOCK,
	SCHEMA,
	fakeRecords,
	lifecycleCounts,
	lifecycleEntries as lifecycleOf,
	observedFromRecords,
	readJournals,
	sha256File,
	writeEvidence,
} from "./lib.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const PACKAGE_ROOT = resolve(HERE, "../..");
const WORKER_ENTRY = join(PACKAGE_ROOT, "runtime/main.mjs");
const SCENARIO_DIR = join(HERE, "scenarios");
const REGISTER = join(HERE, "fake-sdk/register.mjs");
const EVIDENCE_PATH = join(HERE, "evidence.json");
const REFERENCE_EVIDENCE_PATH = join(HERE, "reference-evidence.json");
const TIMEOUT_MS = 180_000;
const API_KEY = "fake";
const DEBUG = process.env.RPI_C_CLOUD_DEBUG === "1";
const debug = (...parts) => { if (DEBUG) process.stderr.write(`[c-cloud] ${parts.join(" ")}\n`); };

const args = process.argv.slice(2);
const WRITE = args.includes("--write");
const KEEP = args.includes("--keep") ? resolve(args[args.indexOf("--keep") + 1]) : undefined;
const ONLY = args.includes("--only") ? args[args.indexOf("--only") + 1] : undefined;

// Pi CLI boolean flags. Everything else that this extension registers is a string.
const BOOLEAN_FLAGS = new Set([
	"cursor-cloud-ack",
	"cursor-cloud-direct-push",
	"cursor-cloud-auto-create-pr",
	"cursor-cloud-skip-reviewer-request",
	"cursor-cloud-allow-local-state",
	"cursor-cloud-env-from-files",
	"cursor-auto-review",
	"cursor-sandbox",
	"cursor-local-force",
	"cursor-local-resume",
	"cursor-no-local-resume",
]);

/** Translate the scenario's Pi CLI args into the flag values the host supplies. */
function flagsFromArgs(argv) {
	const flags = {};
	for (let index = 0; index < argv.length; index++) {
		const arg = argv[index];
		if (!arg.startsWith("--")) continue;
		const name = arg.slice(2);
		if (!name.startsWith("cursor-")) continue;
		if (BOOLEAN_FLAGS.has(name)) {
			flags[name] = true;
			continue;
		}
		flags[name] = argv[index + 1];
		index++;
	}
	return flags;
}

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
			child.stderr.on("data", (chunk) => { output += chunk; });
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
				socket.on("error", () => {});
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
							socket.write(encodeFrame({ version: 1, generation: config.generation, ready: true }));
							authenticated();
							continue;
						}
						frames.push(frame);
						for (const wake of waiters.splice(0)) wake();
					}
					prefix = buffer;
				});
				const send = (frame) => { socket.write(encodeFrame({ version: 1, generation: config.generation, ...frame })); };
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

function createHost({ cwd, mode, model, session, sessionFile, flags }) {
	const { baseUrl: _baseUrl, headers: _headers, ...publicModel } = model;
	const trace = [];
	const notifications = [];
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
				const bytes = Buffer.from(input.bytes, "base64");
				const path = journalFile(input.kind);
				const existing = existsSync(path) ? readFileSync(path) : Buffer.alloc(0);
				if (input.expectedDigest !== undefined && createHash("sha256").update(existing).digest("hex") !== input.expectedDigest) {
					return { committed: false };
				}
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
		notifications,
		failures,
		origins,
		session,
		setActive(names) { active = [...names]; },
		activeTools: () => [...active],
		async handle(method, input) {
			if (method === "scoped") {
				if (!origins.has(input.scope)) throw new Error("scoped call must retain its origin until release acknowledgement");
				if (input.method === "journals") return journals(input.input);
				return host.handle(input.method, input.input);
			}
			if (method === "native" && input.port === "ui" && input.action === "notify") {
				notifications.push({ kind: input.kind, text: input.text });
			}
			trace.push({ method, input: structuredClone(input) });
			if (method !== "native") throw new Error(`Unexpected host method ${method}`);
			switch (input.port) {
				case "context":
					if (input.action === "flags") return { ...flags };
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
					if (input.action === "resolve") return { apiKey: API_KEY, source: "fixture" };
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

async function runScenario(file) {
	const scenarioPath = join(SCENARIO_DIR, file);
	const scenario = JSON.parse(readFileSync(scenarioPath, "utf8"));
	const tmpRoot = mkdtempSync(join(homedir(), ".rpi-c-cloud-"));
	const workspace = join(tmpRoot, "workspace");
	const home = join(tmpRoot, "home");
	const agentDir = join(tmpRoot, "agent");
	const tmpDir = join(tmpRoot, "tmp");
	const logPath = join(tmpRoot, "fake.jsonl");
	const sessionsDir = join(tmpRoot, "sessions");
	for (const dir of [workspace, home, agentDir, tmpDir, sessionsDir]) mkdirSync(dir, { recursive: true, mode: 0o700 });
	const env = {
		PATH: `${dirname(process.execPath)}:/usr/local/bin:/usr/bin:/bin`,
		HOME: home, TMPDIR: tmpDir, LC_ALL: "en_US.UTF-8", TERM: "xterm-256color",
		CURSOR_API_KEY: API_KEY,
		PI_CURSOR_SETTING_SOURCES: "all",
		NODE_OPTIONS: `--import ${REGISTER}`,
		RPI_CURSOR_FAKE_SCENARIO: scenarioPath,
		RPI_CURSOR_FAKE_LOG: logPath,
		RPI_CURSOR_FAKE_LOG_ROOT: tmpRoot,
		RPI_CURSOR_FAKE_HOME: join(tmpRoot, "cursor-home"),
		RPI_CURSOR_FAKE_CLOCK: FROZEN_CLOCK,
	};
	const flags = flagsFromArgs(scenario.run.args ?? []);
	const problems = [];
	let peer;
	let host;
	let exitCode = 0;
	let streamError = null;
	const streamEvents = [];
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
		debug("opened");
		const manifest = ready.event.manifest;
		const model = manifest.provider.models.find((candidate) => candidate.id === scenario.run.model);
		if (!model) throw new Error(`scenario model ${scenario.run.model} is not in the worker manifest`);
		const sessionFile = join(sessionsDir, "fixture-session.jsonl");
		// The native journal path needs a real session file for the command paths
		// that check its existence; the worker's native branch uses the host port.
		writeFileSync(sessionFile, "");
		const session = { metadata: { id: "fixture-session", createdAt: 1000 }, revision: 0, name: null, labels: {}, entries: [], branches: [{ lane: "main", revision: 0, leafId: null }] };
		host = createHost({ cwd: workspace, mode: "json", model, session, sessionFile, flags });
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
		debug("configured", JSON.stringify(flags));
		await begin("extension.event", { event: { type: "session_start", reason: "startup" } }).result();
		debug("session_start");

		const sendStream = (prompt) => begin("provider.stream", {
			entry: "stream", authMode: "replace", model,
			context: { messages: [{ role: "user", content: prompt, timestamp: 1 }], systemPrompt: "native prompt" },
			options: { apiKey: API_KEY, env: {} },
		});
		const stream = sendStream(scenario.run.prompt);
		debug("stream sent");
		let reply;
		for (;;) {
			const frame = await peer.take((candidate) => candidate.id === stream.id);
			if (frame.kind === "event") {
				streamEvents.push(frame.event);
				peer.send({ id: stream.id, kind: "credit", sequence: frame.sequence });
			} else if (frame.kind === "reply") {
				reply = frame.result;
				debug("stream replied");
				break;
			} else {
				streamError = frame.error;
				break;
			}
		}
		if (reply === undefined && streamError === null && problems.length === 0) problems.push("provider.stream returned no reply");
		if (streamError === null && reply !== undefined) {
			// The native stream surfaces a refused cloud send as an `error` event whose
			// `error` is the terminal assistant message (Pi's `turn_end` equivalent).
			const errored = streamEvents.filter((event) => event.error?.stopReason === "error").at(-1)
				?? streamEvents.filter((event) => event.message?.stopReason === "error").at(-1);
			const message = errored?.error ?? errored?.message;
			if (message && typeof message.errorMessage === "string") streamError = message.errorMessage;
		}
		if (typeof streamError === "string") streamError = streamError.split(tmpRoot).join("<root>");

		for (const followUp of scenario.run.followUps ?? []) {
			if (!followUp.startsWith("/")) throw new Error(`scenario follow-up is not a command: ${followUp}`);
			const [name, ...rest] = followUp.slice(1).split(/\s+/);
			debug("command", name, rest.join(" "));
			try {
				await begin("extension.command", { name, args: rest.join(" ") }).result();
			} catch (error) {
				exitCode = 1;
				problems.push(`command ${followUp} failed: ${error.message}`);
			}
		}

		peer.send({ id: open.id, kind: "cancel" });
		await Promise.race([open.result().catch(() => {}), new Promise((wake) => setTimeout(wake, 5000).unref?.())]);
		const deadline = Date.now() + 5000;
		while (host.origins.size > 0 && Date.now() < deadline) await new Promise((wake) => setTimeout(wake, 20));
		pumping = false;
		await peer.close();
		peer = undefined;
		if (host.failures.length) { exitCode = 1; problems.push(`host failures: ${host.failures.join("; ")}`); }
		if (host.origins.size !== 0) problems.push("native session origins were not released");
	} catch (error) {
		exitCode = 1;
		problems.push(`run failed: ${error?.message ?? error}`);
	} finally {
		if (peer) await peer.close().catch(() => {});
	}
	const fakeLog = existsSync(logPath) ? readFileSync(logPath, "utf8") : "";
	const records = fakeRecords(fakeLog);
	const lifecycle = host ? lifecycleOf(host.session.entries) : [];
	const journal = readJournals(sessionsDir);
	if (KEEP) {
		const dir = join(KEEP, scenario.name);
		mkdirSync(dir, { recursive: true });
		writeFileSync(join(dir, "fake.jsonl"), fakeLog);
		if (host) {
			writeFileSync(join(dir, "events.json"), `${JSON.stringify(streamEvents, null, 2)}\n`);
			writeFileSync(join(dir, "session.json"), `${JSON.stringify(host.session, null, 2)}\n`);
			writeFileSync(join(dir, "trace.json"), `${JSON.stringify(host.trace, null, 2)}\n`);
			writeFileSync(join(dir, "notifications.json"), `${JSON.stringify(host.notifications, null, 2)}\n`);
		}
	}
	rmSync(tmpRoot, { recursive: true, force: true });
	return {
		name: scenario.name,
		scenarioPath,
		observed: observedFromRecords(records, lifecycle, journal, streamError),
		notifications: host?.notifications ?? [],
		problems,
	};
}

function aggregate(attempt) {
	return { observed: attempt.observed, notifications: attempt.notifications };
}

async function runWithDeterminism(file) {
	const first = await runScenario(file);
	const second = await runScenario(file);
	const problems = [...first.problems];
	if (JSON.stringify(aggregate(first)) !== JSON.stringify(aggregate(second))) {
		problems.push("candidate output differs between two runs");
	}
	const scenarioPath = first.scenarioPath;
	const scenario = JSON.parse(readFileSync(scenarioPath, "utf8"));
	const want = scenario.expect ?? {};
	if (want.sdkCalls) {
		const expected = Object.fromEntries(Object.entries(want.sdkCalls).sort());
		if (JSON.stringify(expected) !== JSON.stringify(first.observed.sdkCalls)) {
			problems.push(`sdkCalls: expected ${JSON.stringify(expected)}, observed ${JSON.stringify(first.observed.sdkCalls)}`);
		}
	}
	if (want.sendErrorIncludes !== undefined) {
		const error = first.observed.sendError ?? "";
		if (!error.includes(want.sendErrorIncludes)) problems.push(`sendError: expected to include ${JSON.stringify(want.sendErrorIncludes)}, observed ${JSON.stringify(error)}`);
	}
	if (want.noSendError && first.observed.sendError !== null) problems.push(`sendError: expected none, observed ${JSON.stringify(first.observed.sendError)}`);
	return {
		name: first.name,
		scenarioFile: `tests/c-cloud/scenarios/${file}`,
		scenarioSha256: sha256File(scenarioPath),
		observed: first.observed,
		notifications: first.notifications,
		expect: scenario.expect ?? {},
		determinism: { candidate: JSON.stringify(aggregate(first)) === JSON.stringify(aggregate(second)) },
		problems,
		status: problems.length === 0 ? "pass" : "fail",
	};
}

function rpiProvenance() {
	const files = ["lib.mjs", "run-reference.mjs", "run-rpi.mjs", "fake-sdk/sdk.mjs", "fake-sdk/hooks.mjs", "fake-sdk/register.mjs", "fake-sdk/sqlite.mjs"];
	return Object.fromEntries(files.map((name) => [name, sha256File(join(HERE, name))]));
}

async function main() {
	if (!existsSync(WORKER_ENTRY)) throw new Error(`worker entry missing at ${WORKER_ENTRY}`);
	if (!existsSync(join(PACKAGE_ROOT, "runtime/dist/rpi/runtime.js"))) throw new Error("staged worker assets missing; build worker/ and run tests/stage-runtime.mjs");
	const node = process.versions.node;
	if (Number(node.split(".")[0]) < 24) throw new Error(`Node 24+ required, found ${node}`);
	const files = readdirSync(SCENARIO_DIR).filter((name) => name.endsWith(".json")).sort();
	const selected = ONLY ? files.filter((name) => name === `${ONLY}.json`) : files;
	if (selected.length === 0) throw new Error("no scenarios selected");
	const results = [];
	for (const file of selected) {
		process.stdout.write(`running rpi ${file} ...\n`);
		const withTimeout = (promise, label) => Promise.race([
			promise,
			new Promise((_, reject) => setTimeout(() => reject(new Error(`${label} timed out`)), TIMEOUT_MS).unref?.()),
		]);
		results.push(await withTimeout(runWithDeterminism(file), `rpi ${file}`));
	}
	const reference = existsSync(REFERENCE_EVIDENCE_PATH) ? JSON.parse(readFileSync(REFERENCE_EVIDENCE_PATH, "utf8")) : undefined;
	const evidence = {
		status: results.every((result) => result.status === "pass") ? "pass" : "fail",
		schema: SCHEMA,
		role: "candidate",
		purpose: "Offline cloud scenarios through the retained rpi Cursor worker (private peer protocol, emulated native host) against the fake @cursor/sdk. No Cursor network access; wall clock and SDK ids are fixed.",
		runtime: { node, platform: process.platform, arch: process.arch },
		clock: FROZEN_CLOCK,
		...(reference ? { referenceEvidenceSha256: sha256File(REFERENCE_EVIDENCE_PATH) } : {}),
		upstream: { piCursorSdkCommit: "f337dc4", piVersion: "1.3.0", cursorSdkVersion: "1.0.37", piCursorSdkVersion: "0.5.3" },
		toolchains: { rustMinimum: "1.78.0", rustDefault: "1.95.0", node: "24.22.0" },
		scenarioCount: results.length,
		caseCount: results.length,
		normalization: {
			cloudCalls: "Agent.create/archive/delete/get/list and cloud agent.send records are kept whole; only agent.send message text and image bytes are dropped because prompt/image assembly is C01, not C06.",
			journal: "Whole durable entries are kept; only the per-run sessionId, sessionFile and anchorEntryId values are replaced by <session>/<file>/<anchor>.",
			lifecycle: "Complete entry data payloads from the cursor-cloud-lifecycle session entries.",
			clock: "RPI_CURSOR_FAKE_CLOCK freezes Date for both runtimes; fake SDK agent ids come from a counter.",
		},
		compared: ["sdkCalls", "cloudCalls", "lifecycle", "lifecycleCounts", "journal", "sendError"],
		files: rpiProvenance(),
		scenarios: results,
		notTested: [
			"Live Cursor cloud resources, billing and the real Cursor backend are never contacted.",
			"The rpi host binary/cdylib is not run; the packaged worker is driven over its real peer protocol by an emulated host, including the native journal port.",
			"Interactive/TUI cloud acknowledgement dialogs are not rendered; only the non-interactive flag path is exercised.",
			"Prompt/image assembly is compared by the C01 prompt lane, not here.",
		],
		commands: [
			"cd worker && npm ci --offline --ignore-scripts && npm run build",
			"node tests/stage-runtime.mjs",
			"(cd runtime && npm ci --offline --ignore-scripts)",
			"node tests/c-cloud/run-rpi.mjs        (check mode)",
			"node tests/c-cloud/run-rpi.mjs --write",
		],
	};
	writeEvidence(EVIDENCE_PATH, evidence, WRITE);
	for (const result of results) {
		process.stdout.write(`${result.status.toUpperCase()} ${result.name}: ${JSON.stringify(result.observed.sdkCalls)} lifecycle=${JSON.stringify(result.observed.lifecycleCounts)}\n`);
		for (const problem of result.problems) process.stdout.write(`  - ${problem}\n`);
		if (result.status !== "pass") process.exitCode = 1;
	}
}

main().catch((error) => {
	process.stderr.write(`${error?.stack ?? error}\n`);
	process.exitCode = 1;
});
