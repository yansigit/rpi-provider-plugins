import type { Context } from "@earendil-works/pi-ai";
import { randomUUID } from "node:crypto";
import type { RegisteredTool } from "@earendil-works/pi-coding-agent";
import { isNativeCursorToolName } from "../cursor-native-tool-names.js";
import { toolSelection, type NativeTools, type NativeToolPolicy, type NativeToolSelection } from "./context.js";

export function toolMetadata({ definition, sourceInfo }: RegisteredTool): NativeTools[number] {
	const metadata = { name: definition.name, description: definition.description, parameters: definition.parameters,
		label: definition.label, executionMode: definition.executionMode ?? "parallel",
		promptSnippet: definition.promptSnippet, promptGuidelines: definition.promptGuidelines, sourceInfo,
		exposure: definition.exposure ?? "direct" };
	return metadata;
}
export type ToolSnapshot = { tools: NativeTools; active: string[]; policy: NativeToolPolicy; selection: NativeToolSelection };
type SelectionChange = { tools: string[]; state: { runtime: string; tools: string[] }; active: string[] };

/** Retained replay declarations stay inside the worker; executable tools remain host-owned. */
export class NativeToolSurface {
	registered: readonly RegisteredTool[] = [];
	constructor(private readonly runtime = randomUUID()) {}
	capture(tools: NativeTools, active: string[], selection: NativeToolSelection,
		policy: NativeToolPolicy = { disabled: false, allow: null, deny: [] }): ToolSnapshot {
		const state = selection.state as Partial<SelectionChange["state"]> | null;
		let ordered: string[] = [];
		if (state?.runtime === this.runtime) {
			if (!Array.isArray(state.tools) || state.tools.some(name => typeof name !== "string")) throw new Error("Invalid native replay selection");
			ordered = state.tools.filter(name => active.includes(name) || (!tools.some(tool => tool.name === name) &&
				isNativeCursorToolName(name) && this.permits(policy, name) && this.registered.some(tool => tool.definition.name === name)));
		}
		return { tools, policy, selection, active: [...new Set([...ordered, ...active])] };
	}
	private permits(policy: NativeToolPolicy, name: string): boolean {
		return !policy.disabled && (policy.allow === null || policy.allow.includes(name)) && !policy.deny.includes(name);
	}
	all(snapshot: ToolSnapshot): NativeTools {
		const all = new Map(snapshot.tools.map(tool => [tool.name, tool]));
		for (const tool of this.registered) {
			if (this.permits(snapshot.policy, tool.definition.name) && (all.has(tool.definition.name) || isNativeCursorToolName(tool.definition.name))) all.set(tool.definition.name, toolMetadata(tool));
		}
		return [...all.values()];
	}
	activate(snapshot: ToolSnapshot, names: string[]): SelectionChange {
		const known = new Set(this.all(snapshot).map(tool => tool.name));
		snapshot.active = [...new Set(names.filter(name => known.has(name)))];
		const hostNames = new Set(snapshot.tools.map(tool => tool.name));
		return { tools: snapshot.active.filter(name => hostNames.has(name)), active: snapshot.active,
			state: { runtime: this.runtime, tools: [...snapshot.active] } };
	}
	acknowledge(snapshot: ToolSnapshot, change: SelectionChange, value: unknown): void {
		const receipt = value as { tools?: unknown; selection?: unknown } | null;
		if (!receipt || !Array.isArray(receipt.tools) || receipt.tools.some(name => typeof name !== "string" || !change.tools.includes(name))) throw new Error("Invalid native tool selection acknowledgement");
		const selection = toolSelection(receipt.selection);
		const state = selection.state as Partial<SelectionChange["state"]> | null;
		if (state?.runtime !== this.runtime || JSON.stringify(state.tools) !== JSON.stringify(change.state.tools)) throw new Error("Native tool selection acknowledgement does not match request");
		// Earlier acknowledgements must not overwrite a later synchronous selection.
		if (snapshot.active === change.active) {
			snapshot.active = this.capture(snapshot.tools, receipt.tools, selection, snapshot.policy).active;
			snapshot.selection = selection;
		}
	}
	context(snapshot: ToolSnapshot, context: Context): Context {
		if (context.tools === undefined) return context;
		const tools = new Map(context.tools.map(tool => [tool.name, tool]));
		for (const { definition } of this.registered) {
			if (this.permits(snapshot.policy, definition.name) && isNativeCursorToolName(definition.name) && snapshot.active.includes(definition.name)) {
				tools.set(definition.name, { name: definition.name, description: definition.description, parameters: definition.parameters });
			}
		}
		return { ...context, tools: [...tools.values()] };
	}
}
