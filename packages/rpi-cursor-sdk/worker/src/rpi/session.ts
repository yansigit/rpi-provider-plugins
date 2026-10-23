import { NativeJournals, type JournalRequest } from "./journal.js";
import { AsyncLocalStorage } from "node:async_hooks";
import { createHash } from "node:crypto";
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

// Receipts cross a JSON protocol boundary: object properties whose value is
// undefined disappear, while null and all other JSON values remain distinct.
function jsonValue(value: unknown): unknown {
	const inspect = (item: unknown, seen: Set<object>): void => {
		if (item === null || ["string", "boolean", "number", "undefined"].includes(typeof item)) return;
		if (typeof item !== "object" || seen.has(item)) throw new Error("Native session data is not JSON serializable");
		const proto = Object.getPrototypeOf(item);
		if (proto !== Object.prototype && proto !== Array.prototype && proto !== null) throw new Error("Native session data is not plain JSON");
		seen.add(item);
		for (const key of Reflect.ownKeys(item)) {
			if (typeof key !== "string") throw new Error("Native session data is not plain JSON");
			const descriptor = Object.getOwnPropertyDescriptor(item, key)!;
			if (!("value" in descriptor) || key === "toJSON") throw new Error("Native session data contains executable properties");
			inspect(descriptor.value, seen);
		}
		seen.delete(item);
	};
	inspect(value, new Set());
	const encoded = JSON.stringify(value);
	if (encoded === undefined) throw new Error("Native session data is not JSON serializable");
	return JSON.parse(encoded) as unknown;
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
	private references = 1;
	private ownerReleased = false;
	private releasing?: Promise<void>;
	readonly origin: string;
	readonly cwd: string;
	readonly sessionDir: string;
	readonly path: string | undefined;
	private request: SessionRequest;
	private value: View;
	private journalPort?: NativeJournals;
	private constructor(origin: string, cwd: string, sessionDir: string, path: string | undefined,
		request: SessionRequest, value: View, journalPort?: NativeJournals) {
		this.origin = origin; this.cwd = cwd; this.sessionDir = sessionDir; this.path = path;
		this.request = request; this.value = value; this.journalPort = journalPort;
	}
	static async capture(request: SessionRequest, cwd: string, sessionDir: string, journalRequest?: JournalRequest): Promise<NativeSessionOrigin> {
		const captured = object(await request({ action: "capture" }));
		const origin = string(captured.origin);
		try {
			const location = object(await request({ action: "location", origin }));
			const path = location.path === null ? undefined : string(location.path);
			const view = readView(await request({ action: "view", origin }));
			if (view.id !== object(object(captured.snapshot).metadata).id) throw new Error("Native session identity changed during capture");
			return new NativeSessionOrigin(origin, cwd, sessionDir, path, request, view,
				journalRequest && path ? new NativeJournals(input => journalRequest({ ...input, origin })) : undefined);
		} catch (error) {
			await request({ action: "release", origin }).catch(() => {});
			throw error;
		}
	}
	journals(): NativeJournals {
		this.view();
		if (!this.journalPort) throw new Error("Native durable journal capability is unavailable");
		return this.journalPort;
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
	async isCurrent(): Promise<boolean> {
		const previous = this.view();
		const snapshot = object(await this.request({ action: "snapshot", origin: this.origin }));
		if (string(object(snapshot.metadata).id) !== previous.id) throw new Error("Native session identity changed");
		const branch = array(snapshot.branches).map(object).find(branch => branch.lane === "main");
		return !!branch && integer(branch.revision) === previous.branchRevision && nullableId(branch.leafId) === previous.leafId;
	}
	async append(customType: string, data: unknown, journal = false): Promise<{ entry: SessionEntry; durability: "durable" | "ephemeral" }> {
		const before = this.view();
		const expectedData = data === undefined ? undefined : jsonValue(data);
		const receipt = object(await this.request({ action: journal ? "journalAppend" : "append", origin: this.origin,
		customType, ...(data === undefined ? {} : { data: jsonValue(expectedData) }) }));
		const committed = entry(receipt.entry);
		if (committed.type !== "custom" || committed.customType !== customType || !isDeepStrictEqual(committed.data, expectedData) || before.byId.has(committed.id) ||
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
	retain(): () => Promise<void> {
		this.view();
		this.references++;
		let released: Promise<void> | undefined;
		return () => released ??= this.dropReference();
	}
	private dropReference(): Promise<void> {
		if (--this.references > 0) return Promise.resolve();
		this.closed = true;
		return this.releasing = this.request({ action: "release", origin: this.origin }).then(() => {});
	}
	release(): Promise<void> {
		if (this.ownerReleased) return this.releasing ?? Promise.resolve();
		this.ownerReleased = true;
		return this.dropReference();
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
	getPersistentScopeKey(): string {
		const origin = this.origin();
		// A SQLite file contains many sessions; their persistent SDK stores must never share ownership.
		return `rpi:${createHash("sha256").update(JSON.stringify([origin.path, origin.view().id])).digest("hex")}`;
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

export function nativeSessionOrigin(manager: object): NativeSessionOrigin | undefined {
	return manager instanceof NativeSessionManager ? manager.origin() : undefined;
}
