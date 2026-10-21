import { realpathSync, readFileSync } from "node:fs";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { basename, dirname, join } from "node:path";
import { afterEach, expect, it, vi } from "vitest";
import { CONFIG_DIR_NAME, getAgentDir as getPiAgentDir } from "@earendil-works/pi-coding-agent";

afterEach(() => vi.resetModules());

it("leaves the retained Pi path unchanged until native initialization", async () => {
	const paths = await import("../../src/rpi/paths.js");
	expect(paths.getAgentDir()).toBe(getPiAgentDir());
	expect(paths.getConfigDirName()).toBe(CONFIG_DIR_NAME);
	expect(paths.getNativeSdkWorkspaceRoot("/project")).toBeUndefined();
	expect(paths.renderContextFilePath('/a&<"b>/AGENTS.md')).toBe('/a&<"b>/AGENTS.md');
});

it("selects rpi configuration/cache paths once and never calls the original SDK store resolver", async () => {
	const directory = await mkdtemp(join(tmpdir(), "rpi-cursor-paths-"));
	try {
		const paths = await import("../../src/rpi/paths.js");
		paths.initializeNativePaths(directory);
		paths.initializeNativePaths(directory);
		expect(paths.renderContextFilePath('/a&<"b>/AGENTS.md')).toBe('/a&amp;&lt;&quot;b&gt;/AGENTS.md');
		expect(paths.getAgentDir()).toBe(directory); expect(paths.getConfigDirName()).toBe(".rpi");
		expect(() => paths.initializeNativePaths(join(directory, "other"))).toThrow("ownership cannot change");
		const config = await import("../../src/cursor-config.js");
		expect(config.getCursorSdkUserConfigPath()).toBe(join(directory, "cursor-sdk.json"));
		expect(config.getCursorSdkProjectConfigPath(directory)).toBe(join(directory, ".rpi", "cursor-sdk.json"));
		const stores = await import("../../src/cursor-session-store.js");
		const forbidden = vi.fn(() => { throw new Error("must not touch Pi/Cursor SDK stores"); });
		stores.__testUtils.setSdkOperations({ getDefaultStateRoot: forbidden, openSqliteStore: forbidden });
		const roots: string[] = [];
		for (const scope of ["native-session-a", "native-session-b"]) {
			await stores.withCursorSessionStoreIdentities(directory, scope, undefined, async identities => {
				expect(identities.domain).toBe("default");
				const nativeRoot = paths.getNativeSdkWorkspaceRoot(directory)!;
				expect(identities.workspaceRoot).toBe(join(realpathSync(dirname(nativeRoot)), basename(nativeRoot)));
				expect(identities.sessionStore.stateRoot.startsWith(realpathSync(join(directory, "cursor-sdk", "stores")))).toBe(true);
				roots.push(identities.sessionStore.stateRoot);
			});
		}
		expect(roots[0]).not.toBe(roots[1]); expect(forbidden).not.toHaveBeenCalled();
		const { SqliteLocalAgentStore } = await import("@cursor/sdk/sqlite");
		stores.__testUtils.setSdkOperations({ getDefaultStateRoot: forbidden, openSqliteStore: options => SqliteLocalAgentStore.open(options) });
		const opened = await stores.openCursorSessionStoreForScope({ cwd: directory, scopeKey: "native-session-a", persistent: true });
		try { expect(opened.sessionStore.identity.stateRoot).toBe(roots[0]); }
		finally { await opened.sessionStore.dispose(); }
		expect(forbidden).not.toHaveBeenCalled();
		const catalog = await import("../../src/model-list-cache.js");
		expect(catalog.saveModelListCache("fixture-fingerprint", [])).toBe(true);
		expect(JSON.parse(readFileSync(join(directory, "cursor-sdk-model-list.json"), "utf8")).keyFingerprint).toBe("fixture-fingerprint");
		const windows = await import("../../src/context-window-cache.js");
		windows.saveCachedContextWindow("native-fixture-model", 12345);
		expect(JSON.parse(readFileSync(join(directory, "cursor-sdk-context-windows.json"), "utf8")).contextWindows["native-fixture-model"]).toBe(12345);
		stores.__testUtils.setSdkOperations(undefined);
	} finally { await rm(directory, { recursive: true, force: true }); }
});

it("rejects relative and malformed storage ownership", async () => {
	const paths = await import("../../src/rpi/paths.js");
	for (const path of ["", "relative", "/invalid\0path"]) expect(() => paths.initializeNativePaths(path)).toThrow("Invalid native");
});
