import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, test } from "node:test";
import { NativeCursorExtension } from "../runtime/dist/rpi/extension.js";
import { initializeNativePaths } from "../runtime/dist/rpi/paths.js";
import { NativeMessageStream } from "../runtime/dist/rpi/stream.js";
import { withRecordedNativeStream } from "../runtime/dist/rpi/replay.js";
import { createCursorMessageStream } from "../runtime/dist/rpi/stream.js";
import { cursorCredentialEnvironment } from "../runtime/dist/rpi/environment.js";
const { SessionManager, createEventBus } = await import("../runtime/node_modules/@earendil-works/pi-coding-agent/dist/index.js");
const directory = await mkdtemp(join(tmpdir(), "rpi-native-cursor-extension-"));
initializeNativePaths(join(directory, "rpi"));
after(async () => { await rm(directory, { recursive: true, force: true }); });

function fixture() {
	const manager = SessionManager.inMemory(directory);
	const signal = new AbortController().signal, errors = [], notices = [], appends = [];
	const tools = new Map(["read", "bash", "edit", "write"].map(name => [name, { name, description: name,
		parameters: { type: "object", properties: {} }, sourceInfo: { source: "builtin", path: name, scope: "temporary", origin: "top-level" } }]));
	let active = [...tools.keys()], provider, appendGate = Promise.resolve();
	const ctx = { cwd: directory, sessionManager: manager, signal, mode: "rpc", hasUI: true,
		isProjectTrusted: () => true, isIdle: () => true, hasPendingMessages: () => false,
		getContextUsage: () => undefined, getSystemPrompt: () => "native system prompt",
		abort: () => {}, shutdown: () => {}, compact: () => {},
		ui: { notify: (...args) => notices.push(args), setStatus: () => {}, select: async () => undefined,
			input: async () => undefined, confirm: async () => false },
		modelRegistry: {
			getAll: () => provider?.getModels() ?? [],
			getProviderAuth: async () => undefined, getApiKeyForProvider: async () => undefined,
			refresh: async () => {
				await provider.refreshModels({ allowNetwork: false, signal, publish: async ({ update }) => { update?.(); return true; } });
				return { errors: new Map(), aborted: false };
			},
		},
	};
	const extension = new NativeCursorExtension({
		context: () => ctx, flags: new Map([["cursor-no-fast", true]]), events: createEventBus(),
		error: error => errors.push(error), providerRegistered: value => { provider = value; },
		appendEntry: async (customType, data) => { await appendGate; manager.appendCustomEntry(customType, data); appends.push({ customType, data }); },
		getActiveTools: () => [...active], setActiveTools: names => { active = [...names]; },
		getAllTools: () => [...tools.values()], toolsChanged: registered => {
			for (const { definition, sourceInfo } of registered) tools.set(definition.name, { ...definition, sourceInfo });
		},
	}, "rpi-native-cursor-fixture");
	return { extension, ctx, errors, notices, appends, setAppendGate: promise => { appendGate = promise; } };
}

test("loads the complete retained factory and runs native hooks with Pi 1.3.0", async () => {
	const f = fixture();
	try {
		await f.extension.load();
		assert.equal(f.extension.hooks.emitBeforeAgentStart.length, 2);
		assert.deepEqual([...f.extension.registration.commands.keys()].sort(), ["cursor-cloud", "cursor-fast", "cursor-http", "cursor-local-resume-cleanup", "cursor-mode", "cursor-refresh-config", "cursor-refresh-models", "cursor-runtime", "cursor-tools", "cursor-usage"]);
		assert.equal(f.extension.api.getFlag("cursor-no-fast"), true);
		assert.equal(f.extension.api.getFlag("unregistered"), undefined);
		f.ctx.model = f.extension.provider().getModels()[0];
		assert(f.ctx.model && f.ctx.model.provider === "cursor");
		await f.extension.event({ type: "session_start", reason: "startup" });
		assert.deepEqual(f.errors, []);
		assert.throws(() => f.extension.tool("bash"), /presentation-only/);
		assert.throws(() => f.extension.tool("cursor_ask_question"), /Unknown Cursor tool/);
		assert.equal(f.extension.tool("cursor_activate_skill").name, "cursor_activate_skill");
		const started = await f.extension.event({ type: "before_agent_start", trigger: "prompt", prompt: "hello",
			systemPromptOptions: { cwd: directory, forceSystemPrompt: "native system prompt" }, systemPrompt: "native system prompt" });
		assert(started.systemPromptOptions);
		assert.equal(started.systemPrompt, "native system prompt");
		assert.deepEqual(f.errors, []);
	} finally {
		await f.extension.event({ type: "session_shutdown" }); f.extension.invalidate();
	}
	assert.throws(() => f.extension.api.getAllTools(), /owner is closed/);
});

test("native prompt hooks preserve loaded context, skill rules and all ordered custom messages", async () => {
	const f = fixture();
	try {
		await f.extension.load(); f.ctx.model = f.extension.provider().getModels()[0];
		await f.extension.event({ type: "session_start", reason: "startup" });
		const path = join(directory, "project&one", "AGENTS.md");
		const renderedPath = join(directory, "project&amp;one", "AGENTS.md");
		const prompt = `base\n\n<project_context>\n\nProject-specific instructions and guidelines:\n\n<project_instructions path="${renderedPath}">\ncaptured rules\n</project_instructions>\n\n</project_context>\n`;
		for (const name of ["first", "second"]) f.extension.api.on("before_agent_start", () => ({
			message: { customType: name, content: name, display: false },
		}));
		const started = await f.extension.event({ type: "before_agent_start", messageType: "user", prompt: "fixture", systemPrompt: prompt,
			systemPromptOptions: { cwd: directory, forceSystemPrompt: prompt, contextFiles: [{ path, content: "captured rules" }],
				skills: [{ name: "fixture", description: "loaded skill", filePath: join(directory, "skill", "SKILL.md"), disableModelInvocation: false }] } });
		assert(!started.systemPrompt.includes("captured rules"));
		assert(started.systemPrompt.startsWith("base"));
		assert(started.systemPrompt.includes("pi__cursor_activate_skill"));
		assert(started.systemPrompt.includes("loaded skill"));
		assert.deepEqual(started.messages.map(message => message.customType), ["first", "second"]);
		assert.equal(started.systemPromptOptions.contextFiles[0].content, "captured rules");
		assert.deepEqual(f.errors, []);
	} finally {
		await f.extension.event({ type: "session_shutdown", reason: "quit" }); f.extension.invalidate();
	}
});

test("native append is awaited and private header receipts are consumed once by both stream entries", async () => {
	const f = fixture();
	try {
		await f.extension.load(); f.ctx.model = f.extension.provider().getModels()[0];
		await f.extension.event({ type: "session_start", reason: "startup" });
		let release; f.setAppendGate(new Promise(resolve => { release = resolve; }));
		let acknowledged = false;
		const append = f.extension.api.appendEntry("native-fixture", { value: 1 }).then(() => { acknowledged = true; });
		await Promise.resolve(); assert.equal(acknowledged, false);
		release(); await append; assert.equal(f.appends.at(-1).customType, "native-fixture");
		for (const entry of ["stream", "streamSimple"]) {
			const headers = {}, events = [];
			assert.equal(await f.extension.headers(headers), headers);
			const invoke = () => withRecordedNativeStream(f.extension.api, event => events.push(structuredClone(event)), () =>
				f.extension.provider()[entry](f.ctx.model, { messages: [{ role: "user", content: "offline", timestamp: 1 }] }, { headers, signal: f.ctx.signal }));
			const first = invoke(); assert(first instanceof NativeMessageStream);
			const firstResult = await first.finished;
			assert.match(firstResult.errorMessage, /API key/i);
			assert.doesNotMatch(firstResult.errorMessage, /request receipt/);
			const secondResult = await invoke().finished;
			assert.match(secondResult.errorMessage, /request receipt/);
			assert.equal(events.filter(event => event.type === "error").length, 2);
		}
		assert.deepEqual(f.errors, []);
	} finally {
		await f.extension.event({ type: "session_shutdown" }); f.extension.invalidate();
	}
});

test("retained commands wait for native persistence before acknowledging a setting change", async () => {
	const f = fixture();
	try {
		await f.extension.load(); f.ctx.model = f.extension.provider().getModels()[0];
		await f.extension.event({ type: "session_start", reason: "startup" });
		let release; f.setAppendGate(new Promise(resolve => { release = resolve; }));
		const before = f.appends.length;
		let completed = false;
		const pending = f.extension.command("cursor-mode", "plan", f.ctx).then(() => { completed = true; });
		await new Promise(resolve => setImmediate(resolve));
		assert.equal(completed, false); assert.equal(f.appends.length, before);
		release(); await pending;
		assert.equal(completed, true); assert(f.appends.length > before);
		assert.deepEqual(f.errors, []);
	} finally {
		await f.extension.event({ type: "session_shutdown" }); f.extension.invalidate();
	}
});

test("native stream entries preserve request objects and isolate asynchronous credential replacement", async () => {
	const f = fixture(), observed = [], original = process.env.CURSOR_API_KEY;
	process.env.CURSOR_API_KEY = "native-fixture-ambient";
	try {
		await f.extension.load(); f.ctx.model = f.extension.provider().getModels()[0];
		await f.extension.event({ type: "session_start", reason: "startup" });
		for (const entry of ["stream", "streamSimple"]) {
			const events = [], headers = {}, signal = new AbortController().signal;
			const stream = await f.extension.stream(entry, f.ctx.model,
				{ messages: [{ role: "user", content: "offline", timestamp: 1 }] },
				{ apiKey: "$CURSOR_API_KEY", headers, signal, env: {} }, "replace", event => events.push(event));
			assert.match((await stream.finished).errorMessage, /API key/i);
			assert.equal(events.at(-1).type, "error");
		}
		const provider = f.extension.provider();
		const invoke = (model, context, options) => {
			const stream = createCursorMessageStream();
			void Promise.resolve().then(() => {
				observed.push({ options, key: cursorCredentialEnvironment("CURSOR_API_KEY"), context });
				stream.end({ role: "assistant", content: [], api: model.api, provider: model.provider,
					model: model.id, usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0,
						cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } }, stopReason: "stop", timestamp: 1 });
			});
			return stream;
		};
		f.extension.api.registerProvider({ ...provider, stream: invoke, streamSimple: invoke });
		const headers = {}, signal = new AbortController().signal;
		const received = [];
		f.extension.api.on("before_provider_headers", event => { received.push(event.headers); });
		const streams = await Promise.all(["stream", "streamSimple"].map((entry, index) =>
			f.extension.stream(entry, f.ctx.model, { messages: [] },
				{ headers, signal, env: index ? {} : { CURSOR_API_KEY: "native-fixture-selected" },
					temperature: 0.4, maxTokens: 17 }, "replace", () => {})));
		await Promise.all(streams.map(stream => stream.finished));
		assert.deepEqual(observed.map(item => item.key), ["native-fixture-selected", undefined]);
		for (const { options, context } of observed) {
			assert.equal(options.headers, headers); assert.equal(options.signal, signal);
			assert.equal(options.temperature, 0.4); assert.equal(options.maxTokens, 17);
			assert.deepEqual(context.messages, []);
		}
		assert.deepEqual(received, [headers, headers]);
		assert.equal(process.env.CURSOR_API_KEY, "native-fixture-ambient");
	} finally {
		if (original === undefined) delete process.env.CURSOR_API_KEY; else process.env.CURSOR_API_KEY = original;
		await f.extension.event({ type: "session_shutdown" }); f.extension.invalidate();
	}
});

test("native hook dispatch keeps Pi ordering, result chaining and tool-call error propagation", async () => {
	const f = fixture(), order = [];
	try {
		let release; const gate = new Promise(resolve => { release = resolve; });
		f.extension.api.on("tool_result", async () => { order.push(1); await gate; return { content: [{ type: "text", text: "changed" }] }; });
		f.extension.api.on("tool_result", event => { order.push(2); assert.equal(event.structuredContent, undefined); assert.equal(event.content[0].text, "changed"); return { isError: true }; });
		const result = f.extension.event({ type: "tool_result", toolCallId: "call", toolName: "test", input: {},
			content: [{ type: "text", text: "old" }], structuredContent: { old: true }, isError: false });
		assert.deepEqual(order, [1]); release();
		assert.equal((await result).isError, true); assert.deepEqual(order, [1, 2]);
		f.extension.api.on("session_compact_failed", () => { throw new Error("observed failure"); });
		f.extension.api.on("session_compact_failed", () => { order.push(3); });
		await f.extension.event({ type: "session_compact_failed" });
		assert.equal(f.errors.at(-1).error, "observed failure"); assert.equal(order.at(-1), 3);
		f.extension.api.on("tool_call", () => { throw new Error("deny execution"); });
		f.extension.api.on("tool_call", () => { throw new Error("must not run"); });
		await assert.rejects(f.extension.event({ type: "tool_call", toolCallId: "call", toolName: "test", input: {} }), /deny execution/);
	} finally { f.extension.invalidate(); }
});
