import type { SDKAgent } from "@cursor/sdk";
import { afterEach, expect, it, vi } from "vitest";
import { captureCursorUsageRecorder, readCursorUsageView } from "../../src/cursor-usage-ledger.js";
import { captureCursorCloudLifecycleRecorder, registerCursorCloudLifecycleLedger, runCursorCloudLifecycleCommand,
	__testUtils as cloud } from "../../src/cursor-cloud-lifecycle.js";
import { createExtensionTestContext, createPiHarness, makeModel } from "../helpers/pi-harness.js";
import { nativeJournalFixture } from "../helpers/native-journals.js";
import { nativeSessionFixture } from "../helpers/native-session.js";
import { NativeSessionWork, trackNativeSessionWork } from "../../src/rpi/session-work.js";

const agentId = "bc-00000000-0000-0000-0000-000000000001";
afterEach(() => cloud.reset());
async function fixture(path: string | null = "/isolated/sessions.sqlite") {
	const journals = nativeJournalFixture();
	const session = await nativeSessionFixture("native", path, journals.request);
	const pi = createPiHarness();
	pi.appendEntry.mockImplementation(() => { throw new Error("native ledger must use its captured host origin"); });
	const ctx = { ...createExtensionTestContext(), cwd: "/tmp/project", sessionManager: session.manager };
	return { journals, session, pi, ctx };
}

it("claims usage origins over JSON receipts for persistent and fileless native sessions", async () => {
	for (const path of ["/isolated/sessions.sqlite", null]) {
		const { session, pi, ctx } = await fixture(path);
		const turn = await captureCursorUsageRecorder(pi, ctx).start({ agent: usageAgent(), runtime: "local", model: makeModel(),
			modelSelection: { id: "test" }, purpose: "normal", resumed: false, newlyCreated: true });
		expect(session.entries[0]).toMatchObject({ customType: "pi-cursor-sdk:usage-origin-v1" });
		await turn.recordTerminal({ status: "abandon" });
		await session.origin.release();
	}
});
function usageAgent() {
	const getUsage: SDKAgent["getUsage"] = async () => ({
		usage: { inputTokens: 1, outputTokens: 2, cacheReadTokens: 0, cacheWriteTokens: 0, totalTokens: 3 }, runs: [],
	});
	return { agentId: "agent-fixture", getUsage } as SDKAgent;
}

it("records usage only after its durable native claim and journal acknowledgement", async () => {
	const { journals, session, pi, ctx } = await fixture();
	const input = { agent: usageAgent(), runtime: "local" as const, model: makeModel(), modelSelection: { id: "test" },
		purpose: "normal" as const, resumed: false, newlyCreated: true };
	let acknowledge!: () => void, started!: () => void, admitted = false;
	const writing = new Promise<void>(resolve => { started = resolve; });
	const original = journals.request.getMockImplementation()!;
	journals.request.mockImplementation(async input => {
		if (input.action === "append") { started(); await new Promise<void>(resolve => { acknowledge = resolve; }); }
		return original(input);
	});
	const pending = captureCursorUsageRecorder(pi, ctx).start(input).then(turn => { admitted = true; return turn; });
	await writing;
	expect(admitted).toBe(false);
	expect(session.entries.map(entry => entry.customType)).toEqual(["pi-cursor-sdk:usage-origin-v1"]);
	acknowledge(); const turn = await pending;
	journals.request.mockImplementation(original);
	await session.branch(null);
	await turn.recordTerminal({ status: "abandon" });
	expect(session.manager.getBranch()).toEqual([]);
	const view = await readCursorUsageView(pi, ctx);
	expect(view.records).toEqual([]);
	expect(view.otherBranchRecords.map(record => record.kind)).toEqual(["start", "terminal"]);
	expect(view.unclaimedRecords).toEqual([]);
	expect(pi.appendEntry).not.toHaveBeenCalled();
});

it("refuses a new usage start at a torn journal without discarding its prefix or tail", async () => {
	const { journals, pi, ctx } = await fixture();
	const bytes = Buffer.from('{"torn":'); journals.contents.set("usage", bytes);
	await expect(captureCursorUsageRecorder(pi, ctx).start({ agent: usageAgent(), runtime: "local", model: makeModel(),
		modelSelection: { id: "test" }, purpose: "normal", resumed: false, newlyCreated: true })).rejects.toThrow("incomplete journal frame");
	expect(journals.contents.get("usage")).toEqual(bytes);
	expect(journals.request.mock.calls.some(([input]) => input.action === "append")).toBe(false);
});

async function recordedCloud() {
	const f = await fixture();
	registerCursorCloudLifecycleLedger(f.pi);
	await f.pi.runSessionStart(f.ctx);
	const recorder = captureCursorCloudLifecycleRecorder(f.pi);
	expect(await recorder({ agentId, runId: "run-1" }, undefined)).toBe(true);
	cloud.setRuntimeApiKeyResolver(async () => "test-key");
	return f;
}

it("releases replaced cloud bindings while a late recorded result retains its captured origin", async () => {
	const { journals, session, pi, ctx } = await fixture();
	registerCursorCloudLifecycleLedger(pi);
	await pi.runSessionStart(ctx);
	const work = new NativeSessionWork(session.origin);
	let finish!: () => void;
	const completion = new Promise<void>(resolve => { finish = resolve; });
	await work.run(async () => {
		const recorder = captureCursorCloudLifecycleRecorder(pi);
		trackNativeSessionWork(completion.then(async () => {
			expect(await recorder({ agentId, runId: "late-run" }, undefined)).toBe(true);
		}));
	});
	await session.origin.release();
	const next = await session.branch(null);
	await pi.runBeforeAgentStart(ctx);
	await pi.runSessionShutdown({}, ctx);
	await next.release();
	expect(() => next.view()).toThrow("closed");
	expect(session.origin.view().id).toBe("native");
	finish(); await work.settled;
	expect(() => session.origin.view()).toThrow("closed");
	expect(session.activeLeaf).toBeNull();
	const rows = journals.contents.get("cloud")!.toString().trim().split("\n").map(line => JSON.parse(line));
	expect(rows).toHaveLength(1);
	expect(rows[0]).toMatchObject({ runId: "late-run", sessionId: "native", anchorEntryId: session.entries.at(-1)?.id });
});

it("retains cloud intent before mutation, records a late result only on its original branch, and never replays", async () => {
	const { journals, session, pi, ctx } = await recordedCloud();
	const deletion = vi.fn(async () => {
		const rows = journals.contents.get("cloud")!.toString().trim().split("\n").map(line => JSON.parse(line));
		expect(rows.map(row => row.action)).toEqual(["record", "delete_intent"]);
		await session.branch(null);
	});
	cloud.setSdkOperations({ delete: deletion, archive: vi.fn() });
	await runCursorCloudLifecycleCommand(pi, `delete ${agentId} --yes`, ctx);
	expect(deletion).toHaveBeenCalledTimes(1);
	expect(session.manager.getBranch()).toEqual([]);
	const rows = journals.contents.get("cloud")!.toString().trim().split("\n").map(line => JSON.parse(line));
	expect(rows.map(row => row.action)).toEqual(["record", "delete_intent", "delete"]);
	expect(rows.every(row => row.sessionId === "native" && row.sessionFile === "/isolated/sessions.sqlite")).toBe(true);
	await runCursorCloudLifecycleCommand(pi, `delete ${agentId} --yes`, ctx);
	expect(deletion).toHaveBeenCalledTimes(1);
	expect(pi.appendEntry).not.toHaveBeenCalled();
});

it("keeps an unresolved cloud mutation after SDK failure and refuses a repeat", async () => {
	const { journals, pi, ctx } = await recordedCloud();
	const deletion = vi.fn(async () => { throw new Error("connection lost after submission"); });
	cloud.setSdkOperations({ delete: deletion, archive: vi.fn() });
	await runCursorCloudLifecycleCommand(pi, `delete ${agentId} --yes`, ctx);
	await runCursorCloudLifecycleCommand(pi, `delete ${agentId} --yes`, ctx);
	expect(deletion).toHaveBeenCalledTimes(1);
	expect(journals.contents.get("cloud")!.toString()).toContain('"delete_intent"');
	expect(journals.contents.get("cloud")!.toString()).not.toContain('"action":"delete"');
});

it("does not perform a cloud mutation after failed journal durability or a branch change during intent", async () => {
	for (const branchChanged of [false, true]) {
		cloud.reset();
		const { journals, session, pi, ctx } = await recordedCloud();
		const deletion = vi.fn(), original = journals.request.getMockImplementation()!;
		cloud.setSdkOperations({ delete: deletion, archive: vi.fn() });
		journals.request.mockImplementation(async input => {
			if (input.action === "append") {
				if (!branchChanged) throw new Error("fsync failed");
				await session.branch(null);
			}
			return original(input);
		});
		await runCursorCloudLifecycleCommand(pi, `delete ${agentId} --yes`, ctx);
		expect(deletion).not.toHaveBeenCalled();
		if (branchChanged) expect(session.manager.getBranch()).toEqual([]);
	}
});
