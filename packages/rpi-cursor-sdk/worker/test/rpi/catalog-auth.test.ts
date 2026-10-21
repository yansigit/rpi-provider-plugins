import { afterEach, expect, it, vi } from "vitest";
import { registerCursorProviderBinding } from "../../src/cursor-provider-binding.js";
import { discoverModels } from "../../src/model-discovery.js";
import { NativeCursorCatalog } from "../../src/rpi/catalog.js";
import { nativeRequestAuth } from "../../src/rpi/auth.js";
import { createPiHarness, createExtensionTestContext } from "../helpers/pi-harness.js";

vi.mock("../../src/model-discovery.js", () => ({ discoverModels: vi.fn(async () => []) }));
afterEach(() => vi.unstubAllEnvs());

it.each([false, true])("resolves each native refresh from its callback, including no credentials (session bound: %s)", async bound => {
	vi.stubEnv("CURSOR_API_KEY", "ambient-fixture-must-not-be-used");
	const pi = createPiHarness();
	registerCursorProviderBinding(pi)([], () => {});
	const provider = pi._registered[0]!.provider!;
	const previousRegistry = vi.fn(async () => ({ auth: { apiKey: "previous-callback-fixture" } }));
	if (bound) {
		const context = createExtensionTestContext();
		vi.spyOn(context.modelRegistry, "getProviderAuth").mockImplementation(previousRegistry);
		await pi.invokeEventWithContext("session_start", { type: "session_start", reason: "startup" }, context);
	}
	const catalog = new NativeCursorCatalog(provider);
	for (const key of ["first-callback-fixture", "second-callback-fixture", undefined]) {
		const auth = nativeRequestAuth(key === undefined ? null : { apiKey: key, env: {}, source: "credential" });
		await catalog.refresh({ publicationId: "fixture", allowNetwork: false, force: false }, async () => { throw new Error("unexpected publication"); }, new AbortController().signal, auth);
		expect(vi.mocked(discoverModels).mock.calls.at(-1)?.[0]?.apiKey).toBe(key);
	}
	expect(previousRegistry).not.toHaveBeenCalled();
	if (bound) {
		await provider.refreshModels?.({ allowNetwork: false, signal: new AbortController().signal, publish: async () => true });
		expect(previousRegistry).toHaveBeenCalledTimes(1);
		expect(vi.mocked(discoverModels).mock.calls.at(-1)?.[0]?.apiKey).toBe("previous-callback-fixture");
	}
	await pi.runSessionShutdown({ reason: "quit" });
});
