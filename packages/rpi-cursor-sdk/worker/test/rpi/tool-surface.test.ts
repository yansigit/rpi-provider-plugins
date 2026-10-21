import { describe, expect, it } from "vitest";
import type { RegisteredTool } from "@earendil-works/pi-coding-agent";
import { createNativeCursorToolDefinition } from "../../src/cursor-native-tool-display-tools.js";
import { CURSOR_REPLAY_ACTIVITY_TOOL_NAME } from "../../src/cursor-tool-presentation-registry.js";
import { NativeToolSurface, toolMetadata, toolBinding } from "../../src/rpi/tool-surface.js";
const sourceInfo = { source: "extension", path: "fixture", scope: "temporary", origin: "top-level" } as const;
const registered = ["read", CURSOR_REPLAY_ACTIVITY_TOOL_NAME].map(name => ({
	definition: createNativeCursorToolDefinition(name as "read" | typeof CURSOR_REPLAY_ACTIVITY_TOOL_NAME, "/fixture"), sourceInfo,
})) as RegisteredTool[];
const host = [{ name: "read", description: "host read", parameters: { type: "object" }, exposure: "direct", sourceInfo: { ...sourceInfo, source: "builtin" } }] as const;
function fixture(active = ["read"]) {
	const surface = new NativeToolSurface();
	return { surface, snapshot: surface.capture([...host], active, { revision: "0", state: null }) };
}
const selection = (revision: string) => ({ revision, state: null });
describe("native tool surface", () => {
	it("publishes native bindings and immediately mirrors Pi's registration activation", () => {
		const { surface, snapshot } = fixture([]);
		const change = surface.register(registered, snapshot)!;
		expect(change.bindings.map(tool => tool.name)).toEqual(["read", CURSOR_REPLAY_ACTIVITY_TOOL_NAME]);
		expect(snapshot.active).toEqual([CURSOR_REPLAY_ACTIVITY_TOOL_NAME]);
		expect(surface.all(snapshot).find(tool => tool.name === "read")?.sourceInfo).toBe(sourceInfo);
		expect(host[0].sourceInfo.source).toBe("builtin");
		surface.acknowledge(snapshot, change, { registered: change.bindings.map(tool => tool.name), tools: [...snapshot.active], selection: selection("1") });
		expect(snapshot.selection.revision).toBe("1");
		expect(surface.register(registered, snapshot)).toBeUndefined();
		expect(surface.activate(snapshot, ["read", "unregistered", CURSOR_REPLAY_ACTIVITY_TOOL_NAME]).tools).toEqual(["read", CURSOR_REPLAY_ACTIVITY_TOOL_NAME]);
	});
	it("registers with no builtin host tools and never revives filtered executable tools", () => {
		const surface = new NativeToolSurface();
		const snapshot = surface.capture([], [], selection("0"));
		surface.register([...registered, { ...registered[0], definition: { ...registered[0].definition, name: "cursor_activate_skill" } }], snapshot);
		expect(surface.all(snapshot).map(tool => tool.name)).toEqual(["read", CURSOR_REPLAY_ACTIVITY_TOOL_NAME]);
		expect(snapshot.active).toEqual(["read", CURSOR_REPLAY_ACTIVITY_TOOL_NAME]);
	});
	it("applies registration allowlists, exclusions and disablement before publication", () => {
		for (const policy of [
			{ disabled: true, allow: null, deny: [] },
			{ disabled: false, allow: ["read"], deny: [] },
			{ disabled: false, allow: null, deny: [CURSOR_REPLAY_ACTIVITY_TOOL_NAME] },
		]) {
			const surface = new NativeToolSurface();
			const snapshot = surface.capture([], [], selection("0"), policy);
			const change = surface.register(registered, snapshot);
			expect(change?.bindings.map(tool => tool.name) ?? []).toEqual(policy.disabled ? [] : ["read"]);
			expect(surface.all(snapshot).map(tool => tool.name)).not.toContain(CURSOR_REPLAY_ACTIVITY_TOOL_NAME);
		}
	});
	it("uses committed activation exactly and ignores stale worker presentation state", () => {
		const { surface, snapshot } = fixture(); surface.register(registered, snapshot);
		const tools = snapshot.tools;
		expect(surface.capture(tools, [], { revision: "1", state: { runtime: "old", tools: ["read"] } }).active).toEqual([]);
		const fresh = new NativeToolSurface();
		const inherited = fresh.capture(tools, ["read"], selection("2"));
		expect(fresh.all(inherited)[0].sourceInfo.source).toBe("builtin");
		fresh.register(registered, inherited);
		expect(inherited.active).toEqual(["read"]);
		expect(fresh.all(inherited)[0].sourceInfo).toBe(sourceInfo);
	});
	it("preserves foreign tool provenance and rejects replacement", () => {
		const { surface, snapshot } = fixture();
		snapshot.tools[0] = { ...host[0], sourceInfo: { ...sourceInfo, path: "foreign" } };
		expect(surface.all(snapshot)[0].sourceInfo.path).toBe("foreign");
		expect(() => surface.register(registered, snapshot)).toThrow(/another registration/);
		expect(surface.all(snapshot)[0].sourceInfo.path).toBe("foreign");
	});
	it("keeps snapshots independent of acknowledgement order", () => {
		const { surface, snapshot } = fixture();
		const publication = surface.register(registered, snapshot)!;
		const first = surface.activate(snapshot, [CURSOR_REPLAY_ACTIVITY_TOOL_NAME, "read"]);
		const second = surface.activate(snapshot, []);
		surface.acknowledge(snapshot, second, { tools: [], selection: selection("3") });
		surface.acknowledge(snapshot, first, { tools: first.tools, selection: selection("2") });
		surface.acknowledge(snapshot, publication, { registered: publication.bindings.map(tool => tool.name), tools: publication.active, selection: selection("1") });
		expect(snapshot.active).toEqual([]);
		expect(snapshot.selection.revision).toBe("3");
	});
	it("rejects malformed or mismatched receipts", () => {
		const { surface, snapshot } = fixture();
		const publication = surface.register(registered, snapshot)!;
		expect(() => surface.acknowledge(snapshot, publication, { registered: ["read"], tools: [], selection: selection("1") })).toThrow();
		const change = surface.activate(snapshot, [CURSOR_REPLAY_ACTIVITY_TOOL_NAME]);
		for (const receipt of [null, { tools: ["read"], selection: selection("1") }, { tools: [], selection: { revision: "1", state: {} } }]) {
			expect(() => surface.acknowledge(snapshot, change, receipt)).toThrow();
			expect(snapshot.active).toEqual([CURSOR_REPLAY_ACTIVITY_TOOL_NAME]);
		}
	});
	it("retains exact schema, execution, prompt and missing-recording policy in host bindings", () => {
		const definition = { ...registered[0].definition, label: "Question", executionMode: "sequential", promptSnippet: "Ask once", promptGuidelines: ["Wait"] } as RegisteredTool["definition"];
		const item = { definition, sourceInfo };
		expect(toolMetadata(item)).toMatchObject({ label: "Question", executionMode: "sequential", exposure: "direct", promptSnippet: "Ask once", promptGuidelines: ["Wait"], sourceInfo });
		expect(toolBinding(item)).toMatchObject({ binding: { kind: "builtin", name: "read" }, parameters: definition.parameters,
			recordedCallPrefix: "cursor-replay-", recordedError: "No recorded Cursor read result was available. This replay-only call does not execute work." });
		expect(toolBinding(registered[1]).binding).toEqual({ kind: "recorded", error: "No recorded Cursor activity result was available. This replay-only tool does not execute work directly." });
		expect(toolBinding(item)).not.toHaveProperty("sourceInfo");
	});
});
