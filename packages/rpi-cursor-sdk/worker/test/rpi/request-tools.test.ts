import { expect, it, vi } from "vitest";
import type { AssistantMessage, Provider } from "@earendil-works/pi-ai";
import { NativeCursorRuntime } from "../../src/rpi/runtime.js";
import { NativeMessageStream } from "../../src/rpi/stream.js";
import { createNativeCursorToolDefinition } from "../../src/cursor-native-tool-display-tools.js";
import type { WorkerCall } from "../../src/rpi/peer.js";

it("preserves omitted, empty and restricted request tools despite registered replay wrappers", async () => {
	const model = { id: "fixture", name: "Fixture", provider: "cursor", api: "cursor-sdk", baseUrl: "https://cursor.com", reasoning: false,
		input: ["text"], contextWindow: 1000, maxTokens: 100, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 } };
	const session = { metadata: { id: "fixture", createdAt: 0 }, entries: [], branches: [{ lane: "main", revision: 0, leafId: null }], revision: 0, name: null, labels: {} };
	const tools = ["read", "cursor"].map(name => ({ ...createNativeCursorToolDefinition(name as "read" | "cursor", "/fixture"),
		sourceInfo: { source: "builtin", path: name, scope: "temporary", origin: "top-level" }, exposure: "direct" }));
	const call: WorkerCall = { id: "fixture", signal: new AbortController().signal, emit: () => {}, host: async (method, raw) => {
		const input = raw as Record<string, unknown>;
		if (method === "scoped") return call.host(String(input.method), input.input);
		if (input.port === "tools") return tools;
		if (input.port === "context") {
			const { baseUrl: _base, ...publicModel } = model;
			return { cwd: "/fixture", mode: "tui", hasUI: true, projectTrusted: false, projectTrustResolved: false,
				toolRegistration: { disabled: false, allow: null, deny: [] }, toolSelection: { revision: "0", state: null },
				isIdle: true, hasPendingMessages: false, systemPrompt: "fixture", sessionId: "fixture", model: publicModel, tools: ["read", "cursor"], identity: null };
		}
		switch (input.action) {
			case "capture": return { origin: "origin", snapshot: session };
			case "location": return { path: null };
			case "view": return { snapshot: session, leafId: null, branchRevision: 0, projection: { model: null, thinkingLevel: "off", entries: [] } };
			case "release": return null;
			default: throw new Error(`Unexpected host operation ${method}/${input.action}`);
		}
	} };
	const runtime = new NativeCursorRuntime(call, { cwd: "/fixture", sessionDir: "/isolated" });
	vi.spyOn(runtime.extension, "provider").mockReturnValue({ getModels: () => [model] } as unknown as Provider);
	for (const name of ["read", "cursor"] as const) runtime.extension.api.registerTool(createNativeCursorToolDefinition(name, "/fixture"));
	const stream = new NativeMessageStream(() => {});
	stream.end({ role: "assistant", content: [] } as unknown as AssistantMessage);
	const capture = vi.spyOn(runtime.extension, "stream").mockResolvedValue(stream);
	try {
		for (const entry of ["stream", "streamSimple"]) for (const context of [{ messages: [] }, { messages: [], tools: [] },
			{ messages: [], tools: [{ name: "read", description: "request override", parameters: { type: "object", properties: {} } }] }]) {
			await runtime.dispatch("provider.stream", { entry, model, context, options: {}, authMode: "replace" }, call);
			expect(capture.mock.lastCall?.[2]).toBe(context);
		}
	} finally { await runtime.close(); }
});
