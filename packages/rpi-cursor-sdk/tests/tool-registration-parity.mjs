import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import { AgentSession } from "../runtime/node_modules/@earendil-works/pi-coding-agent/dist/core/agent-session.js";
import { NativeToolSurface, toolBinding } from "../worker/dist/rpi/tool-surface.js";
import { createNativeCursorToolDefinition } from "../worker/dist/cursor-native-tool-display-tools.js";

const pi = JSON.parse(await readFile(new URL("../runtime/node_modules/@earendil-works/pi-coding-agent/package.json", import.meta.url)));
assert.equal(pi.version, "1.3.0");
const names = ["grep", "find", "ls", "cursor", "read", "bash", "edit", "write"];
const source = { source: "extension", path: "fixture", scope: "temporary", origin: "top-level" };
const definition = name => createNativeCursorToolDefinition(name, "/fixture");
test("Pi 1.3 edit preparation survives declarative binding", async () => {
	const { createNativeCursorToolDefinition: integratedDefinition } = await import("../runtime/dist/cursor-native-tool-display-tools.js");
	const edit = integratedDefinition("edit", "/fixture");
	assert.equal(typeof edit.prepareArguments, "function");
	assert.equal(toolBinding({ definition: edit, sourceInfo: source }).binding.prepareArguments, true);
	assert.deepEqual(edit.prepareArguments({ path: "fixture", oldText: "before", newText: "after" }),
		{ path: "fixture", edits: [{ oldText: "before", newText: "after" }] });
});
for (const baseNames of [[], ["read", "bash", "edit", "write"], names.filter(name => name !== "cursor")]) {
	for (const allow of [null, [], ["read"], names]) for (const deny of [[], ["read"]]) for (const initiallyActive of [false, true]) {
		test(`Pi 1.3 registration: base=${baseNames.length}, allow=${allow}, deny=${deny}, active=${initiallyActive}`, () => {
			const permits = name => (allow === null || allow.includes(name)) && !deny.includes(name);
			const base = baseNames.filter(permits).map(name => definition(name));
			let active = initiallyActive ? base.map(tool => tool.name) : [];
			const registered = new Map();
			// Exercise Pi's unchanged registry refresh, replacing only its agent executor.
			const reference = Object.assign(Object.create(AgentSession.prototype), {
				_toolRegistry: new Map(base.map(tool => [tool.name, tool])),
				_toolDefinitions: new Map(base.map(tool => [tool.name, { definition: tool }])),
				_baseToolDefinitions: new Map(base.map(tool => [tool.name, tool])),
				_customTools: [], _pendingToolNames: new Set(),
				_allowedTools: allow === null ? undefined : name => allow.includes(name),
				_excludedTools: name => deny.includes(name),
				_extensionRunner: { getAllRegisteredTools: () => [...registered.values()] },
				getActiveToolNames: () => [...active],
				_setActiveTools: requested => { active = requested.filter(name => reference._toolRegistry.has(name) && permits(name)); },
			});
			const surface = new NativeToolSurface();
			const snapshot = surface.capture(base.map(tool => ({ ...tool, exposure: "direct", sourceInfo: { ...source, source: "builtin" } })), active,
				{ revision: "0", state: null }, { disabled: false, allow, deny });
			for (const [index, name] of [...names, "read"].entries()) {
				const item = { definition: definition(name), sourceInfo: source };
				registered.set(name, item); reference._refreshToolRegistry();
				const change = surface.register([...registered.values()], snapshot);
				assert.deepEqual(snapshot.active, active, `activation before acknowledgement: ${name}`);
				assert.deepEqual(surface.all(snapshot).map(tool => tool.name), [...reference._toolRegistry.keys()]);
				assert(change);
				surface.acknowledge(snapshot, change, { registered: permits(name) ? [name] : [], tools: active, selection: { revision: String(index + 1), state: null } });
				// Refreshing an inactive existing tool must follow the pinned allowlist rule.
				if (index === names.length - 1) { active = []; surface.activate(snapshot, []); }
			}
		});
	}
}
