import { expect, it, vi } from "vitest";
import { contextSnapshot, toolSelection, NativeEffects, nativeContext, type HostRequest } from "../../src/rpi/context.js";
import type { NativeSessionManager } from "../../src/rpi/session.js";
import { __testUtils as scopeTests, registerCursorSessionScope, getCursorSessionProjectTrusted } from "../../src/cursor-session-scope.js";
import { createEventHarness, createExtensionTestContext } from "../helpers/pi-harness.js";

const snapshot = () => contextSnapshot({ cwd: "/fixture", mode: "rpc", hasUI: true, projectTrusted: false, projectTrustResolved: false, toolRegistration: { disabled: false, allow: null, deny: [] }, toolSelection: { revision: "0", state: null },
	isIdle: true, hasPendingMessages: false, systemPrompt: "actual prompt", sessionId: "session", tools: [], identity: null,
	model: { id: "fixture", api: "cursor-sdk", provider: "cursor", name: "Fixture", reasoning: false, input: ["text"],
		cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 }, contextWindow: 100, maxTokens: 20 },
});

it("rejects private model bindings and missing authoritative mode/trust/identity", () => {
	for (const change of [ { mode: "unknown" }, { toolRegistration: undefined }, { toolRegistration: { disabled: false, allow: [1], deny: [] } }, { projectTrusted: undefined }, { projectTrustResolved: undefined }, { identity: { purpose: "prompt" } },
		{ model: { ...snapshot().model, headers: { Authorization: "fixture" } } }, { model: { ...snapshot().model, baseUrl: "private" } } ]) {
		expect(() => contextSnapshot({ ...snapshot(), ...change })).toThrow();
	}
});

it("uses explicit native trust provenance even when Pi has previously resolved the same cwd", async () => {
	const pi = createEventHarness(); registerCursorSessionScope(pi);
	const manager = createExtensionTestContext().sessionManager as unknown as NativeSessionManager;
	scopeTests.recordProjectTrustResolution(snapshot().cwd);
	try {
		for (const [projectTrusted, projectTrustResolved, expected] of [[true, false, false], [true, true, true], [false, true, false]]) {
			const context = nativeContext({ ...snapshot(), projectTrusted, projectTrustResolved }, manager, undefined,
				async () => { throw new Error("unexpected host call"); }, new NativeEffects(), () => []);
			await pi.invokeEventWithContext("session_start", { type: "session_start", reason: "startup" }, context);
			expect(getCursorSessionProjectTrusted()).toBe(expected);
		}
	} finally { scopeTests.reset(); }
});

it("serializes presentation acknowledgements and retains a failure while draining later work", async () => {
	const effects = new NativeEffects(), seen: number[] = [];
	let release!: () => void;
	effects.add(async () => { seen.push(1); await new Promise<void>(resolve => { release = resolve; }); throw new Error("failed acknowledgement"); });
	effects.add(async () => { seen.push(2); });
	let settled = false; const pending = effects.settled().finally(() => { settled = true; });
	await Promise.resolve(); expect(seen).toEqual([1]); expect(settled).toBe(false);
	release(); await expect(pending).rejects.toThrow("failed acknowledgement"); expect(seen).toEqual([1, 2]);
});

it("maps dialog controls and outcomes without inventing a choice or forwarding another provider's auth", async () => {
	const request = vi.fn<HostRequest>(), effects = new NativeEffects();
	const context = nativeContext(snapshot(), {} as NativeSessionManager, undefined, request, effects, () => []);
	expect(context.isProjectTrusted()).toBe(false); expect(context.getSystemPrompt()).toBe("actual prompt");
	request.mockResolvedValueOnce({ type: "select", values: ["1"], query: "" });
	expect(await context.ui.select("Pick", ["same", "same"])).toBe("same");
	expect(request.mock.lastCall?.[1]).toEqual({ port: "dialogs", action: "show", ui: { kind: "select", title: "Pick", choices: [{ value: "0", label: "same" }, { value: "1", label: "same" }] } });
	request.mockResolvedValueOnce({ type: "select", values: ["bad"] });
	await expect(context.ui.select("Pick", ["one"])).rejects.toThrow("selection answer");
	request.mockResolvedValueOnce(null); expect(await context.ui.confirm("Confirm", "Description")).toBe(false);
	request.mockResolvedValueOnce({ type: "input", value: "  exact  " }); expect(await context.ui.input("Input")).toBe("  exact  ");
	context.ui.notify("Notice", "warning"); context.ui.setStatus("cursor", undefined);
	await effects.settled();
	expect(request.mock.calls.slice(-2).map(call => call[1])).toEqual([
		{ port: "ui", action: "notify", text: "Notice", kind: "warning" },
		{ port: "ui", action: "status", key: "cursor", value: null },
	]);
	await expect(context.modelRegistry.getProviderAuth("google")).rejects.toThrow("another provider");
	request.mockResolvedValueOnce({ apiKey: "selected-fixture", headers: { "X-Fixture": "selected" }, baseUrl: "https://fixture.invalid", env: { SELECTED: "true" }, source: "fixture" });
	expect(await context.modelRegistry.getProviderAuth("cursor")).toEqual({
		auth: { apiKey: "selected-fixture", headers: { "X-Fixture": "selected" }, baseUrl: "https://fixture.invalid" }, env: { SELECTED: "true" }, source: "fixture",
	});
	request.mockResolvedValueOnce({ apiKey: 42 });
	await expect(context.modelRegistry.getProviderAuth("cursor")).rejects.toThrow("authentication text");
});

it("validates exact unsigned selection revisions and required state", () => {
	for (const revision of [undefined, 0, "-1", "01", "1.5", "18446744073709551616"]) expect(() => toolSelection({ revision, state: null })).toThrow();
	expect(() => toolSelection({ revision: "0" })).toThrow();
	expect(toolSelection({ revision: "18446744073709551615", state: null }).revision).toBe("18446744073709551615");
});
