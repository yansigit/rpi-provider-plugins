import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { __testUtils as cleanup, runCursorSessionAgentCleanupCommand } from "../../src/cursor-session-agent-cleanup.js";
import { CURSOR_SESSION_AGENT_RESUME_ENTRY_TYPE } from "../../src/cursor-session-agent-resume.js";
import { cursorSessionScopeKeyForManager, registerCursorSessionScope, getCursorSessionScopeSnapshot,
	__testUtils as scopes } from "../../src/cursor-session-scope.js";
import { createEventHarness } from "../helpers/pi-harness.js";
import { installCursorSessionStoreMock } from "../helpers/cursor-session-store.js";
import { nativeSessionFixture } from "../helpers/native-session.js";

beforeEach(() => { installCursorSessionStoreMock(); cleanup.reset(); scopes.reset(); });
afterEach(() => { cleanup.reset(); scopes.reset(); });

async function seeded() {
	const session = await nativeSessionFixture();
	for (const agentId of ["agent-old", "agent-active"]) {
		await session.origin.append(CURSOR_SESSION_AGENT_RESUME_ENTRY_TYPE, {
			version: 1, runtime: "local", agentId, scopeKey: cursorSessionScopeKeyForManager(session.manager),
			sessionFile: session.manager.getSessionFile(), sessionId: session.manager.getSessionId(), cwd: "/tmp/project",
			poolKey: "pool", branchPathHash: "hash", compactionGeneration: 0,
			sendState: { bootstrapped: true, contextFingerprint: "fp", incrementalSendCount: 0 },
			createdAt: "2026-07-08T00:00:00.000Z",
			...(agentId === "agent-active" ? { cleanupCandidateAgentIds: ["agent-old"] } : {}),
		});
	}
	return { ...session, ctx: { cwd: "/tmp/project", sessionManager: session.manager, ui: { notify: vi.fn() } } };
}

it("keeps persistent native stores distinct for sessions in the same SQLite file", async () => {
	const a = await nativeSessionFixture("a"), b = await nativeSessionFixture("b"), reopened = await nativeSessionFixture("a");
	expect(a.manager.getSessionFile()).toBe(b.manager.getSessionFile());
	expect(cursorSessionScopeKeyForManager(a.manager)).not.toBe(cursorSessionScopeKeyForManager(b.manager));
	expect(cursorSessionScopeKeyForManager(a.manager)).toBe(cursorSessionScopeKeyForManager(reopened.manager));
	const pi = createEventHarness();
	registerCursorSessionScope(pi);
	await pi.runSessionStart({ cwd: "/tmp/project", sessionManager: a.manager });
	expect(getCursorSessionScopeSnapshot(pi).persistentScopeKey).toBe(a.manager.getPersistentScopeKey());
});

it("awaits host durability, deletes once, and records a late result on its original branch", async () => {
	const s = await seeded(), appendEntry = vi.fn(() => { throw new Error("native paths must not use the Pi file writer"); });
	let acknowledge!: () => void;
	const original = s.request.getMockImplementation()!;
	s.request.mockImplementation(async input => {
		if (input.action === "append" && (input.data as { phase?: string })?.phase === "intent") {
			await new Promise<void>(resolve => { acknowledge = resolve; });
		}
		return original(input);
	});
	const deletion = vi.fn(async () => { await s.branch(null); });
	cleanup.setSdkOperations({ delete: deletion });
	const pending = runCursorSessionAgentCleanupCommand({ appendEntry }, "--yes", s.ctx);
	await Promise.resolve();
	expect(deletion).not.toHaveBeenCalled();
	acknowledge(); await pending;
	expect(deletion).toHaveBeenCalledTimes(1);
	expect(deletion.mock.calls[0]).toEqual(["agent-old", expect.any(Object)]);
	expect(appendEntry).not.toHaveBeenCalled();
	expect(s.manager.getBranch()).toEqual([]);
	const data = s.entries.filter(entry => entry.customType === "cursor-sdk-agent-cleanup").map(entry => entry.data);
	expect(data).toMatchObject([{ phase: "intent" }, { phase: "result", deletedAgentIds: ["agent-old"] }]);
	expect(s.request.mock.calls.some(([input]) => input.action === "journalAppend")).toBe(true);
});

it("does not delete after a failed native commit and does not sync SQLite with Node fsync", async () => {
	const s = await seeded(), deletion = vi.fn();
	cleanup.setSdkOperations({ delete: deletion });
	s.request.mockRejectedValueOnce(new Error("transaction synchronization failed"));
	await runCursorSessionAgentCleanupCommand({ appendEntry: vi.fn() }, "--yes", s.ctx);
	expect(deletion).not.toHaveBeenCalled();
	expect(s.ctx.ui.notify).toHaveBeenCalledWith("Cleanup intent could not be durably recorded. No agents were deleted.", "error");
});
