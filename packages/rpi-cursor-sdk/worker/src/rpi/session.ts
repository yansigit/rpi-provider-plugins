import { AsyncLocalStorage } from "node:async_hooks";
import { dirname } from "node:path";
import { isDeepStrictEqual } from "node:util";
import type { AgentMessage } from "@earendil-works/pi-agent-core";
import type { ExtensionContext, SessionEntry, SessionHeader, SessionProjection, SessionTreeNode } from "@earendil-works/pi-coding-agent";

type ReadonlySessionManager = ExtensionContext["sessionManager"];

export type SessionRequest = (request: Record<string, unknown>) => Promise<unknown>;
type ObjectValue = Record<string, unknown>;
type View = {
	id: string; createdAt: number; revision: number; branchRevision: number; leafId: string | null;
	entries: SessionEntry[]; byId: Map<string, SessionEntry>; labels: Map<string, string>; name?: string;
	projection: SessionProjection;
};

function object(value: unknown): ObjectValue {
	if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("Invalid native session object");
	return value as ObjectValue;
}
function string(value: unknown): string {
	if (typeof value !== "string" || !value) throw new Error("Invalid native session identifier");
	return value;
}
function integer(value: unknown): number {
	if (!Number.isSafeInteger(value) || (value as number) < 0) throw new Error("Invalid native session revision");
	return value as number;
}
function array(value: unknown): unknown[] {
	if (!Array.isArray(value)) throw new Error("Invalid native session array");
	return value;
}
function nullableId(value: unknown): string | null { return value === null ? null : string(value); }
function timestamp(value: unknown): string {
	if (!Number.isSafeInteger(value)) throw new Error("Invalid native session timestamp");
	return new Date(value as number).toISOString();
}

function message(value: unknown): AgentMessage {
	const raw = object(value);
	const { kind, ...fields } = raw;
	const role = string(raw.role);
	timestamp(raw.timestamp);
	if (kind === "custom") {
		array(raw.content);
		delete fields.data;
		return { ...object(raw.data), ...fields, role } as AgentMessage;
	}
	if (!((kind === "user" && role === "user") || (kind === "assistant" && role === "assistant") ||
		(kind === "tool_result" && role === "toolResult"))) throw new Error("Invalid native session message");
	if (kind !== "user" || typeof raw.content !== "string") array(raw.content);
	return { ...fields, role } as AgentMessage;
}

function entry(value: unknown): SessionEntry {
	const raw = object(value);
	const type = string(raw.type);
	const base: ObjectValue & { type: string; id: string; parentId: string | null; timestamp: string } = {
		...raw, type, id: string(raw.id), parentId: nullableId(raw.parentId), timestamp: timestamp(raw.timestamp),
	};
	integer(raw.seq);
	switch (type) {
		case "message": return { ...base, type, message: message(raw.message) };
		case "custom": string(raw.customType); break;
		case "model_change": string(raw.provider); string(raw.modelId); break;
		case "thinking_level_change": string(raw.thinkingLevel); break;
		case "active_tools_change": array(raw.activeToolNames).forEach(string); break;
		case "branch_summary": string(raw.fromId); if (typeof raw.summary !== "string") throw new Error("Invalid native summary"); break;
		case "compaction":
			if (typeof raw.summary !== "string") throw new Error("Invalid native summary");
			integer(raw.tokensBefore);
			base.retainedTail = array(raw.retainedTail).map(message);
			break;
		default: throw new Error("Unsupported native session entry");
	}
	// Native compactions own their retained tail; the host projection supplies it without fabricated Pi entry IDs.
	return base as SessionEntry;
}

function readView(value: unknown): View {
	const raw = object(value), snapshot = object(raw.snapshot), metadata = object(snapshot.metadata);
	const id = string(metadata.id), createdAt = Number(metadata.createdAt);
	timestamp(metadata.createdAt);
	const entries = array(snapshot.entries).map(entry);
	const byId = new Map(entries.map(item => [item.id, item]));
	if (byId.size !== entries.length) throw new Error("Duplicate native session entry");
	const seen = new Set<string>();
	let lastSequence = 0;
	for (const item of entries) {
		const sequence = integer(object(item).seq);
		if (sequence <= lastSequence || (item.parentId && byId.has(item.parentId) && !seen.has(item.parentId))) {
			throw new Error("Invalid native session tree order");
		}
		lastSequence = sequence; seen.add(item.id);
	}
	const leafId = nullableId(raw.leafId);
	if (leafId !== null && !byId.has(leafId)) throw new Error("Missing native session leaf");
	const projection = object(raw.projection);
	const projected = array(projection.entries).map(value => {
		const item = object(value);
		return { sourceEntry: entry(item.sourceEntry), messages: array(item.messages).map(message) };
	});
	const model = projection.model === null ? null : object(projection.model);
	const labels = new Map(Object.entries(object(snapshot.labels)).map(([id, label]) => [id, string(label)]));
	if (snapshot.name !== null && typeof snapshot.name !== "string") throw new Error("Invalid native session name");
	return { id, createdAt, revision: integer(snapshot.revision), branchRevision: integer(raw.branchRevision),
		leafId, entries, byId, labels, name: snapshot.name ?? undefined,
		projection: { entries: projected, messages: projected.flatMap(item => item.messages),
			thinkingLevel: string(projection.thinkingLevel),
			model: model && { provider: string(model.provider), modelId: string(model.modelId) } },
	};
}

/** One captured host origin. Every mutation waits for a committed receipt before reading a new view. */
export class NativeSessionOrigin {
	private closed = false;
	private constructor(readonly origin: string, readonly cwd: string, readonly sessionDir: string,
		readonly path: string | undefined, private request: SessionRequest, private value: View) {}
	static async capture(request: SessionRequest, cwd: string, sessionDir: string): Promise<NativeSessionOrigin> {
		const captured = object(await request({ action: "capture" }));
		const origin = string(captured.origin);
		try {
			const location = object(await request({ action: "location", origin }));
			const path = location.path === null ? undefined : string(location.path);
			const view = readView(await request({ action: "view", origin }));
			if (view.id !== object(object(captured.snapshot).metadata).id) throw new Error("Native session identity changed during capture");
			return new NativeSessionOrigin(origin, cwd, sessionDir, path, request, view);
		} catch (error) {
			await request({ action: "release", origin }).catch(() => {});
			throw error;
		}
	}
	view(): View {
		if (this.closed) throw new Error("Native session origin is closed");
		return this.value;
	}
	async refresh(): Promise<void> {
		const previous = this.view();
		const next = readView(await this.request({ action: "view", origin: this.origin }));
		if (next.id !== previous.id || next.branchRevision !== previous.branchRevision || next.revision < previous.revision) {
			throw new Error("Native session view changed ownership or regressed");
		}
		// Concurrent refreshes cannot regress a view already acknowledged by this origin.
		if (next.revision >= this.value.revision) this.value = next;
	}
	async append(customType: string, data: unknown, journal = false): Promise<{ entry: SessionEntry; durability: "durable" | "ephemeral" }> {
		const before = this.view();
		const receipt = object(await this.request({ action: journal ? "journalAppend" : "append", origin: this.origin,
			customType, ...(data === undefined ? {} : { data }) }));
		const committed = entry(receipt.entry);
		if (committed.type !== "custom" || committed.customType !== customType || !isDeepStrictEqual(committed.data, data) || before.byId.has(committed.id) ||
			integer(receipt.revision) <= before.revision || typeof receipt.advancedLane !== "boolean" ||
			(!journal && receipt.advancedLane !== true) || !["durable", "ephemeral"].includes(String(receipt.durability))) {
			throw new Error("Invalid native session commit receipt; inspect the journal before retrying");
		}
		await this.refresh();
		if (!isDeepStrictEqual(this.value.byId.get(committed.id), committed) || this.value.revision < (receipt.revision as number)) {
			throw new Error("Native session commit is missing from its authoritative view; inspect the journal before retrying");
		}
		return { entry: structuredClone(committed), durability: receipt.durability as "durable" | "ephemeral" };
	}
	async sync(): Promise<"durable" | "ephemeral"> {
		this.view();
		const response = object(await this.request({ action: "sync", origin: this.origin }));
		if (response.durability !== "durable" && response.durability !== "ephemeral") throw new Error("Invalid native durability acknowledgement");
		return response.durability;
	}
	async release(): Promise<void> {
		if (this.closed) return;
		this.closed = true;
		await this.request({ action: "release", origin: this.origin });
	}
}

/** Stable adapter identity with request-local views; late work cannot select a different branch. */
export class NativeSessionManager implements ReadonlySessionManager {
	private current = new AsyncLocalStorage<NativeSessionOrigin>();
	private selected: NativeSessionOrigin | undefined;
	select(origin: NativeSessionOrigin): void { origin.view(); this.selected = origin; }
	run<T>(origin: NativeSessionOrigin, operation: () => T): T { origin.view(); return this.current.run(origin, operation); }
	origin(): NativeSessionOrigin {
		const origin = this.current.getStore() ?? this.selected;
		if (!origin) throw new Error("Native session is unavailable");
		origin.view();
		return origin;
	}
	getCwd(): string { return this.origin().cwd; }
	getSessionDir(): string { return this.getSessionFile() ? dirname(this.getSessionFile()!) : this.origin().sessionDir; }
	getSessionId(): string { return this.origin().view().id; }
	getSessionFile(): string | undefined { return this.origin().path; }
	getLeafId(): string | null { return this.origin().view().leafId; }
	getLeafEntry(): SessionEntry | undefined { const id = this.getLeafId(); return id ? this.getEntry(id) : undefined; }
	getEntry(id: string): SessionEntry | undefined { return structuredClone(this.origin().view().byId.get(id)); }
	getLabel(id: string): string | undefined { return this.origin().view().labels.get(id); }
	getSessionName(): string | undefined { return this.origin().view().name; }
	getEntries(): SessionEntry[] { return structuredClone(this.origin().view().entries); }
	getHeader(): SessionHeader { const view = this.origin().view(); return { type: "session", version: 3, id: view.id,
		timestamp: new Date(view.createdAt).toISOString(), cwd: this.getCwd() }; }
	getBranch(fromId?: string): SessionEntry[] {
		const view = this.origin().view(), branch: SessionEntry[] = [], seen = new Set<string>();
		let id: string | null = fromId ?? view.leafId;
		while (id !== null) {
			if (seen.has(id)) throw new Error("Native session branch contains a cycle");
			seen.add(id);
			const item = view.byId.get(id);
			if (!item) break;
			branch.push(item); id = item.parentId;
		}
		return structuredClone(branch.reverse());
	}
	buildContextEntries(): SessionEntry[] { return structuredClone(this.origin().view().projection.entries.map(item => item.sourceEntry)); }
	buildSessionProjection(): SessionProjection { return structuredClone(this.origin().view().projection); }
	getTree(): SessionTreeNode[] {
		const entries = this.getEntries(), nodes = new Map<string, SessionTreeNode>(), roots: SessionTreeNode[] = [];
		for (const item of entries) nodes.set(item.id, { entry: item, children: [], label: this.getLabel(item.id) });
		for (const item of entries) {
			const node = nodes.get(item.id)!;
			const parent = item.parentId && nodes.get(item.parentId);
			if (parent && parent !== node) parent.children.push(node); else roots.push(node);
		}
		return roots;
	}
}
