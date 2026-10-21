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
	return { surface, snapshot: surface.capture([...host], ["read"]) };
}
describe("native tool surface", () => {
	it("keeps replay activation local and preserves builtin execution ownership", () => {
		const { surface, snapshot } = fixture();
		expect(surface.activate(snapshot, ["read", CURSOR_REPLAY_ACTIVITY_TOOL_NAME])).toEqual(["read"]);
		expect(snapshot.active).toEqual(["read", CURSOR_REPLAY_ACTIVITY_TOOL_NAME]);
		expect(surface.all(snapshot).find(tool => tool.name === "read")?.sourceInfo).toBe(sourceInfo);
		expect(host[0].sourceInfo.source).toBe("builtin");
		expect(surface.activate(snapshot, ["read", "unregistered"])).toEqual(["read"]);
		expect(snapshot.active).toEqual(["read"]);
	});
	it("does not re-advertise executable registrations filtered out by the host", () => {
		const { surface, snapshot } = fixture();
		surface.registered = [...registered, { ...registered[0], definition: { ...registered[0].definition, name: "cursor_activate_skill" } }];
		expect(surface.all(snapshot).map(tool => tool.name)).not.toContain("cursor_activate_skill");
		expect(surface.activate(snapshot, ["cursor_activate_skill"])).toEqual([]);
		expect(snapshot.active).toEqual([]);
	});
	it("retains replay choices across snapshots and honors authoritative external changes", () => {
		const { surface, snapshot } = fixture();
		surface.activate(snapshot, ["read", CURSOR_REPLAY_ACTIVITY_TOOL_NAME]);
		const next = surface.capture([...host], ["read"]);
		expect(next.active).toEqual(snapshot.active);
		expect(surface.capture([...host], []).active).toEqual([]);
		expect(surface.activate(next, [])).toEqual([]);
		expect(surface.capture([...host], []).active).toEqual([]);
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
