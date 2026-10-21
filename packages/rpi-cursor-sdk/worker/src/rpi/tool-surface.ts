import type { Context } from "@earendil-works/pi-ai";
import type { RegisteredTool } from "@earendil-works/pi-coding-agent";
import { isNativeCursorToolName } from "../cursor-native-tool-names.js";
import type { NativeTools } from "./context.js";

export function toolMetadata({ definition, sourceInfo }: RegisteredTool): NativeTools[number] {
	const metadata = { name: definition.name, description: definition.description, parameters: definition.parameters,
		label: definition.label, executionMode: definition.executionMode ?? "parallel",
		promptSnippet: definition.promptSnippet, promptGuidelines: definition.promptGuidelines, sourceInfo,
		exposure: definition.exposure ?? "direct" };
	return metadata;
}
export type ToolSnapshot = { tools: NativeTools; active: string[] };

/** Retained replay declarations stay inside the worker; executable tools remain host-owned. */
export class NativeToolSurface {
	registered: readonly RegisteredTool[] = [];
	private hostActive?: string[];
	private replayActive: string[] = [];
	capture(tools: NativeTools, active: string[]): ToolSnapshot {
		if (this.hostActive && (this.hostActive.length !== active.length || this.hostActive.some(name => !active.includes(name)))) this.replayActive = [];
		this.hostActive = [...active];
		return { tools, active: [...new Set([...active, ...this.replayActive])] };
	}
	all(snapshot: ToolSnapshot): NativeTools {
		const all = new Map(snapshot.tools.map(tool => [tool.name, tool]));
		for (const tool of this.registered) {
			if (all.has(tool.definition.name) || isNativeCursorToolName(tool.definition.name)) all.set(tool.definition.name, toolMetadata(tool));
		}
		return [...all.values()];
	}
	activate(snapshot: ToolSnapshot, names: string[]): string[] {
		const known = new Set(this.all(snapshot).map(tool => tool.name));
		snapshot.active = [...new Set(names.filter(name => known.has(name)))];
		const hostNames = new Set(snapshot.tools.map(tool => tool.name));
		this.hostActive = snapshot.active.filter(name => hostNames.has(name));
		this.replayActive = snapshot.active.filter(name => !hostNames.has(name) && isNativeCursorToolName(name));
		return [...this.hostActive];
	}
	context(snapshot: ToolSnapshot, context: Context): Context {
		if (context.tools === undefined) return context;
		const tools = new Map(context.tools.map(tool => [tool.name, tool]));
		for (const { definition } of this.registered) {
			if (isNativeCursorToolName(definition.name) && snapshot.active.includes(definition.name)) {
				tools.set(definition.name, { name: definition.name, description: definition.description, parameters: definition.parameters });
			}
		}
		return { ...context, tools: [...tools.values()] };
	}
}
