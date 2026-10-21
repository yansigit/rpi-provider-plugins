import { afterEach, expect, it, vi } from "vitest";
import { resolveCursorApiKey } from "../../src/cursor-api-key.js";
import { withNativeAuthEnvironment } from "../../src/rpi/environment.js";

afterEach(() => vi.unstubAllEnvs());
it("isolates concurrent account environments and preserves the original Pi fallback outside native calls", async () => {
	vi.stubEnv("CURSOR_API_KEY", "default-account");
	const keys = await Promise.all([
		withNativeAuthEnvironment("replace", { CURSOR_API_KEY: " account-a " }, async () => { await Promise.resolve(); return resolveCursorApiKey("CURSOR_API_KEY"); }),
		withNativeAuthEnvironment("replace", {}, async () => { await Promise.resolve(); return resolveCursorApiKey("CURSOR_API_KEY"); }),
		withNativeAuthEnvironment("ambient", {}, async () => { await Promise.resolve(); return resolveCursorApiKey("CURSOR_API_KEY"); }),
		withNativeAuthEnvironment("ambient", { CURSOR_API_KEY: "" }, async () => { await Promise.resolve(); return resolveCursorApiKey("CURSOR_API_KEY"); }),
	]);
	expect(keys).toEqual(["account-a", undefined, "default-account", undefined]);
	expect(process.env.CURSOR_API_KEY).toBe("default-account");
	expect(resolveCursorApiKey("CURSOR_API_KEY")).toBe("default-account");
	expect(withNativeAuthEnvironment("replace", {}, () => resolveCursorApiKey("explicit-key"))).toBe("explicit-key");
});
it("captures request overrides and rejects malformed modes or non-string overrides", () => {
	const env = { CURSOR_API_KEY: "captured" };
	expect(withNativeAuthEnvironment("replace", env, () => { env.CURSOR_API_KEY = "changed"; return resolveCursorApiKey("CURSOR_API_KEY"); })).toBe("captured");
	for (const env of [null, [], { CURSOR_API_KEY: 2 }]) expect(() => withNativeAuthEnvironment("replace", env as never, () => {})).toThrow("Invalid native");
	expect(() => withNativeAuthEnvironment("unknown" as never, {}, () => {})).toThrow("Invalid native");
});
