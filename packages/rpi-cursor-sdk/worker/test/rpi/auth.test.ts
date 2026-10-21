import { afterEach, expect, it, vi } from "vitest";
import type { Provider } from "@earendil-works/pi-ai";
import { cursorApiKeyAuth } from "../../src/cursor-api-key.js";
import { cursorNativeAuth } from "../../src/rpi/auth.js";
const provider = { auth: { apiKey: cursorApiKeyAuth() } } as Provider;
const signal = () => new AbortController().signal;
afterEach(() => vi.unstubAllEnvs());

it("uses only the host's request environment, with retained key precedence and fallback", async () => {
	vi.stubEnv("CURSOR_API_KEY", "forbidden-ambient-account");
	vi.stubEnv("PI_CURSOR_HIDE_MODELS_WHEN_LOGGED_OUT", "false");
	for (const [input, key] of [
		[{ credential: { type: "api_key", key: " stored ", env: { CURSOR_API_KEY: "credential-env" } }, env: { CURSOR_API_KEY: "request-env" } }, "stored"],
		[{ credential: { type: "api_key", env: { CURSOR_API_KEY: "credential-env" } }, env: { CURSOR_API_KEY: "request-env" } }, "credential-env"],
		[{ env: { CURSOR_API_KEY: "request-env" } }, "request-env"],
		[{ env: {} }, "pi-cursor-sdk-cursor-api-key-placeholder"],
	] as const) {
		expect(await cursorNativeAuth(provider, "resolveApiKey", input, signal())).toMatchObject({ kind: "auth", payload: { apiKey: key } });
	}
	vi.stubEnv("PI_CURSOR_HIDE_MODELS_WHEN_LOGGED_OUT", "true");
	expect(await cursorNativeAuth(provider, "resolveApiKey", { env: {} }, signal())).toEqual({ version: 1, kind: "unavailable", payload: null });
	expect(process.env.CURSOR_API_KEY).toBe("forbidden-ambient-account");
});

it("retains the installed API-key login's prompt and exact credential, including empty input", async () => {
	const prompts: unknown[] = [];
	await provider.auth.apiKey!.login!({ signal: signal(), prompt: async prompt => { prompts.push(prompt); return "reference"; }, notify: () => {} });
	const first = await cursorNativeAuth(provider, "login", { methodId: "api_key" }, signal()) as { payload: { title: string; continuation: unknown } };
	expect(first.payload.title).toBe((prompts[0] as { message: string }).message);
	for (const value of ["", " key with whitespace "]) {
		const reference = await provider.auth.apiKey!.login!({ signal: signal(), prompt: async () => value, notify: () => {} });
		expect(await cursorNativeAuth(provider, "continue", { continuation: first.payload.continuation, value }, signal()))
			.toEqual({ version: 1, kind: "credential", payload: reference });
	}
});

it("rejects unsupported, cancelled and malformed private auth operations", async () => {
	for (const [action, input] of [
		["login", { methodId: "oauth" }], ["continue", { continuation: {}, value: "secret" }],
		["resolveApiKey", { credential: { type: "oauth", access: "secret" } }],
		["resolveApiKey", { credential: { type: "api_key", key: 5 } }],
		["resolveApiKey", { env: { CURSOR_API_KEY: false } }], ["refresh", {}],
	] as const) await expect(cursorNativeAuth(provider, action, input, signal())).rejects.toThrow();
	const cancelled = new AbortController(); cancelled.abort();
	await expect(cursorNativeAuth(provider, "resolveApiKey", {}, cancelled.signal)).rejects.toThrow();
});
