import { vi } from "vitest";
import type { JournalRequest } from "../../src/rpi/journal.js";
import { NativeSessionManager, NativeSessionOrigin, type SessionRequest } from "../../src/rpi/session.js";

/** Host-issued IDs and origin-bound branches, without using Node session-file writes. */
export async function nativeSessionFixture(id = "native-session", path: string | null = "/isolated/sessions.sqlite", journalRequest?: JournalRequest) {
	const entries: Record<string, unknown>[] = [];
	let revision = 0, branchRevision = 0, activeLeaf: string | null = null;
	const captures = new Map<string, { revision: number; anchor: string | null }>();
	const now = 1_729_000_000_123;
	const snapshot = () => ({ metadata: { id, createdAt: now }, revision, name: null, labels: {}, entries, branches: [{ lane: "main", revision: branchRevision, leafId: activeLeaf }] });
	const request = vi.fn<SessionRequest>(async input => {
		if (input.action === "capture") {
			const origin = `origin-${id}-${captures.size}`;
			captures.set(origin, { revision: branchRevision, anchor: activeLeaf });
			return { origin, snapshot: structuredClone(snapshot()) };
		}
		const captured = captures.get(String(input.origin));
		if (!captured) throw new Error("invalid origin");
		switch (input.action) {
			case "location": return { path };
			case "snapshot": return structuredClone(snapshot());
			case "view": return structuredClone({ snapshot: snapshot(),
				leafId: captured.revision === branchRevision ? activeLeaf : captured.anchor,
				branchRevision: captured.revision, projection: { entries: [], thinkingLevel: "off", model: null } });
			case "append": case "journalAppend": {
				const advancedLane = captured.revision === branchRevision;
				if (!advancedLane && input.action === "append") throw new Error("branch changed");
				const entry = { type: "custom", id: `committed-${++revision}`, seq: revision, timestamp: now,
					parentId: advancedLane ? activeLeaf : captured.anchor, customType: input.customType,
					...(Object.hasOwn(input, "data") ? { data: input.data } : {}) };
				entries.push(entry); captured.anchor = entry.id;
				if (advancedLane) activeLeaf = entry.id;
				return structuredClone({ entry, revision, advancedLane, durability: path ? "durable" : "ephemeral" });
			}
			case "sync": return { durability: path ? "durable" : "ephemeral" };
			case "release": captures.delete(String(input.origin)); return null;
			default: throw new Error("unexpected session operation");
		}
	});
	const capture = () => NativeSessionOrigin.capture(request, "/tmp/project", "/isolated", journalRequest);
	const origin = await capture(), manager = new NativeSessionManager();
	manager.select(origin);
	return { request, origin, manager, capture, entries,
		async branch(leaf: string | null) { activeLeaf = leaf; branchRevision++; const next = await capture(); manager.select(next); return next; },
		get activeLeaf() { return activeLeaf; },
	};
}
