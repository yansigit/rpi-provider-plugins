import type { RegisteredTool } from "@earendil-works/pi-coding-agent";
import { isNativeCursorToolName } from "../cursor-native-tool-names.js";
import { isCursorReplayToolName } from "../cursor-tool-presentation-registry.js";
import { toolSelection, type NativeTools, type NativeToolPolicy, type NativeToolSelection } from "./context.js";

export function toolMetadata({ definition, sourceInfo }: RegisteredTool) {
	return { name: definition.name, description: definition.description, parameters: definition.parameters,
		label: definition.label, executionMode: definition.executionMode ?? "parallel",
		promptSnippet: definition.promptSnippet, promptGuidelines: definition.promptGuidelines, sourceInfo,
		exposure: definition.exposure ?? "direct" };
}
type HostTool = NativeTools[number] & { ownedBinding?: boolean };
export type ToolSnapshot = { tools: HostTool[]; active: string[]; policy: NativeToolPolicy; selection: NativeToolSelection };
type SelectionChange = { tools: string[]; state: null; active: string[] };
export function toolBinding(tool: RegisteredTool) {
	const { sourceInfo: _source, exposure: _exposure, ...metadata } = toolMetadata(tool);
	if (!isNativeCursorToolName(metadata.name)) throw new Error("Only retained Cursor native tools have host bindings");
	if ((tool.definition.exposure ?? "direct") !== "direct" || tool.definition.defaultActive === false) throw new Error("Unexpected retained Cursor tool activation policy");
	return { ...metadata, recordedCallPrefix: "cursor-replay-",
		recordedError: `No recorded Cursor ${metadata.name} result was available. This replay-only call does not execute work.`,
		binding: isCursorReplayToolName(metadata.name)
			? { kind: "recorded", error: "No recorded Cursor activity result was available. This replay-only tool does not execute work directly." }
			: { kind: "builtin", name: metadata.name, prepareArguments: typeof tool.definition.prepareArguments === "function" } };
}

/** Synchronous Pi getters see declarations immediately; enclosing calls await host commits. */
export class NativeToolSurface {
	registered: readonly RegisteredTool[] = [];
	capture(tools: HostTool[], active: string[], selection: NativeToolSelection,
		policy: NativeToolPolicy = { disabled: false, allow: null, deny: [] }): ToolSnapshot {
		return { tools, policy, selection, active: [...active] };
	}
	private permits(policy: NativeToolPolicy, name: string): boolean {
		return !policy.disabled && (policy.allow === null || policy.allow.includes(name)) && !policy.deny.includes(name) && (policy.ceiling === undefined || policy.ceiling.includes(name));
	}
	register(tools: readonly RegisteredTool[], snapshot?: ToolSnapshot) {
		const previous = new Map(this.registered.map(tool => [tool.definition.name, tool.definition]));
		this.registered = tools;
		if (!snapshot) return;
		const bindings = []; let changed = false;
		for (const tool of tools) {
			const name = tool.definition.name;
			if (!isNativeCursorToolName(name) || previous.get(name) === tool.definition) continue;
			changed = true;
			if (!this.permits(snapshot.policy, name)) continue;
			const index = snapshot.tools.findIndex(item => item.name === name);
			const existing = snapshot.tools[index];
			if (existing && existing.sourceInfo.source !== "builtin" && !existing.ownedBinding) throw new Error(`Tool ${name} belongs to another registration`);
			bindings.push(toolBinding(tool));
			const metadata = { ...toolMetadata(tool), ownedBinding: true };
			if (index < 0) {
				snapshot.tools.push(metadata);
				if (snapshot.policy.allow === null) snapshot.active = [...snapshot.active, name];
			}
			else snapshot.tools[index] = metadata;
		}
		if (snapshot.policy.allow !== null && changed) snapshot.active = [...new Set([...snapshot.active,
			...snapshot.tools.filter(tool => this.permits(snapshot.policy, tool.name)).map(tool => tool.name)])];
		return changed ? { bindings, active: snapshot.active } : undefined;
	}
	all(snapshot: ToolSnapshot): NativeTools {
		const all = new Map(snapshot.tools.map(tool => [tool.name, tool.ownedBinding
			? { ...tool, sourceInfo: { ...tool.sourceInfo, source: "builtin" } } : tool]));
		for (const tool of this.registered) {
			const existing = snapshot.tools.find(item => item.name === tool.definition.name);
			if (existing && this.permits(snapshot.policy, tool.definition.name) &&
				(!isNativeCursorToolName(tool.definition.name) || existing.ownedBinding)) all.set(tool.definition.name, toolMetadata(tool));
		}
		return [...all.values()];
	}
	activate(snapshot: ToolSnapshot, names: string[]): SelectionChange {
		const known = new Set(this.all(snapshot).map(tool => tool.name));
		snapshot.active = [...new Set(names.filter(name => known.has(name)))];
		return { tools: [...snapshot.active], active: snapshot.active, state: null };
	}
	acknowledge(snapshot: ToolSnapshot, change: { active: string[]; tools?: string[]; bindings?: ReturnType<typeof toolBinding>[] }, value: unknown): void {
		const receipt = value as { tools?: unknown; registered?: unknown; selection?: unknown } | null;
		if (!receipt || !Array.isArray(receipt.tools) || receipt.tools.some(name => typeof name !== "string") ||
			(change.tools && receipt.tools.some(name => !change.tools!.includes(name))) ||
			(change.bindings && JSON.stringify(receipt.registered) !== JSON.stringify(change.bindings.map(tool => tool.name)))) throw new Error("Invalid native tool acknowledgement");
		const selection = toolSelection(receipt.selection);
		if (selection.state !== null) throw new Error("Native tool acknowledgement does not match request");
		// Earlier acknowledgements must not overwrite a later synchronous selection.
		if (snapshot.active === change.active) {
			snapshot.active = [...receipt.tools];
			snapshot.selection = selection;
		}
	}
}
