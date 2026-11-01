// rpi-claude-agent worker: one Claude Agent SDK query per rpi session, driven over
// the rpi-cursor-sdk worker wire (length-prefixed JSON frames on a private socket).
//
// Claude Code's own tools are off. rpi's tools are an in-process MCP server: when
// Claude calls one, the handler stays pending, the rpi stream ends with a toolCall,
// and the next stream call's toolResult resolves it so the same query continues.
//
// Each rpi session keeps one saved Claude Code session. A record of checkpoints
// (rpi history hash -> Claude message uuid) lets a new query resume it, or fork
// it after an edit, instead of replaying the whole history as text.
import { createHash, randomUUID } from "node:crypto";
import { mkdirSync, readFileSync, renameSync, writeFileSync } from "node:fs";
import { connect } from "node:net";
import { dirname, join } from "node:path";
import { z } from "zod";

// Tests inject a fake SDK by path; production loads the pinned package.
const sdk = await import(process.env.RPI_CLAUDE_AGENT_SDK ?? "@anthropic-ai/claude-agent-sdk");

const API = "claude-agent-sdk";
const PROVIDER = "claude-code";
const SERVER = "rpi";
const PREFIX = `mcp__${SERVER}__`;
const CAPACITY = 64;
const PAYLOAD_BYTES = 4 * 1024 * 1024;
// An idle query stops after this; the next request resumes its saved session.
const IDLE_MS = Number(process.env.RPI_CLAUDE_AGENT_IDLE_MS) || 5 * 60_000;
const STOP_MS = 3000;

// ---- wire ------------------------------------------------------------------

function encode(value) {
	const json = Buffer.from(JSON.stringify(value));
	const frame = Buffer.alloc(json.length + 4);
	frame.writeUInt32BE(json.length);
	json.copy(frame, 4);
	return frame;
}

async function readBootstrap() {
	let text = "";
	for await (const chunk of process.stdin) text += chunk;
	const config = JSON.parse(text);
	if (config.version !== 1 || !/^[a-f0-9]{32}$/.test(config.generation) || !/^[a-f0-9]{64}$/.test(config.token)) throw new Error("Invalid worker bootstrap");
	return config;
}

function serve(config, handle) {
	const socket = connect({ host: "127.0.0.1", port: config.port });
	socket.setNoDelay(true);
	const envelope = id => ({ version: 1, generation: config.generation, id });
	const calls = new Map();
	let authenticated = false;
	let buffer = Buffer.alloc(0);
	const send = value => socket.write(encode(value));
	socket.on("connect", () => send({ version: 1, generation: config.generation, token: config.token }));
	socket.on("data", chunk => {
		buffer = Buffer.concat([buffer, chunk]);
		while (buffer.length >= 4 && buffer.length >= 4 + buffer.readUInt32BE(0)) {
			const size = buffer.readUInt32BE(0);
			const frame = JSON.parse(buffer.subarray(4, 4 + size).toString("utf8"));
			buffer = buffer.subarray(4 + size);
			if (frame.generation !== config.generation) return socket.destroy();
			if (!authenticated) { authenticated = frame.ready === true; continue; }
			if (frame.kind === "request") start(frame);
			else if (frame.kind === "credit") calls.get(frame.id)?.credit(frame.sequence);
			else if (frame.kind === "cancel") calls.get(frame.id)?.controller.abort();
		}
	});
	const closed = new Promise(resolve => socket.on("close", resolve));
	socket.on("error", () => {});

	function start(frame) {
		const controller = new AbortController();
		const owed = new Map();
		let sequence = 0, bytes = 0, wake;
		const call = {
			controller,
			credit(seq) { bytes -= owed.get(seq) ?? 0; owed.delete(seq); wake?.(); },
		};
		calls.set(frame.id, call);
		// Never exceed the host's per-call event credits (count and bytes).
		const emit = async event => {
			const encoded = encode({ ...envelope(frame.id), kind: "event", sequence: sequence + 1, event });
			const weight = Math.min(PAYLOAD_BYTES, Math.max(1, encoded.length - 4));
			while (owed.size >= CAPACITY || bytes + weight > PAYLOAD_BYTES) await new Promise(resolve => { wake = resolve; });
			sequence++;
			owed.set(sequence, weight);
			bytes += weight;
			socket.write(encoded);
		};
		handle(frame.method, frame.input, { emit, signal: controller.signal }).then(
			result => send({ ...envelope(frame.id), kind: "reply", result: result ?? null, lastEvent: sequence }),
			error => send({ ...envelope(frame.id), kind: "error", error: String(error?.message ?? error), lastEvent: sequence }),
		).finally(() => calls.delete(frame.id));
	}
	return closed;
}

// ---- small helpers ---------------------------------------------------------

function channel() {
	const items = [];
	let wake;
	return {
		push(item) { items.push(item); wake?.(); wake = undefined; },
		async next() { while (!items.length) await new Promise(resolve => { wake = resolve; }); return items.shift(); },
	};
}

const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));

function deferred() {
	let resolve;
	const promise = new Promise(r => { resolve = r; });
	return { promise, resolve };
}

const hash = value => createHash("sha256").update(JSON.stringify(value)).digest("hex");

function text(content) {
	if (typeof content === "string") return content;
	return (content ?? []).filter(block => block.type === "text").map(block => block.text).join("\n");
}

function userContent(content) {
	if (typeof content === "string") return content;
	return content.map(block => block.type === "image"
		? { type: "image", source: { type: "base64", media_type: block.mimeType, data: block.data } }
		: { type: "text", text: block.text ?? "" });
}

/**
 * What Claude has not seen, as one user message: the new user input as is, after
 * any earlier turns (replies from another model, a fresh start) rendered as text
 * without thinking inside <conversation-checkpoint>, as OpenCode does.
 */
// shortcut: images inside the checkpoint are dropped (only the trailing user input keeps them), keep them as blocks when someone needs it.
function catchUp(missed) {
	let end = missed.length;
	while (end > 0 && missed[end - 1].role === "user") end--;
	const users = missed.slice(end).map(message => userContent(message.content));
	if (end === 0 && users.length === 1) return users[0];
	const lines = missed.slice(0, end).map(message => {
		if (message.role === "user") return `[user]\n${text(message.content)}`;
		if (message.role === "toolResult") return `[tool result ${message.toolName} ${message.toolCallId}${message.isError ? " error" : ""}]\n${text(message.content)}`;
		return `[assistant]\n${message.content.map(block => block.type === "toolCall"
			? `[tool call ${block.name} ${block.id}] ${JSON.stringify(block.arguments)}`
			: block.type === "text" ? block.text : "").filter(Boolean).join("\n")}`;
	});
	const blocks = [];
	if (end > 0) blocks.push({ type: "text", text: `<conversation-checkpoint>\nThe following is conversation history from rpi that you have not seen. Treat it as prior conversation, not a new request. Continue with the user messages after this checkpoint.\n\n${lines.join("\n\n")}\n</conversation-checkpoint>` });
	for (const content of users) blocks.push(...(typeof content === "string" ? [{ type: "text", text: content }] : content));
	if (!users.length) blocks.push({ type: "text", text: "Continue from where the conversation left off." });
	return blocks;
}

// ---- checkpoints -----------------------------------------------------------

function canonical(value) {
	if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
	if (value && typeof value === "object") return `{${Object.keys(value).sort().filter(key => value[key] !== undefined).map(key => `${JSON.stringify(key)}:${canonical(value[key])}`).join(",")}}`;
	return JSON.stringify(value) ?? "null";
}

/** What both histories must agree on: not thinking, usage, timestamps or the model. */
function visible(message) {
	const content = typeof message.content === "string" ? [{ type: "text", text: message.content }] : message.content ?? [];
	const parts = content.filter(block => block.type !== "thinking" && !(block.type === "text" && !block.text)).map(block =>
		block.type === "text" ? block.text
			: block.type === "toolCall" ? { call: block.id, name: block.name, arguments: block.arguments }
				: block.type === "image" ? { image: block.mimeType, data: block.data } : block);
	return { role: message.role, parts, id: message.toolCallId, error: message.role === "toolResult" ? !!message.isError : undefined };
}

const link = (previous, message) => createHash("sha256").update(previous).update(canonical(visible(message))).digest("hex");

/** hashes[n] identifies the first n messages of rpi's history. */
function chain(messages) {
	const hashes = [""];
	for (const message of messages) hashes.push(link(hashes.at(-1), message));
	return hashes;
}

/** Where `point`'s history continues in `messages`, if it is a prefix with something after it. */
function after(point, messages, hashes) {
	if (!point || point.n >= messages.length || hashes[point.n] !== point.hash) return;
	let start = point.n;
	// After an interrupted reply, Claude's session holds its own partial reply, not rpi's copy.
	while (point.interrupted && messages[start]?.role === "assistant" && !messages[start].content.some(block => block.type === "toolCall")) start++;
	return start < messages.length ? start : undefined;
}

const records = new Map();
const recordPath = session => join(agentDir, "claude-code", "rpi-sessions", `${createHash("sha256").update(session).digest("hex")}.json`);

/** { claude, account, head: { n, hash, open?, interrupted? }, checkpoints: [{ n, hash, uuid }] }; never the token. */
function loadRecord(session) {
	if (!records.has(session)) {
		let record;
		try { record = JSON.parse(readFileSync(recordPath(session), "utf8")); } catch { /* none yet */ }
		records.set(session, record);
	}
	return records.get(session);
}

// shortcut: the record keeps every checkpoint and is rewritten each turn, trim old checkpoints if long sessions make it slow.
function saveRecord(session, record) {
	records.set(session, record);
	const path = recordPath(session);
	try {
		mkdirSync(dirname(path), { recursive: true });
		writeFileSync(`${path}.tmp`, JSON.stringify(record));
		renameSync(`${path}.tmp`, path);
	} catch { /* a lost record only costs a fresh start */ }
}

/** Copy a Claude session up to a message; the copy gets fresh uuids, so map them and check the copy. */
async function fork(claude, uuid) {
	const source = await sdk.getSessionMessages(claude);
	const end = source.findIndex(entry => entry.uuid === uuid);
	if (end < 0) return;
	const { sessionId } = await sdk.forkSession(claude, { upToMessageId: uuid });
	try {
		const copied = await sdk.getSessionMessages(sessionId);
		if (copied.length !== end + 1 || copied.some((entry, i) => entry.type !== source[i].type || JSON.stringify(entry.message) !== JSON.stringify(source[i].message))) throw new Error("Claude fork changed the copied conversation");
		return { claude: sessionId, uuids: new Map(copied.map((entry, i) => [source[i].uuid, entry.uuid])) };
	} catch (error) {
		await sdk.deleteSession(sessionId).catch(() => {});
		throw error;
	}
}

/**
 * Which Claude session a new query continues, and from which rpi message on:
 * resume when the recorded history is a prefix, fork at the last shared
 * checkpoint after an edit, undo or compaction, otherwise start fresh. A model,
 * prompt, tool or effort change resumes too; a different token starts fresh.
 */
async function plan(session, messages, hashes, apiKey) {
	const fresh = { claude: randomUUID(), start: 0, checkpoints: [] };
	const record = loadRecord(session);
	if (!record || record.account !== hash([record.claude, apiKey])) return fresh;
	// A head with an open tool call cannot resume: the call's result would arrive as text.
	const start = record.head?.open ? undefined : after(record.head, messages, hashes);
	if (start !== undefined && await sdk.getSessionInfo(record.claude).catch(() => undefined)) return { claude: record.claude, resume: true, start, checkpoints: record.checkpoints };
	// Checkpoints follow complete replies only, so a fork never cuts between a tool call and its result.
	const point = record.checkpoints.findLast(checkpoint => after(checkpoint, messages, hashes) !== undefined);
	const branch = point && await fork(record.claude, point.uuid).catch(() => undefined);
	if (!branch) return fresh;
	const checkpoints = record.checkpoints.filter(checkpoint => checkpoint.n <= point.n && branch.uuids.has(checkpoint.uuid)).map(checkpoint => ({ ...checkpoint, uuid: branch.uuids.get(checkpoint.uuid) }));
	return { claude: branch.claude, resume: true, start: point.n, checkpoints };
}

function usage() {
	return { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } };
}

/**
 * Claude Code reports one API response's usage several times while it streams.
 * Keep each response's highest counts; output sums the responses, and input and
 * cache are the last response's (the context it occupies), as OpenCode does.
 */
function usageCounter() {
	const responses = new Map();
	let last;
	return {
		update(id, u) {
			if (!u) return;
			let counts = responses.get(id);
			if (!counts) {
				counts = { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 };
				responses.set(id, counts);
				last = id;
			}
			counts.input = Math.max(counts.input, u.input_tokens ?? 0);
			counts.output = Math.max(counts.output, u.output_tokens ?? 0);
			counts.cacheRead = Math.max(counts.cacheRead, u.cache_read_input_tokens ?? 0);
			counts.cacheWrite = Math.max(counts.cacheWrite, u.cache_creation_input_tokens ?? 0);
		},
		write(target) {
			const lastCall = responses.get(last) ?? { input: 0, cacheRead: 0, cacheWrite: 0 };
			target.input = lastCall.input;
			target.cacheRead = lastCall.cacheRead;
			target.cacheWrite = lastCall.cacheWrite;
			target.output = [...responses.values()].reduce((sum, counts) => sum + counts.output, 0);
			target.totalTokens = target.input + target.output + target.cacheRead + target.cacheWrite;
		},
	};
}

// rpi's built-in prompt scaffold, which Claude Code's own prompt replaces.
const SCAFFOLD = "You are an expert coding assistant operating inside rpi, a coding agent harness.";
const RUNTIME_NOTE = "<runtime_info>You are running in rpi through the Claude Code harness. Mention this only when asked.</runtime_info>";

/**
 * Split rpi's system prompt into what Claude Code should get on top of its own
 * prompt and the session's working directory. rpi composes base, appended
 * text, <project_context>, the skills listing and the cwd. The default
 * scaffold (base) and the cwd go: Claude Code writes its own of both. A custom
 * base stays. If the scaffold's end cannot be found, nothing is dropped.
 */
export function splitSystemPrompt(prompt = "") {
	let rest = prompt, cwd;
	// Current rpi: a final <cwd> section.
	const tail = /(?:^|\n\n)<cwd>\n([^\n]*)\n<\/cwd>\s*$/.exec(rest);
	if (tail) { cwd = tail[1]; rest = rest.slice(0, tail.index); }
	const guidelines = rest.startsWith(SCAFFOLD) ? rest.indexOf("\nGuidelines:\n") : -1;
	if (guidelines >= 0) {
		let end = rest.indexOf("\n\n", guidelines + 1);
		if (end < 0) end = rest.length;
		// Released rpi (0.12 to 0.15) ends the scaffold with its cwd line instead.
		const line = /^\n\nCurrent working directory: ([^\n]*)/.exec(rest.slice(end));
		if (line) { cwd ??= line[1]; end += line[0].length; }
		rest = rest.slice(end);
	}
	return { append: [RUNTIME_NOTE, rest.trim()].filter(Boolean).join("\n\n"), cwd };
}

// Claude Code features that duplicate rpi's (OpenCode turns off the same ones).
const ISOLATION_ENV = {
	CLAUDE_CODE_DISABLE_CLAUDE_MDS: "1",
	CLAUDE_CODE_DISABLE_AUTO_MEMORY: "1",
	DISABLE_AUTO_COMPACT: "1",
	CLAUDE_CODE_ENABLE_AWAY_SUMMARY: "0",
	// Every rpi tool is an MCP tool; tool search would hide them all.
	ENABLE_TOOL_SEARCH: "false",
	CLAUDE_CODE_DISABLE_ADVISOR_TOOL: "1",
	// rpi truncates tool output itself.
	MAX_MCP_OUTPUT_TOKENS: "200000",
};
// Shell variables that would skip the subscription login, override rpi's effort, or time out long rpi tools.
const CLEARED_ENV = ["CLAUDE_CODE_SIMPLE", "CLAUDE_CODE_EFFORT_LEVEL", "MCP_TOOL_TIMEOUT", "ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN"];

// ---- live queries ----------------------------------------------------------

const sessions = new Map();
const exits = new Map();
let agentDir, workDir;

class Live {
	constructor(key, model, context, apiKey, effort, target) {
		this.key = key;
		this.target = target;
		this.done = deferred();
		this.inbox = channel();
		this.input = channel();
		this.calls = new Map();
		this.waiting = [];
		this.toolIds = [];
		const env = {
			...process.env,
			...ISOLATION_ENV,
			CLAUDE_CODE_OAUTH_TOKEN: apiKey,
			// Each rpi agent dir (one per account) keeps its own Claude Code state.
			CLAUDE_CONFIG_DIR: process.env.CLAUDE_CONFIG_DIR ?? join(agentDir, "claude-code"),
			CLAUDE_AGENT_SDK_CLIENT_APP: "rpi-claude-agent",
		};
		// The subscription token is the only credential; an API key in the environment would win over it.
		for (const name of CLEARED_ENV) delete env[name];
		const { append, cwd } = splitSystemPrompt(context.systemPrompt);
		const tools = (context.tools ?? []).map(tool => sdk.tool(tool.name, tool.description, schema(tool.parameters), (_args, extra) => this.toolCall(tool.name, extra)));
		const input = this.input;
		this.query = sdk.query({
			prompt: (async function* () { for (;;) { const message = await input.next(); if (!message) return; yield message; } })(),
			options: {
				model: model.id,
				// snapshot: false renders the prompt on every request, so a changed append applies on resume.
				systemPrompt: { type: "preset", preset: "claude_code", append, snapshot: false },
				cwd: cwd ?? workDir,
				tools: [],
				settingSources: [],
				settings: { disableAllHooks: true },
				skills: [],
				strictMcpConfig: true,
				// Chats keep a Claude session to resume; requests without an rpi session id do not.
				persistSession: !!target,
				...(target && (target.resume ? { resume: target.claude } : { sessionId: target.claude })),
				mcpServers: { [SERVER]: sdk.createSdkMcpServer({ name: SERVER, tools }) },
				// Only rpi's tools run; dontAsk denies anything else without asking.
				permissionMode: "dontAsk",
				allowedTools: [`${PREFIX}*`],
				// Summaries give rpi thinking text to show.
				thinking: { type: "adaptive", display: "summarized" },
				// rpi already expanded its prompts; Claude Code adds nothing to them.
				verbatimPrompts: true,
				includePartialMessages: true,
				effort,
				env,
			},
		});
		(async () => {
			try { for await (const message of this.query) this.inbox.push(message); }
			catch (error) { this.inbox.push({ type: "worker_error", error }); }
			this.inbox.push({ type: "worker_end" });
			this.done.resolve();
		})();
	}
	/** The MCP handler: pending until rpi returns this call's toolResult. */
	toolCall(name, extra) {
		let id = extra?._meta?.["claudecode/toolUseId"];
		const index = this.waiting.findIndex(call => (id ? call.id === id : call.name === name));
		if (index >= 0) id = this.waiting.splice(index, 1)[0].id;
		if (!id) return Promise.resolve({ content: [{ type: "text", text: `No rpi tool call is pending for ${name}.` }], isError: true });
		return this.result(id).promise;
	}
	result(id) {
		if (!this.calls.has(id)) this.calls.set(id, deferred());
		return this.calls.get(id);
	}
	send(content) {
		this.input.push({ type: "user", message: { role: "user", content }, parent_tool_use_id: null });
	}
	/** Let Claude Code finish writing its session and exit, then make sure it has. */
	stop() {
		if (sessions.get(this.session) === this) sessions.delete(this.session);
		clearTimeout(this.idle);
		this.stopping ??= (async () => {
			if (this.busy) await Promise.race([this.query.interrupt().catch(() => {}), sleep(STOP_MS)]);
			this.input.push(null);
			await Promise.race([this.done.promise, sleep(STOP_MS)]);
			try { this.query.close(); } catch { /* already closed */ }
		})();
		// The next query for this session waits, so two processes never write one Claude session.
		exits.set(this.session, this.stopping);
		return this.stopping;
	}
}

function schema(parameters) {
	try { return z.fromJSONSchema(parameters ?? { type: "object" }); }
	catch { return z.looseObject({}); }
}

/** Does this context extend the live query by exactly what it is waiting for? */
function continuation(live, key, messages, hashes) {
	if (!live || live.key !== key || messages.length <= live.count) return;
	if (hashes[live.count - 1] !== live.prefix || messages[live.count - 1].role !== "assistant") return;
	const rest = messages.slice(live.count);
	if (live.toolIds.length) {
		const ids = rest.map(message => message.role === "toolResult" && message.toolCallId);
		if (ids.length === live.toolIds.length && live.toolIds.every(id => ids.includes(id))) return { results: rest };
		return;
	}
	if (rest.length === 1 && rest[0].role === "user") return { user: rest[0] };
}

async function stream({ model, context, options }, { emit, signal }) {
	const apiKey = options?.apiKey;
	const message = { role: "assistant", content: [], api: API, provider: PROVIDER, model: model.id, usage: usage(), stopReason: "stop", timestamp: Date.now() };
	const scrub = value => (apiKey ? String(value).split(apiKey).join("[redacted]") : String(value));
	const fail = (reason, error) => ({ ...message, stopReason: reason, errorMessage: scrub(error) });
	if (!apiKey) return fail("error", "claude-code needs a token: run `claude setup-token`, then `rpi auth login --provider claude-code` (or set CLAUDE_CODE_OAUTH_TOKEN).");
	const session = options.sessionId ?? "default";
	const messages = context.messages ?? [];
	// rpi's thinking level; "off" and "minimal" keep the model's lowest effort.
	const effort = { off: "low", minimal: "low", low: "low", medium: "medium", high: "high", xhigh: "xhigh", max: "max" }[String(options.reasoning).toLowerCase()];
	const key = hash([model.id, context.systemPrompt ?? "", context.tools ?? [], apiKey, effort]);
	const hashes = chain(messages);
	let live = sessions.get(session);
	clearTimeout(live?.idle);
	const next = continuation(live, key, messages, hashes);
	if (!next) {
		await live?.stop();
		await exits.get(session);
		const target = options.sessionId ? await plan(session, messages, hashes, apiKey) : { start: 0, checkpoints: [] };
		live = new Live(key, model, context, apiKey, effort, options.sessionId && target);
		live.session = session;
		live.checkpoints = target.checkpoints;
		sessions.set(session, live);
		live.send(catchUp(messages.slice(target.start)));
	} else if (next.user) live.send(userContent(next.user.content));
	else for (const result of next.results) {
		live.result(result.toolCallId).resolve({
			content: result.content.map(block => block.type === "image" ? { type: "image", data: block.data, mimeType: block.mimeType } : { type: "text", text: block.text ?? "" }),
			isError: !!result.isError,
		});
	}
	live.toolIds = [];
	live.prefix = hashes.at(-1);
	live.count = messages.length + 1;
	live.busy = true;
	live.uuid = undefined;

	/** Record where Claude's session now stands, then stop the query or let it idle. */
	const settle = async (state, result) => {
		const reply = { n: messages.length + 1, hash: link(hashes.at(-1), result) };
		const head = state === "stop" ? reply
			: state === "tooluse" ? { ...reply, open: true }
				: { n: messages.length, hash: hashes.at(-1), interrupted: true };
		if (state === "stop" && live.uuid) live.checkpoints.push({ ...reply, uuid: live.uuid });
		if (live.target) saveRecord(session, { claude: live.target.claude, account: hash([live.target.claude, apiKey]), head, checkpoints: live.checkpoints });
		if (state === "tooluse") return result;
		// Only an aborted turn is still running; stop() interrupts it.
		live.busy = state === "aborted";
		// A pending rpi tool call keeps the query alive however long the tool runs.
		// shortcut: a tool call rpi never answers keeps its query until the worker exits, arm the idle stop when the host reports the session idle.
		if (state === "stop") live.idle = setTimeout(() => live.stop(), IDLE_MS);
		else await live.stop();
		return result;
	};

	const aborted = new Promise(resolve => signal.addEventListener("abort", resolve, { once: true }));
	const blocks = new Map();
	const counter = usageCounter();
	let stopReason, apiError, rateLimited = false, resetsAt, response;
	await emit({ type: "start" });
	try {
		for (;;) {
			const sdkMessage = await Promise.race([live.inbox.next(), aborted.then(() => undefined)]);
			// Claude keeps its partial reply in its session; the next request resumes it.
			if (signal.aborted) return await settle("aborted", { ...message, stopReason: "aborted", errorMessage: "Request was aborted" });
			if (sdkMessage.type === "worker_error" || sdkMessage.type === "worker_end") {
				return await settle("error", fail("error", `Claude Code stopped: ${sdkMessage.error?.message ?? "the query ended"}`));
			}
			if (sdkMessage.type === "rate_limit_event" && sdkMessage.rate_limit_info?.status === "rejected") {
				rateLimited = true;
				resetsAt = sdkMessage.rate_limit_info.resetsAt;
			}
			if (sdkMessage.type === "assistant" && !sdkMessage.parent_tool_use_id && !sdkMessage.error) {
				counter.update(sdkMessage.message?.id, sdkMessage.message?.usage);
				live.uuid = sdkMessage.uuid;
			}
			if (sdkMessage.type === "assistant" && sdkMessage.error && !sdkMessage.parent_tool_use_id) {
				apiError = text(sdkMessage.message?.content) || sdkMessage.error;
				rateLimited ||= sdkMessage.error === "rate_limit";
			}
			if (sdkMessage.type === "result") {
				if (!sdkMessage.is_error && sdkMessage.subtype === "success" && !apiError) return await settle("stop", { ...message, stopReason: "stop" });
				const error = apiError ?? (sdkMessage.result || sdkMessage.errors?.join("; ") || sdkMessage.subtype);
				if (!rateLimited) return await settle("error", fail("error", error));
				// Shared contract with Zephyr's account pool.
				const seconds = resetsAt > 1e12 ? Math.floor(resetsAt / 1000) : resetsAt;
				return await settle("error", fail("error", `rate_limited; ${seconds ? `resets_at=${seconds} ` : ""}${error}`));
			}
			if (sdkMessage.type !== "stream_event" || sdkMessage.parent_tool_use_id) continue;
			const event = sdkMessage.event;
			switch (event.type) {
				case "message_start":
					blocks.clear();
					response = event.message?.id;
					counter.update(response, event.message?.usage);
					break;
				case "content_block_start": {
					const block = event.content_block;
					const contentIndex = message.content.length;
					if (block.type === "text") {
						message.content.push({ type: "text", text: "" });
						await emit({ type: "text_start", contentIndex });
					} else if (block.type === "thinking") {
						message.content.push({ type: "thinking", thinking: "" });
						await emit({ type: "thinking_start", contentIndex });
					} else if (block.type === "tool_use" && block.name.startsWith(PREFIX)) {
						const name = block.name.slice(PREFIX.length);
						message.content.push({ type: "toolCall", id: block.id, name, arguments: {} });
						await emit({ type: "toolcall_start", contentIndex, id: block.id, name });
					} else break;
					blocks.set(event.index, { contentIndex, json: "" });
					break;
				}
				case "content_block_delta": {
					const block = blocks.get(event.index);
					if (!block) break;
					const content = message.content[block.contentIndex];
					const delta = event.delta;
					if (delta.type === "text_delta") { content.text += delta.text; await emit({ type: "text_delta", contentIndex: block.contentIndex, delta: delta.text }); }
					else if (delta.type === "thinking_delta") { content.thinking += delta.thinking; await emit({ type: "thinking_delta", contentIndex: block.contentIndex, delta: delta.thinking }); }
					else if (delta.type === "input_json_delta") { block.json += delta.partial_json; await emit({ type: "toolcall_delta", contentIndex: block.contentIndex, delta: delta.partial_json }); }
					break;
				}
				case "content_block_stop": {
					const block = blocks.get(event.index);
					if (!block) break;
					const content = message.content[block.contentIndex];
					if (content.type === "text") await emit({ type: "text_end", contentIndex: block.contentIndex, content: content.text });
					else if (content.type === "thinking") await emit({ type: "thinking_end", contentIndex: block.contentIndex, content: content.thinking });
					else {
						try { content.arguments = JSON.parse(block.json || "{}"); } catch { content.arguments = {}; }
						live.waiting.push({ id: content.id, name: content.name });
						live.toolIds.push(content.id);
						await emit({ type: "toolcall_end", contentIndex: block.contentIndex, id: content.id, name: content.name, arguments: content.arguments });
					}
					break;
				}
				case "message_delta":
					stopReason = event.delta?.stop_reason ?? stopReason;
					counter.update(response, event.usage);
					break;
				case "message_stop":
					if (stopReason === "tool_use" && live.toolIds.length) return await settle("tooluse", { ...message, stopReason: "tooluse" });
					break;
			}
		}
	} finally {
		counter.write(message.usage);
	}
}

// ---- entry -----------------------------------------------------------------

function closeAll() {
	return Promise.all([...sessions.values()].map(live => live.stop()));
}

// Tests import this module for its pure helpers; only `node main.mjs` serves.
if (import.meta.main !== false) {
	const config = await readBootstrap();
	const closed = serve(config, async (method, input, call) => {
		switch (method) {
			case "runtime.open":
				agentDir = input.agentDir;
				workDir = input.cwd;
				// The SDK's session functions (fork, info) read the same Claude Code state as the queries.
				process.env.CLAUDE_CONFIG_DIR ??= join(agentDir, "claude-code");
				await call.emit({ type: "ready", manifest: {} });
				await new Promise(resolve => call.signal.addEventListener("abort", resolve, { once: true }));
				await closeAll();
				return null;
			case "runtime.configure": return {};
			case "provider.stream": return await stream(input, call);
			default: throw new Error(`unknown worker method ${method}`);
		}
	});
	await closed;
	await closeAll();
	process.exit(0);
}
