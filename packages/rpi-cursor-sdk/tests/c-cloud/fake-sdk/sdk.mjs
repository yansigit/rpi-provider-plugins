// Scenario-driven fake of @cursor/sdk@1.0.37 for offline parity runs.
//
// It mirrors the public declarations in
// provider-references/integrated/node_modules/@cursor/sdk/dist/esm (agent.d.ts,
// stubs.d.ts, run.d.ts, errors.d.ts, options.d.ts, messages.d.ts, usage-types.d.ts)
// for the members the pinned adapter uses. It performs no network I/O of its own:
// the only outbound request is the MCP JSON-RPC call to a bridge URL that the
// caller itself supplied in `mcpServers` (the adapter's loopback bridge).
//
// Environment:
//   RPI_CURSOR_FAKE_SCENARIO   (required) path to a scenario JSON file
//   RPI_CURSOR_FAKE_LOG        append-only JSONL call/tool log (optional)
//   RPI_CURSOR_FAKE_LOG_ROOT   path prefix replaced by "<root>" in the log (optional)
//   RPI_CURSOR_FAKE_HOME       persistent fake state directory (optional; without it
//                              state lives only in this process)
import { appendFileSync, existsSync, mkdirSync, readFileSync, renameSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { tmpdir } from "node:os";

const SCENARIO_PATH = process.env.RPI_CURSOR_FAKE_SCENARIO;
if (!SCENARIO_PATH) throw new Error("RPI_CURSOR_FAKE_SCENARIO must name a scenario JSON file");
const SCENARIO = JSON.parse(readFileSync(SCENARIO_PATH, "utf8"));
if (SCENARIO.schema !== "rpi-cursor-fake-sdk/1") {
	throw new Error(`Unsupported fake SDK scenario schema: ${String(SCENARIO.schema)}`);
}

const LOG_PATH = process.env.RPI_CURSOR_FAKE_LOG || undefined;
const LOG_ROOT = process.env.RPI_CURSOR_FAKE_LOG_ROOT || undefined;
const STATE_HOME = process.env.RPI_CURSOR_FAKE_HOME || undefined;
const SECRET_VALUE = process.env.CURSOR_API_KEY || undefined;

// ---------------------------------------------------------------- counters / log

const COUNTERS = {
	sdkCalls: {},
	deltas: {},
	steps: {},
	nativeToolCalls: {},
	bridgeRequests: {},
	toolExecutions: {},
	runs: {},
	violations: 0,
};

function bump(bucket, key, by = 1) {
	COUNTERS[bucket][key] = (COUNTERS[bucket][key] ?? 0) + by;
}

const SECRET_KEYS = new Set(["apiKey", "api_key", "token", "secret", "password", "authorization", "clientSecret", "CLIENT_SECRET", "headers", "auth"]);

function summarize(value, key) {
	if (value === undefined) return undefined;
	if (typeof value === "function") return "<function>";
	if (key === "url" && typeof value === "string" && value.startsWith("http://127.0.0.1:")) {
		// Loopback bridge endpoints use a random port and a random path; logs keep placeholders.
		return value.replace(/^http:\/\/127\.0\.0\.1:\d+/, "http://127.0.0.1:<port>").replace(/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/g, "<uuid>");
	}
	if (value === null || typeof value !== "object") {
		if (key && SECRET_KEYS.has(key)) return "<redacted>";
		if (typeof value === "string" && SECRET_VALUE && value === SECRET_VALUE) return "<redacted>";
		return value;
	}
	if (Buffer.isBuffer(value)) return `<buffer ${value.length}>`;
	if (Array.isArray(value)) return value.map((item) => summarize(item));
	if (typeof value.close === "function" && typeof value.get === "function" && value.agents) return "<store>";
	if (key === "envVars") return Object.fromEntries(Object.keys(value).map((name) => [name, "<redacted>"]));
	if (key === "store") return "<store>";
	if (key === "customTools") return Object.fromEntries(Object.keys(value).map((name) => [name, "<custom-tool>"]));
	const out = {};
	for (const [name, child] of Object.entries(value)) {
		if (child === undefined) continue;
		if (name === "data" && typeof child === "string" && typeof value.mimeType === "string") {
			out.data = `<base64 ${child.length} chars>`;
			continue;
		}
		out[name] = SECRET_KEYS.has(name) ? "<redacted>" : summarize(child, name);
	}
	return out;
}

let logDirReady = false;
function emit(record) {
	if (!LOG_PATH) return;
	if (!logDirReady) {
		mkdirSync(dirname(LOG_PATH), { recursive: true });
		logDirReady = true;
	}
	let line = JSON.stringify(record);
	if (LOG_ROOT) line = line.split(LOG_ROOT).join("<root>");
	appendFileSync(LOG_PATH, `${line}\n`);
}

function record(kind, fields = {}) {
	emit({ kind, ...summarize(fields) });
}

process.on("exit", () => {
	if (!LOG_PATH) return;
	emit({ kind: "process_counters", counters: COUNTERS });
});

// ---------------------------------------------------------------- errors (errors.d.ts)

export class CursorSdkError extends Error {
	constructor(message, options = {}) {
		super(message, options.cause === undefined ? undefined : { cause: options.cause });
		this.name = options.errorName ?? new.target.name;
		this.isRetryable = options.isRetryable ?? false;
		if (options.code !== undefined) this.code = options.code;
		if (options.status !== undefined) this.status = options.status;
		if (options.endpoint !== undefined) this.endpoint = options.endpoint;
		if (options.requestId !== undefined) this.requestId = options.requestId;
		if (options.operation !== undefined) this.operation = options.operation;
	}

	toJSON() {
		return { name: this.name, message: this.message, isRetryable: this.isRetryable, code: this.code, status: this.status };
	}
}
export class CursorAgentError extends CursorSdkError {}
export class AuthenticationError extends CursorAgentError {}
export class RateLimitError extends CursorAgentError {}
export class ConfigurationError extends CursorAgentError {}
export class AgentBusyError extends CursorAgentError {}
export class NetworkError extends CursorAgentError {}
export class UnknownAgentError extends CursorAgentError {}
export class AgentNotFoundError extends CursorAgentError {}
export class IntegrationNotConnectedError extends ConfigurationError {
	constructor(message, options) {
		super(message, options);
		this.helpUrl = options.helpUrl;
		this.provider = options.provider;
	}
}
export class UnsupportedRunOperationError extends ConfigurationError {
	constructor(operation, reason) {
		super(reason ?? `Run operation ${operation} is not supported by the fake SDK`);
		this.operation = operation;
	}
}

const ERROR_CLASSES = {
	CursorSdkError,
	CursorAgentError,
	AuthenticationError,
	RateLimitError,
	ConfigurationError,
	AgentBusyError,
	NetworkError,
	UnknownAgentError,
	AgentNotFoundError,
	IntegrationNotConnectedError,
};

export function wrapSdkError(err, context = {}) {
	if (err instanceof CursorSdkError) return err;
	const message = err instanceof Error ? err.message : String(err);
	return new CursorAgentError(message, { ...context, cause: err });
}
export function convertError(err) {
	return err instanceof CursorSdkError ? err : err instanceof Error ? err : new Error(String(err));
}
export function toRunError(err) {
	const message = err instanceof Error ? err.message : String(err);
	return err && typeof err === "object" && typeof err.code === "string" ? { message, code: err.code } : { message };
}

// ---------------------------------------------------------------- persistent state

const MEMORY_STATE = { seq: {}, turnCursor: 0, local: {}, cloud: {} };
const STATE_FILE = STATE_HOME ? join(STATE_HOME, "fake-cursor-state.json") : undefined;

function loadState() {
	if (!STATE_FILE) return MEMORY_STATE;
	if (!existsSync(STATE_FILE)) return { seq: {}, turnCursor: 0, local: {}, cloud: {} };
	return JSON.parse(readFileSync(STATE_FILE, "utf8"));
}

function withState(mutator) {
	const state = loadState();
	try {
		return mutator(state);
	} finally {
		if (STATE_FILE) {
			mkdirSync(dirname(STATE_FILE), { recursive: true });
			const tmp = `${STATE_FILE}.tmp`;
			writeFileSync(tmp, `${JSON.stringify(state, null, 1)}\n`);
			renameSync(tmp, STATE_FILE);
		}
	}
}

function nextSeq(state, kind) {
	state.seq[kind] = (state.seq[kind] ?? 0) + 1;
	return state.seq[kind];
}

function fakeUuid(n) {
	return `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;
}

// ---------------------------------------------------------------- scenario helpers

function resolveApiKey(options) {
	const key = options?.apiKey ?? process.env.CURSOR_API_KEY;
	if (!key) throw new AuthenticationError("Cursor API key is required (set CURSOR_API_KEY or pass apiKey)", { code: "unauthenticated" });
	const expected = SCENARIO.auth?.apiKey;
	if (expected !== undefined && key !== expected) {
		throw new AuthenticationError("Invalid Cursor API key", { code: "unauthenticated", status: 401 });
	}
	return key;
}

function isCloudAgentId(agentId) {
	return typeof agentId === "string" && agentId.startsWith("bc-");
}

/**
 * C06 failure injection: a scenario may declare `cloudErrors: { archive|delete: { name, message, code, status } }`.
 * The call is logged and counted exactly like a real one, then throws before mutating
 * fake state. This models a failing cloud sync that leaves the lifecycle intent unresolved.
 */
function cloudFailure(action) {
	const failure = SCENARIO.cloudErrors?.[action];
	if (!failure) return;
	const ErrorClass = ERROR_CLASSES[failure.name] ?? CursorAgentError;
	throw new ErrorClass(failure.message ?? `injected ${action} failure`, { code: failure.code, status: failure.status });
}

function storeKey(store) {
	return store?.stateRoot ?? "default";
}

function localBucket(state, key) {
	state.local[key] ??= { agents: {} };
	return state.local[key];
}

function takeTurn(state) {
	const turns = SCENARIO.turns ?? [];
	const index = state.turnCursor;
	if (index >= turns.length) {
		COUNTERS.violations += 1;
		record("violation", { reason: "no_scenario_turn_remaining", turnIndex: index });
		throw new ConfigurationError(`fake scenario "${SCENARIO.name}" has no turn left for run ${index + 1}`, { code: "fake_scenario_exhausted" });
	}
	state.turnCursor = index + 1;
	return { index, turn: turns[index] };
}

// ---------------------------------------------------------------- bridge MCP client

const MCP_CLIENT_VERSION = "1.0.37";

async function postJsonRpc(url, session, body, signal) {
	const headers = { "content-type": "application/json", accept: "application/json, text/event-stream" };
	if (session.sessionId) headers["mcp-session-id"] = session.sessionId;
	if (session.protocolVersion) headers["mcp-protocol-version"] = session.protocolVersion;
	const response = await fetch(url, { method: "POST", headers, body: JSON.stringify(body), signal });
	const sessionId = response.headers.get("mcp-session-id");
	if (sessionId) session.sessionId = sessionId;
	if (response.status === 202 || body.id === undefined) {
		await response.body?.cancel().catch(() => undefined);
		return undefined;
	}
	const text = await response.text();
	const type = response.headers.get("content-type") ?? "";
	let message;
	if (type.includes("text/event-stream")) {
		const payloads = text
			.split(/\r?\n/)
			.filter((line) => line.startsWith("data:"))
			.map((line) => line.slice(5).trimStart());
		for (const payload of payloads) {
			const candidate = JSON.parse(payload);
			if (candidate.id === body.id) message = candidate;
		}
	} else {
		message = JSON.parse(text);
	}
	if (!message) throw new NetworkError(`bridge returned no JSON-RPC response for ${body.method}`, { code: "bridge_no_response", status: response.status });
	if (message.error) throw new UnknownAgentError(`bridge ${body.method} failed: ${message.error.message}`, { code: String(message.error.code ?? "bridge_error") });
	return message.result;
}

async function callBridgeTool({ url, serverName, toolName, args, callId, signal }) {
	const session = {};
	let nextId = 1;
	const init = await postJsonRpc(url, session, {
		jsonrpc: "2.0",
		id: nextId++,
		method: "initialize",
		params: { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "fake-cursor-sdk", version: MCP_CLIENT_VERSION } },
	}, signal);
	session.protocolVersion = init?.protocolVersion ?? "2025-06-18";
	record("bridge_request", { method: "initialize", server: serverName, protocolVersion: session.protocolVersion });
	bump("bridgeRequests", "initialize");
	await postJsonRpc(url, session, { jsonrpc: "2.0", method: "notifications/initialized" }, signal);
	const list = await postJsonRpc(url, session, { jsonrpc: "2.0", id: nextId++, method: "tools/list", params: {} }, signal);
	const names = (list?.tools ?? []).map((tool) => tool.name);
	record("bridge_request", { method: "tools/list", server: serverName, tools: names });
	bump("bridgeRequests", "tools/list");
	const resolvedName = toolName === "$first" ? names[0] : toolName;
	if (!resolvedName || !names.includes(resolvedName)) {
		throw new NetworkError(`bridge tool ${String(toolName)} is not exposed; exposed: ${names.join(", ")}`, { code: "bridge_tool_missing" });
	}
	record("bridge_request", { method: "tools/call", server: serverName, tool: resolvedName, callId, arguments: args });
	bump("bridgeRequests", "tools/call");
	const result = await postJsonRpc(url, session, {
		jsonrpc: "2.0",
		id: nextId++,
		method: "tools/call",
		params: { name: resolvedName, arguments: args ?? {} },
	}, signal);
	return { toolName: resolvedName, result };
}

function mcpResultToSdk(result) {
	const content = (result?.content ?? []).map((item) => {
		if (item.type === "text") return { text: { text: item.text } };
		if (item.type === "image") return { image: { data: item.data, mimeType: item.mimeType } };
		return { text: { text: JSON.stringify(item) } };
	});
	if (result?.isError) return { status: "error", error: { content } };
	return { status: "success", value: { content } };
}

// ---------------------------------------------------------------- runs

function assertExpectations(expect, payload, options, agentId) {
	const problems = [];
	const text = typeof payload === "string" ? payload : payload?.text ?? "";
	const images = typeof payload === "string" ? [] : payload?.images ?? [];
	if (expect?.promptIncludes !== undefined && !text.includes(expect.promptIncludes)) {
		problems.push(`prompt does not include ${JSON.stringify(expect.promptIncludes)}`);
	}
	if (expect?.imageCount !== undefined && images.length !== expect.imageCount) {
		problems.push(`expected ${expect.imageCount} images, got ${images.length}`);
	}
	if (expect?.mode !== undefined && (options?.mode ?? "agent") !== expect.mode) {
		problems.push(`expected mode ${expect.mode}, got ${String(options?.mode)}`);
	}
	if (expect?.modelId !== undefined && options?.model?.id !== expect.modelId) {
		problems.push(`expected model ${expect.modelId}, got ${String(options?.model?.id)}`);
	}
	if (expect?.mcpServers !== undefined) {
		const actual = Object.keys(options?.mcpServers ?? {}).sort();
		if (JSON.stringify(actual) !== JSON.stringify([...expect.mcpServers].sort())) {
			problems.push(`expected mcpServers ${JSON.stringify(expect.mcpServers)}, got ${JSON.stringify(actual)}`);
		}
	}
	if (problems.length > 0) {
		COUNTERS.violations += 1;
		record("violation", { reason: "expectation", agentId, problems });
	}
	return problems;
}

function validatePayload(message) {
	if (typeof message === "string") return { text: message, images: [] };
	if (!message || typeof message !== "object" || typeof message.text !== "string") {
		throw new ConfigurationError("message must be a string or { text, images }", { code: "invalid_message" });
	}
	for (const image of message.images ?? []) {
		const hasUrl = typeof image.url === "string";
		const hasData = typeof image.data === "string" && typeof image.mimeType === "string";
		if (!hasUrl && !hasData) throw new ConfigurationError("image needs url or data+mimeType", { code: "invalid_image" });
	}
	return { text: message.text, images: message.images ?? [] };
}

class FakeRun {
	#id;
	#requestId;
	#agentId;
	#status = "running";
	#result;
	#error;
	#model;
	#durationMs;
	#usage;
	#git;
	#createdAt;
	#events = [];
	#statusListeners = new Set();
	#settleResolvers = [];
	#changeResolvers = [];
	#cancelResolvers = [];
	#cancelRequested = false;
	#conversationSteps = [];
	#abort = new AbortController();

	constructor({ id, requestId, agentId, model, createdAt }) {
		this.#id = id;
		this.#requestId = requestId;
		this.#agentId = agentId;
		this.#model = model;
		this.#createdAt = createdAt;
	}

	get id() { return this.#id; }
	get requestId() { return this.#requestId; }
	get agentId() { return this.#agentId; }
	get status() { return this.#status; }
	get result() { return this.#result; }
	get error() { return this.#error; }
	get model() { return this.#model; }
	get durationMs() { return this.#durationMs; }
	get usage() { return this.#usage; }
	get git() { return this.#git; }
	get createdAt() { return this.#createdAt; }
	get cancelRequested() { return this.#cancelRequested; }
	get abortSignal() { return this.#abort.signal; }

	supports(operation) {
		return ["stream", "wait", "cancel", "conversation"].includes(operation);
	}

	unsupportedReason(operation) {
		return this.supports(operation) ? undefined : `fake run does not support ${operation}`;
	}

	// Internal: the executor pushes SDK messages and conversation steps here.
	_push(message) {
		this.#events.push(message);
		const waiters = this.#changeResolvers.splice(0);
		for (const resolve of waiters) resolve();
	}

	_addConversationStep(step) {
		this.#conversationSteps.push(step);
	}

	_setUsage(usage) {
		this.#usage = usage;
	}

	_setGit(git) {
		this.#git = git;
	}

	_setStatus(status) {
		if (this.#status === status) return;
		this.#status = status;
		for (const listener of this.#statusListeners) listener(status);
	}

	_settle({ status, result, error, durationMs }) {
		this.#result = result;
		this.#error = error;
		this.#durationMs = durationMs;
		this._setStatus(status);
		bump("runs", status);
		record("run_settled", { runId: this.#id, agentId: this.#agentId, status, error: error?.message });
		const waiters = this.#settleResolvers.splice(0);
		for (const resolve of waiters) resolve(this.#resultRecord());
		const changes = this.#changeResolvers.splice(0);
		for (const resolve of changes) resolve();
		const cancels = this.#cancelResolvers.splice(0);
		for (const resolve of cancels) resolve();
	}

	#resultRecord() {
		return {
			id: this.#id,
			requestId: this.#requestId,
			status: this.#status,
			...(this.#result !== undefined ? { result: this.#result } : {}),
			...(this.#error ? { error: this.#error } : {}),
			...(this.#model ? { model: this.#model } : {}),
			...(this.#durationMs !== undefined ? { durationMs: this.#durationMs } : {}),
			...(this.#git ? { git: this.#git } : {}),
			...(this.#usage ? { usage: this.#usage } : {}),
		};
	}

	async wait() {
		if (this.#status !== "running") return this.#resultRecord();
		return new Promise((resolve) => this.#settleResolvers.push(resolve));
	}

	async *stream() {
		let index = 0;
		for (;;) {
			while (index < this.#events.length) yield this.#events[index++];
			if (this.#status !== "running") return;
			await new Promise((resolve) => this.#changeResolvers.push(resolve));
		}
	}

	async conversation() {
		return [{ type: "agentConversation", turn: { steps: this.#conversationSteps.slice() } }];
	}

	async cancel() {
		if (this.#status !== "running") return;
		this.#cancelRequested = true;
		this.#abort.abort();
		record("run_cancel_requested", { runId: this.#id, agentId: this.#agentId });
		bump("runs", "cancel_requested");
		if (this.#status === "running") await new Promise((resolve) => this.#cancelResolvers.push(resolve));
	}

	onDidChangeStatus(listener) {
		this.#statusListeners.add(listener);
		return () => this.#statusListeners.delete(listener);
	}
}

// Replay a finished run's recorded outcome (used by Agent.getRun / listRuns).
function recordedRun(runRecord, agentId) {
	const run = new FakeRun({ id: runRecord.id, requestId: runRecord.requestId, agentId, model: runRecord.model, createdAt: runRecord.createdAt });
	run._settle({ status: runRecord.status, result: runRecord.result, error: runRecord.error, durationMs: runRecord.durationMs });
	return run;
}

const activeRuns = new Map(); // agentId -> FakeRun (in-process owner tracking)

async function deliverDelta(options, update) {
	record("delta", { update: update.type, callId: update.callId, text: update.text, bytes: update.text?.length });
	bump("deltas", update.type);
	if (typeof options.onDelta === "function") {
		try {
			await options.onDelta({ update });
		} catch (error) {
			COUNTERS.violations += 1;
			record("violation", { reason: "onDelta_threw", message: String(error?.message ?? error) });
		}
	}
}

async function deliverStep(options, step) {
	record("step", { type: step.type, text: step.message?.text, toolCall: step.type === "toolCall" ? step.message?.type : undefined });
	bump("steps", step.type);
	if (typeof options.onStep === "function") {
		try {
			await options.onStep({ step });
		} catch (error) {
			COUNTERS.violations += 1;
			record("violation", { reason: "onStep_threw", message: String(error?.message ?? error) });
		}
	}
}

function nativeToolCallOf(step) {
	const { toolCall, callId } = step;
	const { result: _ignored, ...started } = toolCall;
	return { started: { type: "tool-call-started", callId, toolCall: started }, completed: { type: "tool-call-completed", callId, toolCall } };
}

async function executeTurn({ run, agent, options, payload, turn, expectProblems, runtime, runIndex }) {
	const startedAt = Date.now();
	const steps = turn.steps ?? [];
	let text = "";
	let usage;
	let outcome = turn.outcome ?? { status: "finished" };
	let stepFailure;
	let turnUsageEmitted = false;
	run._push({ type: "system", subtype: "init", agent_id: agent.agentId, run_id: run.id, model: options.model, tools: [] });
	run._push({ type: "status", agent_id: agent.agentId, run_id: run.id, status: "RUNNING" });
	try {
		if (expectProblems.length > 0) throw new ConfigurationError(`fake scenario expectation failed: ${expectProblems.join("; ")}`, { code: "fake_expectation_failed" });
		for (const [stepIndex, step] of steps.entries()) {
			if (run.cancelRequested) break;
			switch (step.op) {
				case "thinking": {
					for (const chunk of step.chunks ?? []) {
						await deliverDelta(options, { type: "thinking-delta", text: chunk });
						run._push({ type: "thinking", agent_id: agent.agentId, run_id: run.id, text: chunk });
					}
					if (step.completed !== false) await deliverDelta(options, { type: "thinking-completed" });
					break;
				}
				case "text": {
					let chunkText = "";
					for (const chunk of step.chunks ?? []) {
						if (run.cancelRequested) break;
						await deliverDelta(options, { type: "text-delta", text: chunk });
						chunkText += chunk;
					}
					text += chunkText;
					run._push({ type: "assistant", agent_id: agent.agentId, run_id: run.id, message: { role: "assistant", content: [{ type: "text", text: chunkText }] } });
					if (step.step !== false) await deliverStep(options, { type: "assistantMessage", message: { text: chunkText } });
					run._addConversationStep({ type: "assistantMessage", message: { text: chunkText } });
					break;
				}
				case "nativeTool": {
					const { started, completed } = nativeToolCallOf(step);
					bump("nativeToolCalls", step.toolCall?.type ?? "unknown");
					record("native_tool", { phase: "started", callId: step.callId, name: step.toolCall?.type, args: step.toolCall?.args });
					await deliverDelta(options, { ...started, callId: step.callId });
					run._push({ type: "tool_call", agent_id: agent.agentId, run_id: run.id, call_id: step.callId, name: step.toolCall?.type, status: "running", args: step.toolCall?.args });
					if (step.phase !== "started-only") {
						record("native_tool", { phase: "completed", callId: step.callId, name: step.toolCall?.type, result: step.toolCall?.result });
						await deliverDelta(options, completed);
						run._push({ type: "tool_call", agent_id: agent.agentId, run_id: run.id, call_id: step.callId, name: step.toolCall?.type, status: "completed", args: step.toolCall?.args, result: step.toolCall?.result });
						await deliverStep(options, { type: "toolCall", id: step.callId, message: step.toolCall });
					}
					run._addConversationStep({ type: "toolCall", id: step.callId, message: step.toolCall });
					break;
				}
				case "mcpCall": {
					const serverName = step.server ?? "pi_tools";
					const server = options.mcpServers?.[serverName];
					if (!server?.url) throw new ConfigurationError(`mcp server ${serverName} is not configured for this send`, { code: "mcp_server_missing" });
					const started = { type: "mcp", args: { providerIdentifier: serverName, toolName: step.tool, args: step.arguments ?? {} } };
					await deliverDelta(options, { type: "tool-call-started", callId: step.callId, toolCall: started });
					let completedCall;
					try {
						const { toolName, result } = await callBridgeTool({
							url: server.url,
							serverName,
							toolName: step.tool,
							args: step.arguments ?? {},
							callId: step.callId,
							signal: run.abortSignal,
						});
						const sdkResult = mcpResultToSdk(result);
						bump("toolExecutions", `${serverName}/${toolName}`);
						record("tool_execution", {
							source: "bridge",
							server: serverName,
							tool: toolName,
							callId: step.callId,
							isError: result?.isError === true,
							text: (result?.content ?? []).filter((item) => item.type === "text").map((item) => item.text).join("\n"),
						});
						completedCall = { type: "mcp", args: { providerIdentifier: serverName, toolName, args: step.arguments ?? {} }, result: sdkResult };
					} catch (error) {
						if (run.cancelRequested) throw error;
						completedCall = { type: "mcp", args: started.args, result: { status: "error", error: { message: String(error?.message ?? error) } } };
						record("tool_execution", { source: "bridge", server: serverName, tool: step.tool, callId: step.callId, failed: String(error?.message ?? error) });
					}
					await deliverDelta(options, { type: "tool-call-completed", callId: step.callId, toolCall: completedCall });
					run._push({ type: "tool_call", agent_id: agent.agentId, run_id: run.id, call_id: step.callId, name: "mcp", status: completedCall.result.status === "success" ? "completed" : "error", args: started.args, result: completedCall.result });
					await deliverStep(options, { type: "toolCall", id: step.callId, message: completedCall });
					run._addConversationStep({ type: "toolCall", id: step.callId, message: completedCall });
					break;
				}
				case "usage": {
					usage = { inputTokens: step.usage.inputTokens, outputTokens: step.usage.outputTokens, cacheReadTokens: step.usage.cacheReadTokens ?? 0, cacheWriteTokens: step.usage.cacheWriteTokens ?? 0, totalTokens: step.usage.totalTokens ?? step.usage.inputTokens + step.usage.outputTokens, ...(step.usage.reasoningTokens !== undefined ? { reasoningTokens: step.usage.reasoningTokens } : {}) };
					break;
				}
				case "delay": {
					await new Promise((resolve) => setTimeout(resolve, step.ms ?? 0));
					break;
				}
				case "hang": {
					if (!run.abortSignal.aborted) await new Promise((resolve) => run.abortSignal.addEventListener("abort", resolve, { once: true }));
					break;
				}
				case "error": {
					stepFailure = { message: step.message ?? "fake scenario error", ...(step.code ? { code: step.code } : {}) };
					break;
				}
				default:
					throw new ConfigurationError(`unknown fake scenario op ${String(step.op)} at step ${stepIndex}`, { code: "fake_scenario_invalid" });
			}
			if (stepFailure) break;
		}
	} catch (error) {
		stepFailure = toRunError(error);
	}
	// Turn end: usage is reported once per turn, as the SDK does at turn end.
	if (usage) {
		await deliverDelta(options, { type: "turn-ended", usage: { inputTokens: usage.inputTokens, outputTokens: usage.outputTokens, cacheReadTokens: usage.cacheReadTokens, cacheWriteTokens: usage.cacheWriteTokens, ...(usage.reasoningTokens !== undefined ? { reasoningTokens: usage.reasoningTokens } : {}) } });
		run._push({ type: "usage", agent_id: agent.agentId, run_id: run.id, usage });
		turnUsageEmitted = true;
	} else {
		await deliverDelta(options, { type: "turn-ended" });
	}
	let status;
	let error;
	if (run.cancelRequested) status = "cancelled";
	else if (stepFailure) {
		status = "error";
		error = stepFailure;
	} else if (outcome.status === "error") {
		status = "error";
		error = outcome.error ?? { message: "fake scenario outcome error" };
	} else if (outcome.status === "cancelled") {
		status = "cancelled";
	} else {
		status = "finished";
	}
	const resultText = status === "finished" ? (outcome.result ?? text) : undefined;
	const runUsage = usage ? { ...usage } : undefined;
	if (runUsage) run._setUsage(runUsage);
	if (turn.git) run._setGit(turn.git);
	else if (runtime.git) run._setGit(runtime.git);
	run._push({ type: "status", agent_id: agent.agentId, run_id: run.id, status: status === "finished" ? "FINISHED" : status === "cancelled" ? "CANCELLED" : "ERROR", ...(error ? { message: error.message } : {}) });
	agent._recordTurn({ runId: run.id, usage: runUsage, status, text: resultText, prompt: payload.text, runIndex, turnUsageEmitted, cost: turn.cost });
	run._settle({ status, result: resultText, error, durationMs: Date.now() - startedAt, ...(options.model ? { model: options.model } : {}) });
}

// ---------------------------------------------------------------- SDK agent

class FakeAgent {
	#agentId;
	#model;
	#cloud;
	#storeKey;
	#closed = false;
	#cwd;
	#mcpServers;

	constructor({ agentId, model, cloud, storeKey, cwd, mcpServers }) {
		this.#agentId = agentId;
		this.#model = model;
		this.#cloud = cloud;
		this.#storeKey = storeKey;
		this.#cwd = cwd;
		this.#mcpServers = mcpServers;
	}

	get agentId() { return this.#agentId; }
	get model() { return this.#model; }

	async send(message, sendOptions = {}) {
		const api = "agent.send";
		const payload = validatePayload(message);
		// Agent-level mcpServers (from Agent.create/resume) apply unless a send overrides them.
		const options = { ...sendOptions, mcpServers: sendOptions.mcpServers ?? this.#mcpServers };
		const cloudSend = options.cloud ?? undefined;
		record("sdk_call", { api, agentId: this.#agentId, runtime: this.#cloud ? "cloud" : "local", message: payload.text, imageCount: payload.images.length, images: payload.images.map((image) => ({ mimeType: image.mimeType, url: image.url, dimension: image.dimension })), options: { mode: options.mode, model: options.model, mcpServers: options.mcpServers, local: options.local, cloud: cloudSend ? { envVars: cloudSend.envVars ? Object.keys(cloudSend.envVars) : undefined } : undefined, idempotencyKey: options.idempotencyKey, onDelta: typeof options.onDelta, onStep: typeof options.onStep } });
		bump("sdkCalls", api);
		if (this.#closed) throw new ConfigurationError("agent is closed", { code: "agent_closed" });
		const current = activeRuns.get(this.#agentId);
		if (current && current.status === "running" && !options.local?.force) {
			throw new AgentBusyError(`agent ${this.#agentId} already has an active run`, { code: "agent_busy" });
		}
		const { turn, index, runNumber } = withState((state) => ({ ...takeTurn(state), runNumber: nextSeq(state, "run") }));
		const expectProblems = assertExpectations(turn.expect, payload, options, this.#agentId);
		const runId = `run-${fakeUuid(runNumber)}`;
		const run = new FakeRun({ id: runId, requestId: `req-${runNumber}`, agentId: this.#agentId, model: options.model ?? this.#model, createdAt: runNumber });
		activeRuns.set(this.#agentId, run);
		this.#recordUserMessage(payload.text, runId);
		if (options.model) this.#setModel(options.model);
		record("run", { runId, agentId: this.#agentId, turnIndex: index, scenarioTurn: turn.name ?? index });
		if (turn.sendError) {
			const ErrorClass = ERROR_CLASSES[turn.sendError.name] ?? CursorAgentError;
			activeRuns.delete(this.#agentId);
			throw new ErrorClass(turn.sendError.message, { code: turn.sendError.code, status: turn.sendError.status });
		}
		const git = this.#cloud ? cloudGit(loadState().cloud[this.#agentId], turn) : undefined;
		setImmediate(() => {
			executeTurn({ run, agent: this, options, payload, turn, expectProblems, runtime: { git }, runIndex: index })
				.catch((error) => {
					run._settle({ status: "error", error: toRunError(error) });
				})
				.finally(() => {
					if (activeRuns.get(this.#agentId) === run) activeRuns.delete(this.#agentId);
				});
		});
		return run;
	}

	#setModel(model) {
		this.#model = model;
		if (!this.#cloud) withState((state) => { const rec = localBucket(state, this.#storeKey).agents[this.#agentId]; if (rec) rec.model = model; });
	}

	#recordUserMessage(text, runId) {
		if (this.#cloud) return;
		withState((state) => {
			const rec = localBucket(state, this.#storeKey).agents[this.#agentId];
			if (!rec) return;
			rec.transcript.push({ type: "user", uuid: `${runId}-user`, agent_id: this.#agentId, message: { role: "user", content: [{ type: "text", text }] } });
			rec.status = "running";
		});
	}

	_recordTurn({ runId, usage, status, text, prompt, cost }) {
		const runUsage = usage ? { runId, usage, ...(cost ? { cost } : {}) } : undefined;
		withState((state) => {
			const bucket = this.#cloud ? state.cloud : localBucket(state, this.#storeKey).agents;
			const rec = this.#cloud ? state.cloud[this.#agentId] : bucket[this.#agentId];
			if (!rec) return;
			rec.runs.push({ id: runId, status, result: text, usage: runUsage, prompt });
			if (runUsage) rec.usageRuns.push(runUsage);
			if (!this.#cloud && text) rec.transcript.push({ type: "assistant", uuid: `${runId}-assistant`, agent_id: this.#agentId, message: { role: "assistant", content: [{ type: "text", text }] } });
			rec.status = status === "finished" ? "idle" : status === "cancelled" ? "idle" : "error";
			rec.updatedAt = rec.runs.length;
		});
	}

	close() {
		record("sdk_call", { api: "agent.close", agentId: this.#agentId });
		bump("sdkCalls", "agent.close");
		this.#closed = true;
	}

	async reload() {
		record("sdk_call", { api: "agent.reload", agentId: this.#agentId });
		bump("sdkCalls", "agent.reload");
	}

	async [Symbol.asyncDispose]() {
		record("sdk_call", { api: "agent.asyncDispose", agentId: this.#agentId });
		bump("sdkCalls", "agent.asyncDispose");
		this.#closed = true;
	}

	async listArtifacts() {
		record("sdk_call", { api: "agent.listArtifacts", agentId: this.#agentId });
		bump("sdkCalls", "agent.listArtifacts");
		return (SCENARIO.cloud?.artifacts ?? []).map((artifact) => ({ path: artifact.path, sizeBytes: artifact.sizeBytes ?? Buffer.byteLength(artifact.content ?? ""), updatedAt: artifact.updatedAt ?? "2026-01-01T00:00:00.000Z" }));
	}

	async downloadArtifact(path) {
		record("sdk_call", { api: "agent.downloadArtifact", agentId: this.#agentId, path });
		bump("sdkCalls", "agent.downloadArtifact");
		const artifact = (SCENARIO.cloud?.artifacts ?? []).find((candidate) => candidate.path === path);
		if (!artifact) throw new AgentNotFoundError(`artifact not found: ${path}`, { code: "artifact_not_found" });
		return Buffer.from(artifact.content ?? "", "utf8");
	}

	async getUsage(options = {}) {
		record("sdk_call", { api: "agent.getUsage", agentId: this.#agentId, runId: options.runId });
		bump("sdkCalls", "agent.getUsage");
		return usageFor(this.#cloud ? "cloud" : this.#storeKey, this.#agentId, this.#cloud, options.runId);
	}
}

function cloudGit(cloudRecord, turn) {
	const repo = cloudRecord?.repos?.[0];
	if (!repo) return undefined;
	return { branches: [{ repoUrl: repo.url, branch: repo.startingRef ?? "main", ...(turn.prUrl ? { prUrl: turn.prUrl } : {}) }] };
}

function usageFor(key, agentId, isCloud, runId) {
	const state = loadState();
	const rec = isCloud ? state.cloud[agentId] : state.local[key]?.agents?.[agentId];
	if (!rec) throw new AgentNotFoundError(`agent ${agentId} not found`, { code: "agent_not_found" });
	const runs = (rec.usageRuns ?? []).filter((entry) => runId === undefined || entry.runId === runId);
	const total = sumUsage(runs.map((entry) => entry.usage));
	const cost = sumCost(runs.map((entry) => entry.cost));
	return { usage: total, ...(cost ? { cost } : {}), runs };
}

function sumUsage(list) {
	const out = { inputTokens: 0, outputTokens: 0, cacheReadTokens: 0, cacheWriteTokens: 0, totalTokens: 0 };
	for (const usage of list) {
		for (const field of Object.keys(out)) out[field] += usage[field] ?? 0;
	}
	return out;
}

function sumCost(list) {
	const costs = list.filter(Boolean);
	if (costs.length === 0) return undefined;
	return costs.reduce((acc, cost) => ({ rawCostCents: acc.rawCostCents + cost.rawCostCents, chargedCents: acc.chargedCents + cost.chargedCents }), { rawCostCents: 0, chargedCents: 0 });
}

// ---------------------------------------------------------------- Agent / Cursor namespaces

function localRecordView(agentId, rec, cwd) {
	return {
		agentId,
		name: rec.name ?? "Fake local agent",
		summary: rec.summary ?? "",
		lastModified: rec.updatedAt ?? 0,
		status: rec.status === "archived" ? undefined : rec.status === "error" ? "error" : rec.status === "running" ? "running" : "finished",
		archived: rec.status === "archived",
		createdAt: rec.createdAt ?? 0,
		runtime: "local",
		cwd: cwd ?? rec.cwd,
	};
}

function cloudRecordView(agentId, rec) {
	const lastRun = rec.runs[rec.runs.length - 1];
	return {
		agentId,
		name: rec.name ?? "Fake cloud agent",
		summary: lastRun?.result ?? "",
		lastModified: rec.updatedAt ?? 0,
		status: lastRun ? (lastRun.status === "finished" ? "finished" : lastRun.status === "error" ? "error" : "running") : undefined,
		archived: rec.archived === true,
		createdAt: rec.createdAt ?? 0,
		runtime: "cloud",
		env: rec.env,
		repos: rec.repos,
		metadata: rec.metadata,
	};
}

function resolveStoreFromOptions(options) {
	return options?.store ?? options?.local?.store;
}

export class Agent {
	constructor() {
		throw new ConfigurationError("Agent has a private constructor");
	}

	static async create(options) {
		record("sdk_call", { api: "Agent.create", options });
		bump("sdkCalls", "Agent.create");
		if (!options || typeof options !== "object") throw new ConfigurationError("Agent.create requires options", { code: "invalid_options" });
		if (options.cloud) {
			resolveApiKey(options);
			const agentId = withState((state) => {
				const n = nextSeq(state, "cloud");
				const id = `bc-${fakeUuid(n)}`;
				state.cloud[id] = {
					name: options.name,
					createdAt: n,
					updatedAt: n,
					archived: false,
					status: "idle",
					runs: [],
					usageRuns: [],
					repos: (options.cloud.repos ?? []).map((repo) => ({ url: repo.url, startingRef: repo.startingRef })),
					env: options.cloud.env,
					metadata: options.cloud.metadata,
					envVarNames: Object.keys(options.cloud.envVars ?? {}),
				};
				return id;
			});
			return new FakeAgent({ agentId, model: options.model, cloud: true, storeKey: "cloud", mcpServers: options.mcpServers });
		}
		if (!options.model?.id) throw new ConfigurationError("model is required for local agents", { code: "model_required" });
		const key = storeKey(resolveStoreFromOptions(options));
		const agentId = withState((state) => {
			const n = nextSeq(state, "local");
			const id = fakeUuid(n);
			localBucket(state, key).agents[id] = {
				name: options.name,
				cwd: options.local?.cwd ?? process.cwd(),
				model: options.model,
				status: "idle",
				createdAt: n,
				updatedAt: n,
				runs: [],
				usageRuns: [],
				transcript: [],
				mode: options.mode ?? "agent",
			};
			return id;
		});
		return new FakeAgent({ agentId, model: options.model, cloud: false, storeKey: key, cwd: options.local?.cwd, mcpServers: options.mcpServers });
	}

	static async resume(agentId, options = {}) {
		record("sdk_call", { api: "Agent.resume", agentId, options });
		bump("sdkCalls", "Agent.resume");
		if (isCloudAgentId(agentId)) {
			const state = loadState();
			if (!state.cloud[agentId]) throw new AgentNotFoundError(`cloud agent ${agentId} not found`, { code: "agent_not_found" });
			return new FakeAgent({ agentId, model: options.model, cloud: true, storeKey: "cloud", mcpServers: options.mcpServers });
		}
		const key = storeKey(resolveStoreFromOptions(options));
		const state = loadState();
		const rec = state.local[key]?.agents?.[agentId];
		if (!rec || rec.status === "archived") throw new AgentNotFoundError(`local agent ${agentId} not found`, { code: "agent_not_found" });
		return new FakeAgent({ agentId, model: options.model ?? rec.model, cloud: false, storeKey: key, cwd: rec.cwd, mcpServers: options.mcpServers });
	}

	static async prompt(message, options) {
		const agent = await Agent.create(options);
		try {
			const run = await agent.send(message);
			return await run.wait();
		} finally {
			await agent[Symbol.asyncDispose]();
		}
	}

	static async list(options = {}) {
		record("sdk_call", { api: "Agent.list", options });
		bump("sdkCalls", "Agent.list");
		if (options.runtime === "cloud") {
			const state = loadState();
			const items = Object.entries(state.cloud)
				.filter(([, rec]) => options.includeArchived || rec.archived !== true)
				.map(([id, rec]) => cloudRecordView(id, rec));
			return { items };
		}
		const key = storeKey(options.store);
		const state = loadState();
		const items = Object.entries(state.local[key]?.agents ?? {})
			.filter(([, rec]) => rec.status !== "archived" && (!options.cwd || rec.cwd === options.cwd))
			.map(([id, rec]) => localRecordView(id, rec, options.cwd));
		return { items };
	}

	static async listRuns(agentId, options = {}) {
		record("sdk_call", { api: "Agent.listRuns", agentId, options: { runtime: options.runtime, limit: options.limit } });
		bump("sdkCalls", "Agent.listRuns");
		const state = loadState();
		const rec = isCloudAgentId(agentId) ? state.cloud[agentId] : state.local[storeKey(options.store)]?.agents?.[agentId];
		if (!rec) throw new AgentNotFoundError(`agent ${agentId} not found`, { code: "agent_not_found" });
		return { items: rec.runs.map((runRecord) => recordedRun(runRecord, agentId)) };
	}

	static async getRun(runId, options = {}) {
		record("sdk_call", { api: "Agent.getRun", runId, options: { runtime: options.runtime, agentId: options.agentId } });
		bump("sdkCalls", "Agent.getRun");
		const state = loadState();
		const search = [...Object.entries(state.cloud), ...Object.entries(state.local[storeKey(options.store)]?.agents ?? {})];
		for (const [agentId, rec] of search) {
			const found = rec.runs.find((candidate) => candidate.id === runId);
			if (found) return recordedRun(found, agentId);
		}
		throw new AgentNotFoundError(`run ${runId} not found`, { code: "run_not_found" });
	}

	static async cancelRun(runId) {
		record("sdk_call", { api: "Agent.cancelRun", runId });
		bump("sdkCalls", "Agent.cancelRun");
		for (const run of activeRuns.values()) {
			if (run.id === runId) return run.cancel();
		}
	}

	static async get(agentId, options = {}) {
		record("sdk_call", { api: "Agent.get", agentId, options: { apiKey: options.apiKey ? "<redacted>" : undefined, cwd: options.cwd } });
		bump("sdkCalls", "Agent.get");
		const state = loadState();
		if (isCloudAgentId(agentId)) {
			resolveApiKey(options);
			const rec = state.cloud[agentId];
			if (!rec) throw new AgentNotFoundError(`cloud agent ${agentId} not found`, { code: "agent_not_found" });
			return cloudRecordView(agentId, rec);
		}
		const rec = state.local[storeKey(options.store)]?.agents?.[agentId];
		if (!rec) throw new AgentNotFoundError(`local agent ${agentId} not found`, { code: "agent_not_found" });
		return localRecordView(agentId, rec, options.cwd);
	}

	static async archive(agentId, options = {}) {
		record("sdk_call", { api: "Agent.archive", agentId, options: { apiKey: options.apiKey ? "<redacted>" : undefined } });
		bump("sdkCalls", "Agent.archive");
		if (isCloudAgentId(agentId)) cloudFailure("archive");
		withState((state) => {
			if (isCloudAgentId(agentId)) {
				resolveApiKey(options);
				if (!state.cloud[agentId]) throw new AgentNotFoundError(`cloud agent ${agentId} not found`, { code: "agent_not_found" });
				state.cloud[agentId].archived = true;
				return;
			}
			const rec = state.local[storeKey(options.store)]?.agents?.[agentId];
			if (!rec) throw new AgentNotFoundError(`local agent ${agentId} not found`, { code: "agent_not_found" });
			rec.status = "archived";
		});
	}

	static async unarchive(agentId, options = {}) {
		record("sdk_call", { api: "Agent.unarchive", agentId, options: { apiKey: options.apiKey ? "<redacted>" : undefined } });
		bump("sdkCalls", "Agent.unarchive");
		withState((state) => {
			if (isCloudAgentId(agentId)) {
				resolveApiKey(options);
				if (!state.cloud[agentId]) throw new AgentNotFoundError(`cloud agent ${agentId} not found`, { code: "agent_not_found" });
				state.cloud[agentId].archived = false;
				return;
			}
			const rec = state.local[storeKey(options.store)]?.agents?.[agentId];
			if (!rec) throw new AgentNotFoundError(`local agent ${agentId} not found`, { code: "agent_not_found" });
			rec.status = "idle";
		});
	}

	static async delete(agentId, options = {}) {
		record("sdk_call", { api: "Agent.delete", agentId, options: { apiKey: options.apiKey ? "<redacted>" : undefined } });
		bump("sdkCalls", "Agent.delete");
		if (isCloudAgentId(agentId)) cloudFailure("delete");
		withState((state) => {
			if (isCloudAgentId(agentId)) {
				resolveApiKey(options);
				if (!state.cloud[agentId]) throw new AgentNotFoundError(`cloud agent ${agentId} not found`, { code: "agent_not_found" });
				delete state.cloud[agentId];
				return;
			}
			const bucket = localBucket(state, storeKey(options.store)).agents;
			if (!bucket[agentId]) throw new AgentNotFoundError(`local agent ${agentId} not found`, { code: "agent_not_found" });
			delete bucket[agentId];
		});
	}

	static async getUsage(agentId, options = {}) {
		record("sdk_call", { api: "Agent.getUsage", agentId, runId: options.runId, options: { apiKey: options.apiKey ? "<redacted>" : undefined } });
		bump("sdkCalls", "Agent.getUsage");
		if (isCloudAgentId(agentId)) resolveApiKey(options);
		return usageFor(storeKey(options.store), agentId, isCloudAgentId(agentId), options.runId);
	}

	static messages = {
		async list(agentId, options = {}) {
			record("sdk_call", { api: "Agent.messages.list", agentId, options: { runtime: options.runtime, limit: options.limit, offset: options.offset } });
			bump("sdkCalls", "Agent.messages.list");
			const state = loadState();
			const rec = state.local[storeKey(options.store)]?.agents?.[agentId];
			if (!rec) throw new AgentNotFoundError(`local agent ${agentId} not found`, { code: "agent_not_found" });
			const offset = options.offset ?? 0;
			const limit = options.limit ?? rec.transcript.length;
			return rec.transcript.slice(offset, offset + limit);
		},
	};
}

function cursorModels() {
	return SCENARIO.models ?? [];
}

export class Cursor {
	constructor() {
		throw new ConfigurationError("Cursor has a private constructor");
	}

	static configure(options) {
		record("sdk_call", { api: "Cursor.configure", options });
		bump("sdkCalls", "Cursor.configure");
		Object.assign(CONFIG, options?.local ? { local: { ...CONFIG.local, ...options.local } } : {});
	}

	static async me(options = {}) {
		record("sdk_call", { api: "Cursor.me", apiKey: options.apiKey ? "<redacted>" : undefined });
		bump("sdkCalls", "Cursor.me");
		resolveApiKey(options);
		return { apiKeyName: "fake-api-key", createdAt: "2026-01-01T00:00:00.000Z", ...(SCENARIO.account ?? {}) };
	}

	static models = {
		async list(options = {}) {
			record("sdk_call", { api: "Cursor.models.list", apiKey: options.apiKey ? "<redacted>" : undefined });
			bump("sdkCalls", "Cursor.models.list");
			resolveApiKey(options);
			if (SCENARIO.modelsError) {
				const ErrorClass = ERROR_CLASSES[SCENARIO.modelsError.name] ?? CursorAgentError;
				throw new ErrorClass(SCENARIO.modelsError.message, { code: SCENARIO.modelsError.code, status: SCENARIO.modelsError.status });
			}
			return cursorModels();
		},
	};

	static repositories = {
		async list(options = {}) {
			record("sdk_call", { api: "Cursor.repositories.list", apiKey: options.apiKey ? "<redacted>" : undefined });
			bump("sdkCalls", "Cursor.repositories.list");
			resolveApiKey(options);
			return SCENARIO.repositories ?? [];
		},
	};

	static auth = {
		async login() {
			record("sdk_call", { api: "Cursor.auth.login" });
			bump("sdkCalls", "Cursor.auth.login");
			throw new ConfigurationError("fake SDK does not perform interactive login", { code: "fake_login_unsupported" });
		},
		async logout() {
			record("sdk_call", { api: "Cursor.auth.logout" });
			bump("sdkCalls", "Cursor.auth.logout");
		},
		async status() {
			record("sdk_call", { api: "Cursor.auth.status" });
			bump("sdkCalls", "Cursor.auth.status");
			return { status: "logged-out" };
		},
	};
}

const CONFIG = { local: { store: undefined, useHttp1ForAgent: undefined, workspaceScanCacheTtlMs: undefined } };
export function configureCursorSdk(options) {
	Cursor.configure(options);
}
export function getDefaultSdkStateRoot() {
	return STATE_HOME ?? join(tmpdir(), "rpi-cursor-fake-sdk-unset-state-root");
}
export function getDefaultUseHttp1ForAgent() {
	return CONFIG.local.useHttp1ForAgent ?? undefined;
}
export function getDefaultWorkspaceScanCacheTtlMs() {
	return CONFIG.local.workspaceScanCacheTtlMs ?? undefined;
}

export const LOCAL_RUN_STREAM_SCHEMA_VERSION = 1;
export const LOCAL_RUN_STREAM_EVENT_TYPE = "run_stream_event";
export const DEFAULT_LOGIN_API_KEY_TTL_MS = 30 * 24 * 60 * 60 * 1000;

export function createAgentPlatform() {
	record("sdk_call", { api: "createAgentPlatform" });
	bump("sdkCalls", "createAgentPlatform");
	return Promise.resolve({
		checkpointStore: {
			async loadLatest() {
				return undefined;
			},
		},
	});
}

// ---------------------------------------------------------------- local store (@cursor/sdk/sqlite)

export class SqliteLocalAgentStore {
	#stateRoot;
	#workspaceRef;

	constructor(stateRoot, workspaceRef) {
		this.#stateRoot = stateRoot;
		this.#workspaceRef = workspaceRef;
	}

	get stateRoot() { return this.#stateRoot; }
	get workspaceRef() { return this.#workspaceRef; }

	static async open(options = {}) {
		// The adapter derives the store directory from a hash of the session ID, which
		// changes per run; logs keep only the stable part so runs can be compared.
		const stateRootLabel = String(options.stateRoot ?? "").replace(/pi-sessions\/[0-9a-f]{32}/, "pi-sessions/<scope>");
		record("sdk_call", { api: "SqliteLocalAgentStore.open", workspaceRef: options.workspaceRef, stateRoot: stateRootLabel });
		bump("sdkCalls", "SqliteLocalAgentStore.open");
		const store = new SqliteLocalAgentStore(options.stateRoot ?? getDefaultSdkStateRoot(), options.workspaceRef);
		store.agents = {
			async get({ agentId }) {
				return loadState().local[store.stateRoot]?.agents?.[agentId];
			},
			async list() {
				return { items: Object.values(loadState().local[store.stateRoot]?.agents ?? {}) };
			},
		};
		store.runs = {
			async list() {
				return { items: [] };
			},
		};
		store.checkpoints = {
			async get() {
				return undefined;
			},
		};
		return store;
	}

	async dispose() {
		record("sdk_call", { api: "SqliteLocalAgentStore.dispose", stateRoot: this.#stateRoot.replace(/pi-sessions\/[0-9a-f]{32}/, "pi-sessions/<scope>") });
		bump("sdkCalls", "SqliteLocalAgentStore.dispose");
	}
}

// Exposed for the contract test and the README table; not part of the SDK surface.
export const __fake = Object.freeze({ scenario: SCENARIO, counters: COUNTERS, fakeUuid, errorClasses: ERROR_CLASSES });
