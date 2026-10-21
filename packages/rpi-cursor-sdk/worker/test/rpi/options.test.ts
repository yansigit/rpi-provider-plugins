import { expect, it } from "vitest";
import { nativeStreamOptions } from "../../src/rpi/options.js";
import { buildCursorModelSelection } from "../../src/model-discovery.js";

it("maps native enum values to the retained model selection for both fast settings", () => {
	const signal = new AbortController().signal;
	for (const reasoning of ["Off", "Minimal", "Low", "Medium", "High", "Xhigh", "Max"]) {
		const actual = nativeStreamOptions({ reasoning }, signal);
		const expected = reasoning.toLowerCase() as "off" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max";
		for (const fast of [false, true]) expect(buildCursorModelSelection("composer-2.5", actual.reasoning ?? "off", fast))
			.toEqual(buildCursorModelSelection("composer-2.5", expected, fast));
		expect(actual.reasoning).toBe(expected === "off" ? undefined : expected);
	}
	for (const cacheRetention of ["None", "Short", "Long", "none", "short", "long"]) {
		expect(nativeStreamOptions({ cacheRetention }, signal).cacheRetention).toBe(cacheRetention.toLowerCase());
	}
});

it("omits only absent native fields and preserves explicit zero, empty and provider options", () => {
	const signal = new AbortController().signal;
	const headers = { "x-fixture": "value", "x-omit": null };
	const input = { apiKey: "", reasoning: null, timeoutMs: 0, maxRetries: 0, maxRetryDelayMs: 0, maxTokens: 0,
		temperature: 0, sessionId: "", metadata: { privateFixture: true }, env: { CURSOR_API_KEY: "fixture" }, headers,
		thinkingBudgets: null, extraOption: null, toolChoice: "none", signal: "untrusted-wire-signal" };
	const actual = nativeStreamOptions(input, signal);
	expect(actual).toEqual({ ...input, reasoning: undefined, thinkingBudgets: undefined, headers, signal });
	expect(Object.hasOwn(actual, "reasoning")).toBe(false);
	expect(Object.hasOwn(actual, "thinkingBudgets")).toBe(false);
	expect(actual.headers).not.toBe(headers);
	expect(actual.signal).toBe(signal);
	expect(input.reasoning).toBe(null);
	for (const name of ["apiKey", "timeoutMs", "maxRetries", "maxRetryDelayMs", "headers", "metadata", "cacheRetention", "sessionId", "reasoning", "maxTokens", "temperature", "thinkingBudgets"]) {
		const mapped = nativeStreamOptions({ [name]: null }, signal);
		if (name === "headers") expect(mapped.headers).toEqual({});
		else expect(Object.hasOwn(mapped, name)).toBe(false);
	}
});

it("rejects malformed enum and header values without echoing private input", () => {
	for (const input of [{ reasoning: "secret-fixture" }, { reasoning: 2 }, { cacheRetention: "secret-fixture" },
		{ cacheRetention: false }, { headers: "secret-fixture" }, { headers: [] }, { headers: { private: 2 } }]) {
		expect(() => nativeStreamOptions(input, new AbortController().signal)).toThrow(/^Invalid native Cursor /);
		try { nativeStreamOptions(input, new AbortController().signal); } catch (error) { expect(String(error)).not.toContain("secret-fixture"); }
	}
});
