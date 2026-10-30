import { randomUUID } from "node:crypto";
import type { WorkerCall } from "./peer.js";

type Input = Record<string, unknown>;

/**
 * Minimal host profile: a plain provider host (rpi-plugin-sdk 0.12) has no native
 * context, tool or session ports, so this worker answers them itself with one
 * in-memory, unpersisted session. No callback ever reaches the real host.
 */
export class LocalHost {
	/** The request model and prompt of the stream being served; snapshots describe it. */
	model: Input = { id: "auto", provider: "cursor", api: "cursor-sdk" };
	systemPrompt = "";
	private origins = new Set<string>();
	private next = 0;
	private active: string[] = [];
	private session = { metadata: { id: randomUUID(), createdAt: Date.now() }, revision: 0, name: null, labels: {},
		entries: [] as Input[], branches: [{ lane: "main", revision: 0, leafId: null as string | null }] };
	constructor(private cwd: string) {}
	bind(call: WorkerCall): WorkerCall {
		return { id: call.id, signal: call.signal, emit: event => call.emit(event), host: (method, input) => this.handle(method, input) };
	}
	async handle(method: string, raw: unknown): Promise<unknown> {
		const input = raw as Input;
		if (method === "scoped") {
			if (!this.origins.has(String(input.scope))) throw new Error("Unknown local Cursor session origin");
			return this.handle(String(input.method), input.input);
		}
		if (method !== "native") throw new Error(`Unsupported local Cursor host method ${method}`);
		switch (`${input.port}.${input.action}`) {
			case "context.flags": return {};
			case "context.snapshot": {
				// "print" keeps the adapter from registering replay tools: Cursor's own tool
				// activity arrives as transcript text, never as a tool call the host could run.
				const { baseUrl: _url, headers: _headers, ...model } = this.model;
				return { cwd: this.cwd, mode: "print", hasUI: false, projectTrusted: true, projectTrustResolved: true,
					isIdle: true, hasPendingMessages: false, systemPrompt: this.systemPrompt, sessionId: this.session.metadata.id,
					model, tools: [...this.active], toolRegistration: { disabled: false, allow: null, deny: [] },
					toolSelection: { revision: "0", state: null }, identity: null };
			}
			case "tools.inspect": return [];
			case "tools.publish": {
				const names = (input.tools as { name: string }[]).map(tool => tool.name);
				for (const name of names) if (!this.active.includes(name)) this.active.push(name);
				return { registered: names, tools: [...this.active], selection: { revision: "0", state: null } };
			}
			case "tools.setActive": this.active = [...input.tools as string[]]; return { tools: [...this.active], selection: { revision: "0", state: null } };
			case "auth.resolve": case "dialogs.show": return null;
			case "ui.notify": case "ui.status": case "context.abort": case "context.shutdown": case "context.compact": return null;
			case "sessions.capture": {
				const origin = `local-${++this.next}`; this.origins.add(origin);
				return { origin, snapshot: structuredClone(this.session) };
			}
		}
		if (input.port !== "sessions" || !this.origins.has(String(input.origin))) throw new Error(`Unsupported local Cursor host port ${input.port}.${input.action}`);
		const session = this.session, branch = session.branches[0];
		switch (input.action) {
			// No path: the retained adapter keeps usage and lifecycle records ephemeral.
			case "location": return { path: null };
			case "snapshot": return structuredClone(session);
			case "view": return { snapshot: structuredClone(session), leafId: branch.leafId, branchRevision: 0,
				projection: { model: null, thinkingLevel: "off", entries: [] } };
			case "sync": return { durability: "ephemeral" };
			case "release": this.origins.delete(String(input.origin)); return null;
			case "append": case "journalAppend": {
				const entry = { type: "custom", id: `local-entry-${++session.revision}`, seq: session.revision,
					parentId: branch.leafId, timestamp: Date.now(), customType: input.customType,
					...(Object.hasOwn(input, "data") ? { data: input.data } : {}) };
				session.entries.push(entry); branch.leafId = entry.id;
				// A long-lived process must not grow (and clone) an unbounded history; recent claims suffice.
				if (session.entries.length > 256) session.entries.shift();
				return { entry, revision: session.revision, durability: "ephemeral", advancedLane: true };
			}
		}
		throw new Error(`Unsupported local Cursor session action ${input.action}`);
	}
}
