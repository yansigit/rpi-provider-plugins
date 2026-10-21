import { AsyncLocalStorage } from "node:async_hooks";
import { createEventBus, type ExtensionCommandContext, type ExtensionContext, type ExtensionEvent, type ExtensionToolContext } from "@earendil-works/pi-coding-agent";
import type { Api, Context, Model } from "@earendil-works/pi-ai";
import { NativeCursorExtension } from "./extension.js";
import { cursorNativeAuth, nativeRequestAuth } from "./auth.js";
import { NativeCursorCatalog } from "./catalog.js";
import { contextSnapshot, nativeContext, NativeEffects, type HostRequest, type NativeTools } from "./context.js";
import { NativeSessionManager, NativeSessionOrigin } from "./session.js";
import { NativeSessionWork } from "./session-work.js";
import type { WorkerCall } from "./peer.js";
import { NativeToolSurface, toolMetadata, type ToolSnapshot } from "./tool-surface.js";
import { isNativeCursorToolName } from "../cursor-native-tool-names.js";
import { scrubSensitiveText } from "../cursor-sensitive-text.js";
import { nativeStreamOptions } from "./options.js";

type Frame = ToolSnapshot & { context: ExtensionContext; effects: NativeEffects; request: HostRequest; origin: NativeSessionOrigin };
function object(value: unknown): Record<string, unknown> {
	if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("Invalid native Cursor request");
	return value as Record<string, unknown>;
}
function text(value: unknown): string { if (typeof value !== "string") throw new Error("Invalid native Cursor request text"); return value; }
function requestModel(value: unknown): Model<Api> {
	const model = object(value);
	for (const key of ["id", "name", "api", "provider", "baseUrl"]) text(model[key]);
	if (model.provider !== "cursor" || model.api !== "cursor-sdk" || typeof model.reasoning !== "boolean" ||
		!Array.isArray(model.input) || model.input.some(input => input !== "text" && input !== "image")) throw new Error("Invalid native Cursor request model");
	for (const key of ["contextWindow", "maxTokens"]) if (!Number.isSafeInteger(model[key]) || Number(model[key]) < 0) throw new Error("Invalid native model limits");
	const cost = object(model.cost);
	for (const key of ["input", "output", "cacheRead", "cacheWrite"]) if (typeof cost[key] !== "number" || !Number.isFinite(cost[key])) throw new Error("Invalid native model cost");
	return value as Model<Api>;
}
function requestContext(value: unknown): Context {
	const context = object(value);
	if (!Array.isArray(context.messages) || (context.systemPrompt !== undefined && typeof context.systemPrompt !== "string") ||
		(context.tools !== undefined && !Array.isArray(context.tools))) throw new Error("Invalid native Cursor conversation");
	for (const message of context.messages) {
		const item = object(message);
		if (!["user", "assistant", "toolResult"].includes(String(item.role)) || typeof item.timestamp !== "number" ||
			(typeof item.content !== "string" && !Array.isArray(item.content))) throw new Error("Invalid native Cursor message");
	}
	return value as Context;
}
/** One retained adapter and SDK owner per native session runtime. */
export class NativeCursorRuntime {
	readonly extension: NativeCursorExtension;
	private catalog?: NativeCursorCatalog;
	private manager = new NativeSessionManager();
	private frames = new AsyncLocalStorage<Frame>();
	private selected?: Frame;
	private flags = new Map<string, string | boolean>();
	private toolSurface = new NativeToolSurface();
	private background = new Set<Promise<void>>();
	private failures: unknown[] = [];
	private operations = new Map<string, AbortController>();
	private started = false;
	private stopping = false;
	private shutDown = false;
	private closing?: Promise<void>;
	constructor(private standing: WorkerCall, private locations: { cwd: string; sessionDir: string }) {
		const frame = () => this.current();
		this.extension = new NativeCursorExtension({
			context: () => frame().context, flags: this.flags, events: createEventBus(),
			appendEntry: async (kind, data) => { await this.manager.origin().append(kind, data); },
			getActiveTools: () => [...frame().active],
			setActiveTools: names => {
				const current = frame(); const change = this.toolSurface.activate(current, names);
				current.effects.add(async () => this.toolSurface.acknowledge(current, change,
					await current.request("native", { port: "tools", action: "setActive", tools: change.tools, state: change.state })));
			},
			getAllTools: () => this.toolSurface.all(frame()),
			toolsChanged: tools => { this.toolSurface.registered = tools; },
			providerRegistered: provider => { this.catalog = new NativeCursorCatalog(provider); },
			error: error => {
				const current = frame();
				current.effects.add(() => current.request("native", { port: "ui", action: "notify", kind: "error", text: scrubSensitiveText(error.error) }));
			},
		}, import.meta.url);
	}
	private current(): Frame {
		const frame = this.frames.getStore() ?? this.selected;
		if (!frame) throw new Error("Native Cursor session context is unavailable");
		return frame;
	}
	async load(): Promise<unknown> {
		await this.extension.load();
		const registration = this.extension.registration;
		return { provider: { id: "cursor", name: "Cursor", baseUrl: "https://cursor.com", models: this.extension.provider().getModels() },
			commands: [...registration.commands.values()].map(({ name, description }) => ({ name, description })),
			flags: [...registration.flags.values()], events: [...registration.handlers.keys()],
			tools: this.toolSurface.registered.filter(tool => !isNativeCursorToolName(tool.definition.name)).map(toolMetadata) };
	}
	async configure(call: WorkerCall): Promise<void> {
		const values = object(await call.host("native", { port: "context", action: "flags", names: [...this.extension.registration.flags.keys()] }));
		for (const [name, value] of Object.entries(values)) {
			if (!this.extension.registration.flags.has(name) || (typeof value !== "string" && typeof value !== "boolean")) throw new Error("Invalid native Cursor flag value");
			this.flags.set(name, value);
		}
	}
	private track(task: Promise<void>): void {
		this.background.add(task);
		void task.then(() => this.background.delete(task), error => { this.background.delete(task); this.failures.push(error); });
	}
	private async withContext<T>(call: WorkerCall, body: (frame: Frame) => Promise<T>): Promise<T> {
		await this.catalog?.settled();
		const snapshot = contextSnapshot(await call.host("native", { port: "context", action: "snapshot" }));
		const tools = await call.host("native", { port: "tools", action: "inspect" });
		if (!Array.isArray(tools)) throw new Error("Invalid native tool inventory");
		const origin = await NativeSessionOrigin.capture(async input => {
			if (typeof input.origin !== "string") return call.host("native", { port: "sessions", ...input });
			return this.standing.host("scoped", { scope: input.origin, method: "native", input: { port: "sessions", ...input } }, null);
		}, this.locations.cwd, this.locations.sessionDir,
			input => this.standing.host("scoped", { scope: input.origin, method: "journals", input }, null));
		let abort: (() => void) | undefined;
		try {
			if (origin.view().id !== snapshot.sessionId) throw new Error("Native context and session capture disagree");
			let controller: AbortController | undefined;
			if (snapshot.identity && ["prompt", "compaction", "tree"].includes(snapshot.identity.purpose)) {
				controller = this.operations.get(snapshot.identity.operationId);
				if (!controller) {
					if (this.operations.size >= 64) throw new Error("Native Cursor operation capacity exhausted");
					controller = new AbortController(); this.operations.set(snapshot.identity.operationId, controller);
				}
			}
			abort = () => controller?.abort(call.signal.reason);
			call.signal.addEventListener("abort", abort, { once: true });
			if (call.signal.aborted) abort();
			const request: HostRequest = (method, input) => this.standing.host("scoped", { scope: origin.origin, method, input }, call.signal);
			const effects = new NativeEffects();
			const frame: Frame = { origin, request, effects, ...this.toolSurface.capture(tools as NativeTools, snapshot.tools, snapshot.toolSelection, snapshot.toolRegistration),
				context: nativeContext(snapshot, this.manager, controller?.signal, request, effects, () => this.extension.provider().getModels()) };
			const previous = this.selected;
			this.manager.select(origin); this.selected = frame;
			if (previous) await previous.origin.release();
			const work = new NativeSessionWork(origin); this.track(work.settled);
			return await work.run(() => this.frames.run(frame, () => this.manager.run(origin, async () => {
				try { return await body(frame); }
				finally { await effects.settled(); }
			})));
		} catch (error) {
			if (this.selected?.origin !== origin) await origin.release();
			throw error;
		} finally { if (abort) call.signal.removeEventListener("abort", abort); }
	}
	async dispatch(method: string, raw: unknown, call: WorkerCall): Promise<unknown> {
		if (this.stopping) throw new Error("Native Cursor runtime is shutting down");
		const input = object(raw);
		switch (method) {
			case "runtime.configure": await this.configure(call); return null;
			case "provider.auth": return cursorNativeAuth(this.extension.provider(), text(input.action), input.input, call.signal);
			case "provider.catalog": {
				const auth = nativeRequestAuth(await call.host("native", { port: "auth", action: "resolve", providerId: "cursor" }));
				return this.catalog!.refresh(input as Parameters<NativeCursorCatalog["refresh"]>[0], request => call.host("native", request), call.signal, auth);
			}
			case "provider.stream": return this.withContext(call, async frame => {
				const entry = input.entry;
				if (entry !== "stream" && entry !== "streamSimple") throw new Error("Invalid Cursor stream entry");
				if (input.authMode !== "ambient" && input.authMode !== "replace") throw new Error("Invalid Cursor authentication mode");
				const options = nativeStreamOptions(object(input.options), frame.context.signal ?? call.signal);
				const stream = await this.extension.stream(entry, requestModel(input.model), this.toolSurface.context(frame, requestContext(input.context)),
					options, input.authMode, call.emit);
				return stream.finished;
			});
			case "extension.command": return this.withContext(call, async frame => {
				await this.extension.command(text(input.name), text(input.args), frame.context as ExtensionCommandContext); return null;
			});
			case "extension.tool": return this.withContext(call, async frame => {
				const tool = this.extension.tool(text(input.name));
				// Retained skill/question tools use the ordinary context; nested execution stays host-owned.
				const context = { ...frame.context, tools: [], executeTool: async () => {
					throw new Error("Cursor skill/question tools must not request nested execution");
				} } satisfies ExtensionToolContext;
				return tool.execute(text(input.id), input.args, call.signal, update => call.emit(update), context);
			});
			case "extension.event": return this.withContext(call, async frame => {
				const raw = object(input.event);
				if (typeof raw.type !== "string" || !this.extension.registration.handlers.has(raw.type)) throw new Error("Unregistered native Cursor event");
				const event: unknown = { ...raw };
				const fields = object(event);
				if (fields.type === "before_provider_headers") throw new Error("Cursor header receipts are private to each stream");
				if (fields.type === "session_before_compact" || fields.type === "session_before_tree") fields.signal = frame.context.signal ?? call.signal;
				if (fields.type === "session_start") this.started = true;
				const result = await this.extension.event(event as ExtensionEvent);
				if (fields.type === "session_shutdown") this.shutDown = true;
				return result ?? null;
			});
			case "operation.end": {
				const id = text(input.operationId); this.operations.delete(id); return null;
			}
			default: throw new Error("Unsupported native Cursor worker method");
		}
	}
	close(): Promise<void> { return this.closing ??= this.finish(); }
	private async finish(): Promise<void> {
		this.stopping = true;
		for (const controller of this.operations.values()) controller.abort();
		try {
			if (this.started && !this.shutDown && this.selected) {
				const frame = this.selected;
				try {
					await this.frames.run(frame, () => this.manager.run(frame.origin, () => this.extension.event({ type: "session_shutdown", reason: "quit" })));
					await frame.effects.settled();
				} catch (error) { this.failures.push(error); }
			}
			while (this.background.size) await Promise.allSettled([...this.background]);
			if (this.failures.length) throw new AggregateError(this.failures, "Native Cursor background work failed");
		} finally {
			this.extension.invalidate();
			this.operations.clear();
			await this.selected?.origin.release(); this.selected = undefined;
		}
	}
}
