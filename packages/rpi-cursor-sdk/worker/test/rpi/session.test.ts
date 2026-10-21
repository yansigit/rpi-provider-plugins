import { expect, it, vi } from "vitest";
import { NativeSessionManager, NativeSessionOrigin, type SessionRequest } from "../../src/rpi/session.js";

const now = 1_729_000_000_123;
const user = (content: string) => ({ kind: "user", role: "user", content, timestamp: now });
const custom = (id: string, seq: number, parentId: string | null, data?: unknown) => ({
	type: "custom", id, seq, parentId, timestamp: now, customType: "test:entry", ...(data === undefined ? {} : { data }),
});
function fixture(id = "session") {
	const entries = [
		{ type: "message", id: "old", seq: 1, parentId: null, timestamp: now, message: user("summarized") },
		{ type: "compaction", id: "compact", seq: 2, parentId: "old", timestamp: now, summary: "summary",
			tokensBefore: 100, retainedTail: [user("retained")] },
		custom("anchor", 3, "compact"),
	];
	return { snapshot: { metadata: { id, createdAt: now }, revision: 3, name: "native", labels: { compact: "checkpoint" },
		branches: [{ lane: "main", revision: 0, leafId: "anchor" }], entries }, leafId: "anchor", branchRevision: 0,
		projection: { model: null, thinkingLevel: "off", entries: [
			{ sourceEntry: entries[1], messages: [{ kind: "custom", role: "compactionSummary", content: [], timestamp: now,
				data: { summary: "summary", tokensBefore: 100 } }, user("retained")] },
			{ sourceEntry: entries[2], messages: [] },
		] },
	};
}
function host(id = "session") {
	let view = fixture(id);
	const request = vi.fn<SessionRequest>(async input => {
		switch (input.action) {
			case "capture": return { origin: `origin-${id}`, snapshot: structuredClone(view.snapshot) };
			case "location": return { path: "/isolated/sessions.db" };
			case "view": return structuredClone(view);
			case "sync": return { durability: "durable" };
			case "release": return null;
			default: throw new Error("unexpected request");
		}
	});
	return { request, get view() { return view; }, set view(next) { view = next; } };
}
const capture = (request: SessionRequest) => NativeSessionOrigin.capture(request, "/workspace", "/isolated");

it("projects authoritative compaction messages with real source IDs and defensive getters", async () => {
	const h = host(), origin = await capture(h.request), manager = new NativeSessionManager();
	manager.select(origin);
	expect(manager.getHeader()).toMatchObject({ id: "session", timestamp: new Date(now).toISOString() });
	expect(manager.getEntries()[0].timestamp).toBe(new Date(now).toISOString());
	expect(manager.getBranch().map(item => item.id)).toEqual(["old", "compact", "anchor"]);
	expect(manager.getBranch("compact").map(item => item.id)).toEqual(["old", "compact"]);
	expect(manager.buildContextEntries().map(item => item.id)).toEqual(["compact", "anchor"]);
	const projection = manager.buildSessionProjection();
	expect(projection.messages).toEqual([
		{ role: "compactionSummary", content: [], timestamp: now, summary: "summary", tokensBefore: 100 },
		{ role: "user", content: "retained", timestamp: now },
	]);
	expect(projection.entries[0].sourceEntry).not.toHaveProperty("firstKeptEntryId");
	projection.messages.length = 0;
	expect(manager.buildSessionProjection().messages).toHaveLength(2);
	const tree = manager.getTree();
	expect(tree[0].children[0].label).toBe("checkpoint");
	tree[0].children[0].entry.id = "mutated";
	expect(manager.getEntry("compact")?.id).toBe("compact");
	expect(await origin.sync()).toBe("durable");
	await origin.release();
	expect(() => manager.getSessionId()).toThrow("closed");
});

it("awaits the committed receipt and refreshes from host state without reserving an ID", async () => {
	const h = host(), origin = await capture(h.request), manager = new NativeSessionManager();
	manager.select(origin);
	let acknowledge!: () => void;
	const committed = custom("host-issued", 4, "anchor", null);
	h.request.mockImplementationOnce(async input => {
		expect(input).toEqual({ action: "append", origin: "origin-session", customType: "test:entry", data: null });
		await new Promise<void>(resolve => { acknowledge = resolve; });
		h.view.snapshot.entries.push(committed);
		h.view.snapshot.revision = 4;
		h.view.leafId = committed.id;
		return { entry: committed, revision: 4, advancedLane: true, durability: "durable" };
	});
	const pending = origin.append("test:entry", null);
	await Promise.resolve();
	expect(manager.getLeafId()).toBe("anchor");
	expect(manager.getEntry(committed.id)).toBeUndefined();
	acknowledge();
	expect(await pending).toMatchObject({ entry: { id: "host-issued", data: null }, durability: "durable" });
	expect(manager.getLeafId()).toBe("host-issued");
	h.request.mockRejectedValueOnce(new Error("disk full"));
	await expect(origin.append("test:entry", undefined)).rejects.toThrow("disk full");
	expect(h.request.mock.lastCall?.[0]).not.toHaveProperty("data");
	expect(manager.getLeafId()).toBe("host-issued");
});

it("keeps delayed work on its captured view while the stable manager selects another session", async () => {
	const a = await capture(host("a").request), b = await capture(host("b").request), manager = new NativeSessionManager();
	manager.select(a);
	let resume!: () => void;
	const pending = manager.run(a, async () => {
		await new Promise<void>(resolve => { resume = resolve; });
		expect(manager.getSessionId()).toBe("a");
		await a.sync();
		expect(manager.getSessionId()).toBe("a");
	});
	manager.select(b);
	expect(manager.getSessionId()).toBe("b");
	resume(); await pending;
	expect(manager.getSessionId()).toBe("b");
});

it("rejects duplicate/cyclic trees, imprecise revisions and reversed session identities", async () => {
	for (const damage of [
		(h: ReturnType<typeof host>) => { h.view.snapshot.entries.push(h.view.snapshot.entries[0]); },
		(h: ReturnType<typeof host>) => { h.view.snapshot.entries[0].parentId = "compact"; },
		(h: ReturnType<typeof host>) => { h.view.snapshot.revision = Number.MAX_SAFE_INTEGER + 1; },
	]) {
		const h = host(); damage(h);
		await expect(capture(h.request)).rejects.toThrow();
		expect(h.request.mock.lastCall?.[0]).toEqual({ action: "release", origin: "origin-session" });
	}
	const h = host(), origin = await capture(h.request);
	h.view.snapshot.metadata.id = "foreign";
	await expect(origin.refresh()).rejects.toThrow("ownership");
	expect(origin.view().id).toBe("session");
});

it("rejects a claimed commit that is absent from the authoritative tree", async () => {
	const h = host(), origin = await capture(h.request);
	h.request.mockResolvedValueOnce({ entry: custom("reserved", 4, "anchor"), revision: 4,
		advancedLane: true, durability: "durable" });
	await expect(origin.append("test:entry", undefined)).rejects.toThrow("missing from its authoritative view");
	expect(origin.view().leafId).toBe("anchor");
});
