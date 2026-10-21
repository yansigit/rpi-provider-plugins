import { createExtensionRuntime, ExtensionRunner, type Extension, type ExtensionAPI, type ExtensionCommandContext, type ExtensionContext, type ExtensionError, type ExtensionEvent, type RegisteredTool } from "@earendil-works/pi-coding-agent";
import { normalizeContext, type Api, type AssistantMessageEvent, type Context, type Model, type Provider, type ProviderHeaders, type SimpleStreamOptions } from "@earendil-works/pi-ai";
import register from "../index.js";
import { isNativeCursorToolName } from "../cursor-native-tool-names.js";
import { withNativeAuthEnvironment } from "./environment.js";
import { withRecordedNativeStream } from "./replay.js";
import { NativeMessageStream } from "./stream.js";

type CursorApi = Parameters<typeof register>[0];
type Handler = Extension["handlers"] extends Map<string, (infer T)[]> ? T : never;

export interface NativeExtensionPorts {
	context(): ExtensionContext;
	appendEntry(customType: string, data: unknown): Promise<void>;
	getActiveTools: ExtensionAPI["getActiveTools"];
	getAllTools: ExtensionAPI["getAllTools"];
	setActiveTools: ExtensionAPI["setActiveTools"];
	toolsChanged(tools: readonly RegisteredTool[]): void;
	providerRegistered(provider: Provider): void;
	events: ExtensionAPI["events"];
	flags: ReadonlyMap<string, boolean | string>;
	error(error: ExtensionError): void;
}

/** The only factory loaded here is the retained Cursor adapter, with awaited native append. */
export class NativeCursorExtension {
	readonly api: CursorApi;
	readonly registration: Extension;
	readonly hooks: ExtensionRunner;
	private providerValue?: Provider;
	private active = true;
	private loading = false;
	private unsubscribers = new Set<() => void>();
	constructor(ports: NativeExtensionPorts, path: string) {
		const extension: Extension = { path, resolvedPath: path,
			sourceInfo: { path, source: "native", scope: "temporary", origin: "top-level" },
			handlers: new Map(), tools: new Map(), commands: new Map(), flags: new Map(),
			messageRenderers: new Map(), shortcuts: new Map() };
		this.registration = extension;
		// Only the hook reducer is used; the overridden context excludes Pi's session and model stores.
		this.hooks = Reflect.construct(ExtensionRunner, [[extension], createExtensionRuntime(), "", null, null]);
		this.hooks.createContext = () => { this.assertActive(); return ports.context(); };
		this.hooks.onError(ports.error);
		const values = new Map<string, boolean | string>();
		const on = (event: string, handler: Handler) => {
			this.assertActive();
			const wrapped: Handler = (...args) => handler(...args);
			const handlers = extension.handlers.get(event) ?? [];
			handlers.push(wrapped); extension.handlers.set(event, handlers);
			return () => { const index = handlers.indexOf(wrapped); if (index >= 0) handlers.splice(index, 1); };
		};
		this.api = {
			on: on as CursorApi["on"],
			appendEntry: async (customType, data) => { this.assertActive(); await ports.appendEntry(customType, data); },
			getActiveTools: () => { this.assertActive(); return ports.getActiveTools(); },
			getAllTools: () => { this.assertActive(); return ports.getAllTools(); },
			setActiveTools: names => { this.assertActive(); ports.setActiveTools(names); },
			registerTool: definition => {
				this.assertActive();
				if (!definition.parameters || typeof definition.parameters !== "object" || Array.isArray(definition.parameters)) {
					throw new Error(`Tool "${definition.name}" must define an object parameter schema.`);
				}
				extension.tools.set(definition.name, { definition: definition as RegisteredTool["definition"], sourceInfo: extension.sourceInfo });
				ports.toolsChanged([...extension.tools.values()]);
			},
			registerCommand: (name, options) => {
				this.assertActive();
				if (typeof name !== "string" || !name) throw new Error("Cursor command must have a non-empty name");
				extension.commands.set(name, { name, ...options, sourceInfo: extension.sourceInfo });
			},
			registerFlag: (name, options) => {
				this.assertActive(); extension.flags.set(name, { name, ...options, extensionPath: path });
				if (options.default !== undefined && !values.has(name)) values.set(name, options.default);
			},
			getFlag: name => { this.assertActive(); return extension.flags.has(name) ? ports.flags.get(name) ?? values.get(name) : undefined; },
			registerProvider: ((provider: Provider | string) => {
				this.assertActive();
				if (typeof provider === "string" || provider.id !== "cursor") throw new Error("Unexpected retained Cursor provider registration");
				ports.providerRegistered(provider); this.providerValue = provider;
			}) as CursorApi["registerProvider"],
			events: {
				emit: (name, data) => { this.assertActive(); ports.events.emit(name, data); },
				on: (name, listener) => {
					this.assertActive(); const unsubscribe = ports.events.on(name, listener);
					const release = () => { this.unsubscribers.delete(release); unsubscribe(); };
					this.unsubscribers.add(release); return release;
				},
			},
		};
	}
	private assertActive(): void { if (!this.active) throw new Error("Native Cursor extension owner is closed"); }
	async load(): Promise<void> {
		this.assertActive();
		if (this.loading || this.providerValue) throw new Error("Native Cursor extension is already loaded");
		if (this.hooks.emitBeforeAgentStart.length !== 2) throw new Error("Native Cursor hooks require Pi 1.3.0");
		this.loading = true;
		try { await register(this.api); }
		catch (error) { this.invalidate(); throw error; }
		finally { this.loading = false; }
	}
	provider(): Provider { this.assertActive(); if (!this.providerValue) throw new Error("Native Cursor provider is not registered"); return this.providerValue; }
	async stream(entry: "stream" | "streamSimple", model: Model<Api>, context: Context, options: SimpleStreamOptions,
		mode: "ambient" | "replace", emit: (event: AssistantMessageEvent) => void): Promise<NativeMessageStream> {
		this.assertActive();
		if (entry !== "stream" && entry !== "streamSimple") throw new Error("Invalid native Cursor stream entry");
		const selected = { ...options, headers: options.headers ?? {} };
		return withNativeAuthEnvironment(mode, options.env ?? {}, async () => {
			await this.headers(selected.headers);
			const stream = withRecordedNativeStream(this.api, emit, () => this.provider()[entry](model, normalizeContext(context), selected));
			if (!(stream instanceof NativeMessageStream)) throw new Error("Cursor provider did not use the native stream boundary");
			return stream;
		});
	}
	async command(name: string, args: string, context: ExtensionCommandContext): Promise<void> {
		this.assertActive(); const command = this.registration.commands.get(name);
		if (!command) throw new Error(`Unknown Cursor command: ${name}`);
		await command.handler(args, context);
	}
	tool(name: string): RegisteredTool["definition"] {
		this.assertActive();
		if (isNativeCursorToolName(name)) throw new Error("Cursor replay is presentation-only; executable host tools must run through rpi");
		const tool = this.registration.tools.get(name);
		if (!tool) throw new Error(`Unknown Cursor tool: ${name}`);
		return tool.definition;
	}
	async event(event: ExtensionEvent): Promise<unknown> {
		this.assertActive();
		switch (event.type) {
			case "tool_call": return this.hooks.emitToolCall(event);
			case "tool_result": return this.hooks.emitToolResult(event);
			case "message_end": return this.hooks.emitMessageEnd(event);
			case "before_provider_headers": return this.headers(event.headers);
			case "before_agent_start": {
				// Native distribution pins Pi 1.3.0; its trigger replaced the older prompt/images pair.
				if (this.hooks.emitBeforeAgentStart.length !== 2) throw new Error("Native Cursor hooks require Pi 1.3.0");
				return Reflect.apply(this.hooks.emitBeforeAgentStart, this.hooks, [event, event.systemPromptOptions]);
			}
			default: return this.hooks.emit(event as Parameters<ExtensionRunner["emit"]>[0]);
		}
	}
	async headers(headers: ProviderHeaders): Promise<ProviderHeaders> {
		this.assertActive(); return this.hooks.emitBeforeProviderHeaders(headers);
	}
	invalidate(): void {
		if (!this.active) return;
		this.active = false;
		for (const release of this.unsubscribers) release();
		this.hooks.invalidate("Native Cursor extension owner is closed");
	}
}
