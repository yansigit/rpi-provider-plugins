import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";
import type { Model, Api, AuthResult } from "@earendil-works/pi-ai";
import type { NativeSessionManager } from "./session.js";
import { nativeRequestAuth } from "./auth.js";
import { bindNativeSessionCallback, trackNativeSessionWork } from "./session-work.js";

export type HostRequest = (method: string, input: Record<string, unknown>) => Promise<unknown>;
export type NativeToolPolicy = { disabled: boolean; allow: string[] | null; deny: string[]; ceiling?: string[] };
export type NativeToolSelection = { revision: string; state: unknown };
export function toolSelection(value: unknown): NativeToolSelection {
	if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("Invalid native tool selection");
	const selection = value as Partial<NativeToolSelection>;
	if (typeof selection.revision !== "string" || !/^(0|[1-9][0-9]{0,19})$/.test(selection.revision) ||
		BigInt(selection.revision) > 18446744073709551615n || !("state" in selection)) throw new Error("Invalid native tool selection");
	return selection as NativeToolSelection;
}
export type NativeContextSnapshot = {
	cwd: string; mode: ExtensionContext["mode"]; hasUI: boolean; projectTrusted: boolean;
	projectTrustResolved: boolean;
	isIdle: boolean; hasPendingMessages: boolean; systemPrompt: string; sessionId: string;
	model: Omit<Model<Api>, "baseUrl" | "headers">; thinkingLevel?: ExtensionContext["thinkingLevel"];
	tools: string[]; toolRegistration: NativeToolPolicy; toolSelection: NativeToolSelection; identity: { operationId: string; purpose: string } | null;
};
export function nativeModel(value: unknown, models: readonly Model<Api>[]): Model<Api> {
	if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("Invalid native context model");
	const model = value as Record<string, unknown>;
	if (typeof model.id !== "string" || typeof model.provider !== "string" || typeof model.api !== "string" || "headers" in model || "baseUrl" in model) throw new Error("Invalid public native context model");
	return models.find(owned => owned.id === model.id && owned.provider === model.provider)
		?? { ...value as NativeContextSnapshot["model"], baseUrl: "" };
}
export function contextSnapshot(value: unknown): NativeContextSnapshot {
	if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("Invalid native context snapshot");
	const input = value as Record<string, unknown>;
	toolSelection(input.toolSelection);
	for (const key of ["cwd", "systemPrompt", "sessionId"]) if (typeof input[key] !== "string") throw new Error("Invalid native context text");
	for (const key of ["hasUI", "projectTrusted", "projectTrustResolved", "isIdle", "hasPendingMessages"]) if (typeof input[key] !== "boolean") throw new Error("Invalid native context state");
	if (!["tui", "rpc", "json", "print"].includes(String(input.mode)) || !Array.isArray(input.tools) || input.tools.some(name => typeof name !== "string")) {
		throw new Error("Invalid native context mode or tools");
	}
	const policy = input.toolRegistration as Partial<NativeToolPolicy> | undefined;
	const names = (value: unknown): value is string[] => Array.isArray(value) && value.every(name => typeof name === "string");
	if (!policy || typeof policy !== "object" || typeof policy.disabled !== "boolean" ||
		(policy.allow !== null && !names(policy.allow)) || !names(policy.deny) || (policy.ceiling !== undefined && !names(policy.ceiling))) throw new Error("Invalid native tool registration policy");
	nativeModel(input.model, []);
	if (input.identity !== null) {
		if (!input.identity || typeof input.identity !== "object" || !("operationId" in input.identity) ||
			typeof input.identity.operationId !== "string" || !("purpose" in input.identity) || typeof input.identity.purpose !== "string") throw new Error("Invalid native operation identity");
	}
	return input as NativeContextSnapshot;
}

/** Synchronous presentation effects are acknowledged before the enclosing operation returns. */
export class NativeEffects {
	private pending: Promise<void> = Promise.resolve();
	private failure: unknown;
	private failed = false;
	add(effect: () => Promise<unknown>): void {
		const task = this.pending.then(bindNativeSessionCallback(() => {
			if (this.failed) throw this.failure;
			return effect();
		}));
		this.pending = task.then(() => {}, error => { if (!this.failed) { this.failed = true; this.failure = error; } });
		trackNativeSessionWork(task);
	}
	async settled(): Promise<void> {
		for (;;) { const pending = this.pending; await pending; if (pending === this.pending) break; }
		if (this.failed) throw this.failure;
	}
}

const trustResolutions = new WeakMap<ExtensionContext, boolean>();
export function nativeProjectTrustResolved(context: ExtensionContext): boolean | undefined { return trustResolutions.get(context); }

export function nativeContext(snapshot: NativeContextSnapshot, manager: NativeSessionManager, signal: AbortSignal | undefined,
	request: HostRequest, effects: NativeEffects, models: () => readonly Model<Api>[]): ExtensionContext {
	const notify: ExtensionContext["ui"]["notify"] = (message, type = "info") => {
		effects.add(() => request("native", { port: "ui", action: "notify", text: message, kind: type }));
	};
	async function dialog(ui: Record<string, unknown>): Promise<Record<string, unknown> | null> {
		const answer = await request("native", { port: "dialogs", action: "show", ui });
		if (answer === null) return null;
		if (!answer || typeof answer !== "object" || Array.isArray(answer)) throw new Error("Invalid native dialog answer");
		return answer as Record<string, unknown>;
	}
	const ui = {
		notify,
		setStatus: (key: string, text: string | undefined) => effects.add(() => request("native", { port: "ui", action: "status", key, value: text ?? null })),
		input: async (title: string, placeholder?: string) => {
			const answer = await dialog({ kind: "input", title, ...(placeholder === undefined ? {} : { placeholder }) });
			if (answer === null) return undefined;
			if (answer.type !== "input" || typeof answer.value !== "string") throw new Error("Invalid native input answer");
			return answer.value;
		},
		select: async (title: string, choices: string[]) => {
			const answer = await dialog({ kind: "select", title, choices: choices.map((label, index) => ({ label, value: String(index) })) });
			if (answer === null) return undefined;
			const values = answer.values;
			const index = Array.isArray(values) && values.length === 1 && typeof values[0] === "string" ? Number(values[0]) : -1;
			if (answer.type !== "select" || !Number.isInteger(index) || index < 0 || index >= choices.length) throw new Error("Invalid native selection answer");
			return choices[index];
		},
		confirm: async (title: string, description: string) => {
			const answer = await dialog({ kind: "confirm", title, description });
			if (answer === null) return false;
			if (answer.type !== "confirm" || typeof answer.confirmed !== "boolean") throw new Error("Invalid native confirmation answer");
			return answer.confirmed;
		},
	} satisfies Pick<ExtensionContext["ui"], "notify" | "setStatus" | "input" | "select" | "confirm">;
	async function auth(id: string): Promise<AuthResult | undefined> {
		if (id !== "cursor") throw new Error("Cursor worker cannot resolve another provider's credentials");
		const response = await request("native", { port: "auth", action: "resolve", providerId: id });
		return nativeRequestAuth(response);
	}
	const modelRegistry: Pick<ExtensionContext["modelRegistry"], "getAll" | "getProviderAuth" | "getApiKeyForProvider" | "refresh"> = {
		getAll: () => [...models()],
		getProviderAuth: auth,
		getApiKeyForProvider: async (id: string) => {
			const resolved = await auth(id);
			return resolved?.auth?.apiKey;
		},
		refresh: async (options = {}) => {
			if (options.providers?.some(id => id !== "cursor")) throw new Error("Cursor worker cannot refresh another provider");
			const errors = new Map<string, Error>();
			try {
				options.signal?.throwIfAborted();
				await request("native", { port: "providers", action: "refresh", providerId: "cursor", allowNetwork: options.allowNetwork ?? false, force: options.force ?? false });
			} catch (error) { if (!options.signal?.aborted) errors.set("cursor", error instanceof Error ? error : new Error(String(error))); }
			return { errors, aborted: options.signal?.aborted === true };
		},
	};
	const context: ExtensionContext = {
		cwd: snapshot.cwd, mode: snapshot.mode, hasUI: snapshot.hasUI, sessionManager: manager,
		model: nativeModel(snapshot.model, models()),
		thinkingLevel: snapshot.thinkingLevel, signal,
		isIdle: () => snapshot.isIdle, isProjectTrusted: () => snapshot.projectTrusted,
		hasPendingMessages: () => snapshot.hasPendingMessages, getSystemPrompt: () => snapshot.systemPrompt,
		getContextUsage: () => undefined,
		scopedModels: [],
		abort: () => effects.add(() => request("native", { port: "context", action: "abort" })),
		shutdown: () => effects.add(() => request("native", { port: "context", action: "shutdown" })),
		compact: options => effects.add(() => request("native", { port: "context", action: "compact", options })),
		ui: ui as ExtensionContext["ui"], modelRegistry: modelRegistry as ExtensionContext["modelRegistry"],
	};
	trustResolutions.set(context, snapshot.projectTrustResolved);
	return context;
}

export type NativeTools = ReturnType<ExtensionAPI["getAllTools"]>;
