import { describe, expect, it } from "vitest";
import type { Context } from "@earendil-works/pi-ai";
import type { RegisteredTool } from "@earendil-works/pi-coding-agent";
import { createNativeCursorToolDefinition } from "../../src/cursor-native-tool-display-tools.js";
import { CURSOR_REPLAY_ACTIVITY_TOOL_NAME } from "../../src/cursor-tool-presentation-registry.js";
import { NativeToolSurface, toolMetadata } from "../../src/rpi/tool-surface.js";

const sourceInfo = { source: "extension", path: "fixture", scope: "temporary", origin: "top-level" } as const;
const registered = ["read", CURSOR_REPLAY_ACTIVITY_TOOL_NAME].map(name => ({
	definition: createNativeCursorToolDefinition(name as "read" | typeof CURSOR_REPLAY_ACTIVITY_TOOL_NAME, "/fixture"), sourceInfo,
})) as RegisteredTool[];
const host = [{ name: "read", description: "host read", parameters: { type: "object" }, exposure: "direct", sourceInfo: { ...sourceInfo, source: "builtin" } }] as const;
function fixture() {
	const surface = new NativeToolSurface(); surface.registered = registered;
	return { surface, snapshot: surface.capture([...host], ["read"], { revision: "0", state: null }) };
}
describe("native tool surface", () => {
	it("keeps replay activation local and preserves builtin execution ownership", () => {
		const { surface, snapshot } = fixture();
		expect(surface.activate(snapshot, ["read", CURSOR_REPLAY_ACTIVITY_TOOL_NAME]).tools).toEqual(["read"]);
		expect(snapshot.active).toEqual(["read", CURSOR_REPLAY_ACTIVITY_TOOL_NAME]);
		expect(surface.all(snapshot).find(tool => tool.name === "read")?.sourceInfo).toBe(sourceInfo);
		expect(host[0].sourceInfo.source).toBe("builtin");
		expect(surface.activate(snapshot, ["read", "unregistered"]).tools).toEqual(["read"]);
		expect(snapshot.active).toEqual(["read"]);
	});
	it("does not re-advertise executable registrations filtered out by the host", () => {
		const { surface, snapshot } = fixture();
		surface.registered = [...registered, { ...registered[0], definition: { ...registered[0].definition, name: "cursor_activate_skill" } }];
		expect(surface.all(snapshot).map(tool => tool.name)).not.toContain("cursor_activate_skill");
		expect(surface.activate(snapshot, ["cursor_activate_skill"]).tools).toEqual([]);
		expect(snapshot.active).toEqual([]);
	});
	it("applies registration allowlists, exclusions and disablement to local replay declarations", () => {
		for (const policy of [
			{ disabled: true, allow: null, deny: [] },
			{ disabled: false, allow: ["read"], deny: [] },
			{ disabled: false, allow: null, deny: [CURSOR_REPLAY_ACTIVITY_TOOL_NAME] },
		]) {
			const { surface, snapshot } = fixture();
			const change = surface.activate(snapshot, ["read", CURSOR_REPLAY_ACTIVITY_TOOL_NAME]);
			const next = surface.capture([...host], ["read"], { revision: "1", state: change.state }, policy);
			expect(surface.all(next).map(tool => tool.name)).not.toContain(CURSOR_REPLAY_ACTIVITY_TOOL_NAME);
			expect(next.active).not.toContain(CURSOR_REPLAY_ACTIVITY_TOOL_NAME);
			expect(surface.activate(next, ["read", CURSOR_REPLAY_ACTIVITY_TOOL_NAME]).tools).toEqual(["read"]);
			expect(surface.context(next, { messages: [], tools: [] } as Context).tools?.map(tool => tool.name)).not.toContain(CURSOR_REPLAY_ACTIVITY_TOOL_NAME);
		}
	});
	it("recovers committed replay choices before acknowledgement and clears them on same-name reselection", () => {
		const { surface, snapshot } = fixture();
		const change = surface.activate(snapshot, [CURSOR_REPLAY_ACTIVITY_TOOL_NAME, "read"]);
		const selection = { revision: "1", state: change.state };
		expect(surface.capture([...host], ["read"], selection).active).toEqual(change.active);
		surface.acknowledge(snapshot, change, { tools: ["read"], selection });
		expect(snapshot.selection).toEqual(selection);
		expect(surface.capture([...host], ["read"], { revision: "2", state: null }).active).toEqual(["read"]);
		const fresh = fixture().surface;
		expect(fresh.capture([...host], ["read"], selection).active).toEqual(["read"]);
	});
	it("keeps snapshots independent of later selections and acknowledgement order", () => {
		const { surface, snapshot } = fixture();
		const first = surface.activate(snapshot, ["read", CURSOR_REPLAY_ACTIVITY_TOOL_NAME]);
		const second = surface.activate(snapshot, []);
		const secondReceipt = { tools: [], selection: { revision: "2", state: second.state } };
		surface.acknowledge(snapshot, second, secondReceipt);
		surface.acknowledge(snapshot, first, { tools: ["read"], selection: { revision: "1", state: first.state } });
		expect(snapshot.active).toEqual([]);
		expect(snapshot.selection.revision).toBe("2");
		expect(surface.capture([...host], ["read"], { revision: "1", state: first.state }).active).toEqual(first.active);
		expect(surface.capture([...host], [], secondReceipt.selection).active).toEqual([]);
	});
	it("rejects mismatched and malformed acknowledgements without altering local selection", () => {
		const { surface, snapshot } = fixture();
		const change = surface.activate(snapshot, [CURSOR_REPLAY_ACTIVITY_TOOL_NAME]);
		for (const receipt of [null, { tools: ["read"] }, { tools: [], selection: { revision: "1", state: null } },
			{ tools: [], selection: { revision: "1", state: { ...change.state, tools: [] } } }]) {
			expect(() => surface.acknowledge(snapshot, change, receipt)).toThrow();
			expect(snapshot.active).toEqual([CURSOR_REPLAY_ACTIVITY_TOOL_NAME]);
		}
		expect(() => surface.capture([...host], [], { revision: "1", state: { ...change.state, tools: [12] } })).toThrow();
		const hidden = surface.capture([...host], [], { revision: "1", state: { ...change.state, tools: ["read", CURSOR_REPLAY_ACTIVITY_TOOL_NAME] } });
		expect(hidden.active).toEqual([CURSOR_REPLAY_ACTIVITY_TOOL_NAME]);
	});
	it("projects active replay declarations into the request without mutating host context or executing", () => {
		const { surface, snapshot } = fixture();
		surface.activate(snapshot, ["read", CURSOR_REPLAY_ACTIVITY_TOOL_NAME]);
		const context = { messages: [], tools: [{ name: "read", description: "host read", parameters: { type: "object" } }] } as Context;
		const projected = surface.context(snapshot, context);
		expect(projected.tools?.map(tool => tool.name)).toEqual(["read", CURSOR_REPLAY_ACTIVITY_TOOL_NAME]);
		expect(projected.tools?.[0].parameters).toEqual(registered[0].definition.parameters);
		expect(projected.tools?.[0]).not.toHaveProperty("sourceInfo");
		expect(context.tools).toHaveLength(1);
		expect(context.tools?.[0].description).toBe("host read");
		const unspecified = { messages: [] } as Context;
		expect(surface.context(snapshot, unspecified)).toBe(unspecified);
	});
	it("retains declared execution and prompt policy in the host manifest", () => {
		const definition = { ...registered[0].definition, label: "Question", executionMode: "sequential", promptSnippet: "Ask once", promptGuidelines: ["Wait"] } as RegisteredTool["definition"];
		expect(toolMetadata({ definition, sourceInfo })).toMatchObject({ label: "Question", executionMode: "sequential", exposure: "direct", promptSnippet: "Ask once", promptGuidelines: ["Wait"], sourceInfo });
	});
});
