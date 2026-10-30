// rpi-claude-agent worker: one Claude Agent SDK query per rpi session, driven over
// the rpi-cursor-sdk worker wire (length-prefixed JSON frames on a private socket).
//
// Claude Code's own tools are off. rpi's tools are an in-process MCP server: when
// Claude calls one, the handler stays pending, the rpi stream ends with a toolCall,
// and the next stream call's toolResult resolves it so the same query continues.
import { createHash } from "node:crypto";
import { connect } from "node:net";
import { join } from "node:path";
import { z } from "zod";

// Tests inject a fake SDK by path; production loads the pinned package.
const sdk = await import(process.env.RPI_CLAUDE_AGENT_SDK ?? "@anthropic-ai/claude-agent-sdk");

const API = "claude-agent-sdk";
const PROVIDER = "claude-code";
const SERVER = "rpi";
const PREFIX = `mcp__${SERVER}__`;
const CAPACITY = 64;
const PAYLOAD_BYTES = 4 * 1024 * 1024;

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

/** A fresh query cannot replay rpi's history natively, so it reads it as text. */
function transcript(messages) {
	if (messages.length === 1 && messages[0].role === "user") return userContent(messages[0].content);
	const lines = messages.map(message => {
		if (message.role === "user") return `[user]\n${text(message.content)}`;
		if (message.role === "toolResult") return `[tool result ${message.toolName} ${message.toolCallId}${message.isError ? " error" : ""}]\n${text(message.content)}`;
		return `[assistant]\n${message.content.map(block => block.type === "toolCall"
			? `[tool call ${block.name} ${block.id}] ${JSON.stringify(block.arguments)}`
			: block.type === "text" ? block.text : "").filter(Boolean).join("\n")}`;
	});
	const last = messages.at(-1)?.role === "user" ? "Reply to the last user message." : "Continue from the last tool result.";
	return `This conversation is being resumed. The transcript so far:\n\n${lines.join("\n\n")}\n\n${last}`;
}

function usage() {
	return { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } };
}

// ---- live queries ----------------------------------------------------------

const sessions = new Map();
let agentDir;

class Live {
	constructor(key, model, context, apiKey, effort) {
		this.key = key;
		this.inbox = channel();
		this.input = channel();
		this.calls = new Map();
		this.waiting = [];
		this.toolIds = [];
		const env = {
			...process.env,
			CLAUDE_CODE_OAUTH_TOKEN: apiKey,
			// Each rpi agent dir (one per account) keeps its own Claude Code state.
			CLAUDE_CONFIG_DIR: process.env.CLAUDE_CONFIG_DIR ?? join(agentDir, "claude-code"),
			CLAUDE_AGENT_SDK_CLIENT_APP: "rpi-claude-agent",
		};
		// The subscription token is the only credential; an API key in the environment would win over it.
		delete env.ANTHROPIC_API_KEY;
		delete env.ANTHROPIC_AUTH_TOKEN;
		const tools = (context.tools ?? []).map(tool => sdk.tool(tool.name, tool.description, schema(tool.parameters), (_args, extra) => this.toolCall(tool.name, extra)));
		const input = this.input;
		this.query = sdk.query({
			prompt: (async function* () { for (;;) { const message = await input.next(); if (!message) return; yield message; } })(),
			options: {
				model: model.id,
				systemPrompt: context.systemPrompt ?? "",
				tools: [],
				settingSources: [],
				strictMcpConfig: true,
				persistSession: false,
				mcpServers: { [SERVER]: sdk.createSdkMcpServer({ name: SERVER, tools }) },
				permissionMode: "default",
				canUseTool: async (name, toolInput) => name.startsWith(PREFIX)
					? { behavior: "allow", updatedInput: toolInput }
					: { behavior: "deny", message: "Only the tools rpi provides are available." },
				includePartialMessages: true,
				effort,
				env,
			},
		});
		(async () => {
			try { for await (const message of this.query) this.inbox.push(message); }
			catch (error) { this.inbox.push({ type: "worker_error", error }); }
			this.inbox.push({ type: "worker_end" });
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
	close() {
		sessions.delete(this.session);
		this.input.push(null);
		try { this.query.close(); } catch { /* already closed */ }
	}
}

function schema(parameters) {
	try { return z.fromJSONSchema(parameters ?? { type: "object" }); }
	catch { return z.looseObject({}); }
}

/** Does this context extend the live query by exactly what it is waiting for? */
function continuation(live, key, messages) {
	if (!live || live.key !== key || messages.length <= live.count) return;
	if (hash(messages.slice(0, live.count - 1)) !== live.prefix || messages[live.count - 1].role !== "assistant") return;
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
	let live = sessions.get(session);
	const next = continuation(live, key, messages);
	if (!next) {
		live?.close();
		live = new Live(key, model, context, apiKey, effort);
		live.session = session;
		sessions.set(session, live);
		live.send(transcript(messages));
	} else if (next.user) live.send(userContent(next.user.content));
	else for (const result of next.results) {
		live.result(result.toolCallId).resolve({
			content: result.content.map(block => block.type === "image" ? { type: "image", data: block.data, mimeType: block.mimeType } : { type: "text", text: block.text ?? "" }),
			isError: !!result.isError,
		});
	}
	live.toolIds = [];
	live.prefix = hash(messages);
	live.count = messages.length + 1;

	const aborted = new Promise(resolve => signal.addEventListener("abort", resolve, { once: true }));
	const blocks = new Map();
	let stopReason, apiError, rateLimited = false, resetsAt;
	await emit({ type: "start" });
	try {
		for (;;) {
			const sdkMessage = await Promise.race([live.inbox.next(), aborted.then(() => undefined)]);
			if (signal.aborted) {
				await Promise.race([live.query.interrupt().catch(() => {}), new Promise(resolve => setTimeout(resolve, 2000))]);
				live.close();
				return { ...message, stopReason: "aborted", errorMessage: "Request was aborted" };
			}
			if (sdkMessage.type === "worker_error" || sdkMessage.type === "worker_end") {
				live.close();
				return fail("error", `Claude Code stopped: ${sdkMessage.error?.message ?? "the query ended"}`);
			}
			if (sdkMessage.type === "rate_limit_event" && sdkMessage.rate_limit_info?.status === "rejected") {
				rateLimited = true;
				resetsAt = sdkMessage.rate_limit_info.resetsAt;
			}
			if (sdkMessage.type === "assistant" && sdkMessage.error && !sdkMessage.parent_tool_use_id) {
				apiError = text(sdkMessage.message?.content) || sdkMessage.error;
				rateLimited ||= sdkMessage.error === "rate_limit";
			}
			if (sdkMessage.type === "result") {
				if (!sdkMessage.is_error && sdkMessage.subtype === "success" && !apiError) return { ...message, stopReason: "stop" };
				const error = apiError ?? (sdkMessage.result || sdkMessage.errors?.join("; ") || sdkMessage.subtype);
				live.close();
				if (!rateLimited) return fail("error", error);
				// Shared contract with Zephyr's account pool.
				const seconds = resetsAt > 1e12 ? Math.floor(resetsAt / 1000) : resetsAt;
				return fail("error", `rate_limited; ${seconds ? `resets_at=${seconds} ` : ""}${error}`);
			}
			if (sdkMessage.type !== "stream_event" || sdkMessage.parent_tool_use_id) continue;
			const event = sdkMessage.event;
			const add = (key, value) => { message.usage[key] += value ?? 0; };
			switch (event.type) {
				case "message_start": {
					blocks.clear();
					const u = event.message?.usage ?? {};
					add("input", u.input_tokens); add("cacheRead", u.cache_read_input_tokens); add("cacheWrite", u.cache_creation_input_tokens); add("output", u.output_tokens);
					break;
				}
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
					add("output", event.usage?.output_tokens);
					break;
				case "message_stop":
					if (stopReason === "tool_use" && live.toolIds.length) return { ...message, stopReason: "tooluse" };
					break;
			}
		}
	} finally {
		message.usage.totalTokens = message.usage.input + message.usage.output + message.usage.cacheRead + message.usage.cacheWrite;
	}
}

// ---- entry -----------------------------------------------------------------

function closeAll() {
	for (const live of [...sessions.values()]) live.close();
}

const config = await readBootstrap();
const closed = serve(config, async (method, input, call) => {
	switch (method) {
		case "runtime.open":
			agentDir = input.agentDir;
			await call.emit({ type: "ready", manifest: {} });
			await new Promise(resolve => call.signal.addEventListener("abort", resolve, { once: true }));
			closeAll();
			return null;
		case "runtime.configure": return {};
		case "provider.stream": return await stream(input, call);
		default: throw new Error(`unknown worker method ${method}`);
	}
});
await closed;
closeAll();
process.exit(0);
