// C-USAGE C07 scenarios. Each one is a faithful transcription of a pinned
// upstream `test/cursor-usage-ledger.test.ts` case (plus two fake-SDK cases),
// run against the unchanged upstream usage modules.
import { createHash } from "node:crypto";
import { mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { createHarness, forkFixed, loadUpstream, makeAssistantMessage, makeModel, openFixed, REFERENCE_ROOT } from "./harness.mjs";

const sha256 = (value) => createHash("sha256").update(value).digest("hex");

export const cached = { inputTokens: 136, outputTokens: 3, cacheReadTokens: 4096, cacheWriteTokens: 0, totalTokens: 4235 };
export const bill = (tokens = cached, chargedCents = 0.2) => ({
	usage: tokens,
	cost: { rawCostCents: 0.3, chargedCents },
	runs: [{ runId: "uuid-a", usage: tokens, cost: { rawCostCents: 0.3, chargedCents } }],
});

const sid = (n) => `20000000-0000-4000-8000-${String(n).padStart(12, "0")}`;
const drain = () => new Promise((resolve) => setImmediate(resolve));

function normalize(value, root) {
	if (typeof value === "string") return value.split(root).join("<root>");
	if (Array.isArray(value)) return value.map((item) => normalize(item, root));
	if (value && typeof value === "object") {
		const out = {};
		for (const [key, child] of Object.entries(value)) out[key] = normalize(child, root);
		return out;
	}
	return value;
}

const json = (value) => JSON.parse(JSON.stringify(value));

function captureJournals(s, root) {
	return s.journalFiles().sort().map((path) => {
		const text = readFileSync(path, "utf8");
		const normalizeLine = (line) => line.split(root).join("<root>");
		const torn = text.length > 0 && !text.endsWith("\n");
		// A deliberately torn tail is not a replayable frame; recovery is covered by
		// the lifecycle fixture's refusal case.
		const complete = torn ? text.slice(0, text.lastIndexOf("\n") + 1) : text;
		const bytes = Buffer.from(normalizeLine(text), "utf8");
		return {
			name: path.slice(path.lastIndexOf("/") + 1),
			replayable: !torn,
			frameLines: complete.split("\n").filter(Boolean).map((line) => Buffer.from(`${normalizeLine(line)}\n`, "utf8").toString("base64")),
			bytesBase64: bytes.toString("base64"),
			bytesSha256: sha256(bytes),
		};
	});
}

function captureView(ledger, s) {
	return normalize(json(ledger.readCursorUsageView(s.pi, s.ctx())), s.root);
}

function make(upstream, seed) {
	const root = mkdtempSync(join(homedir(), ".rpi-c-usage-"));
	const s = createHarness({ ...upstream, root, sessionId: sid(seed) });
	return { root, s };
}

const agent = (overrides = {}) => ({ agentId: "agent-test", getUsage: async () => bill(), ...overrides });

function fileless(s) {
	const real = s.manager;
	const wrapper = Object.create(real);
	wrapper.getSessionFile = () => undefined;
	wrapper.appendCustomEntry = (...args) => real.appendCustomEntry(...args);
	wrapper.appendMessage = (...args) => real.appendMessage(...args);
	return wrapper;
}

export async function runAll({ only } = {}) {
	const upstream = await loadUpstream();
	const { ledger } = upstream;
	const scenarios = [];
	const run = async (name, covers, body) => {
		if (only && only !== name) return;
		const problems = [];
		let observations = null;
		let journals = [];
		let root = null;
		try {
			const result = await body({ upstream, ledger, command: upstream.command, problems });
			root = result?.root ?? null;
			observations = result?.root ? normalize(result.observations, result.root) : result?.observations ?? null;
			if (result?.s) journals = captureJournals(result.s, result.root);
		} catch (error) {
			problems.push(`threw: ${error?.message ?? error}`);
		}
		if (root) rmSync(root, { recursive: true, force: true });
		scenarios.push({ name, covers, observations, journals, problems });
	};

	await run("raw-cache-correction-revised-bills", ["raw cache correction", "revised aggregate bills", "restart"], async ({ ledger }) => {
		const { root, s } = make(upstream, 1);
		const turn = await s.start(agent());
		turn.observeRawTurn({ inputTokens: 4232, outputTokens: 3, cacheReadTokens: 4096, cacheWriteTokens: 0 });
		await drain();
		await turn.recordRun({ runId: "run-client", requestId: "request-client" });
		await turn.recordTerminal({ status: "success", waitUsage: { ...cached, inputTokens: 4232, totalTokens: 8331 }, handleUsage: { ...cached, inputTokens: 4232, totalTokens: 8331 } });
		await turn.recordSnapshot();
		const view1 = captureView(ledger, s);
		const larger = { ...cached, outputTokens: 500003, totalTokens: 504235 };
		const current = { usage: larger, cost: { rawCostCents: 0.1, chargedCents: 0.05 }, runs: [{ runId: "uuid-a", usage: cached, cost: { rawCostCents: 0.1, chargedCents: 0.05 } }] };
		const turn2 = await s.start(agent({ getUsage: async () => current }));
		await turn2.recordSnapshot();
		const view2 = captureView(ledger, s);
		s.replace(openFixed(upstream.SessionManager, s.manager.getSessionFile(), sid(1)));
		const next = await s.start(agent({ getUsage: async () => current }), { resumed: true, newlyCreated: false });
		await next.recordSnapshot();
		const view3 = captureView(ledger, s);
		return {
			root, s,
			observations: {
				correctedRawTotal: view1.correctedRawTotal,
				wholeAgentUsage1: view1.agents[0]?.wholeAgent?.usage,
				terminalWaitUsage: view1.records.find((r) => r.kind === "terminal")?.waitUsage,
				view2ChargedCents: view2.agents[0]?.wholeAgent?.cost?.chargedCents,
				view2AggregateOnlyOutput: view2.agents[0]?.aggregateOnly?.outputTokens,
				view3: { correctedRawTotal: view3.correctedRawTotal, wholeAgentUsage: view3.agents[0]?.wholeAgent?.usage, unknownHistory: view3.agents[0]?.unknownHistory },
				containsNotExported: JSON.stringify(view3).includes("not exported"),
				render: upstream.command.renderCursorUsageView(view3),
			},
		};
	});

	await run("unknown-history-unavailable-late", ["unknown inherited history", "unavailable observation", "late facts"], async ({ ledger, command }) => {
		const { root, s } = make(upstream, 2);
		let mode = "reject";
		const fetch = async () => {
			if (mode === "reject") throw new Error("private credential");
			if (mode === "bill") return bill();
			return { usage: cached, runs: [{ runId: "uuid-a", usage: cached }] };
		};
		const turn = await s.start(agent({ getUsage: fetch }), { resumed: true, newlyCreated: false });
		const render0 = command.renderCursorUsageView(ledger.readCursorUsageView(s.pi, s.ctx()));
		await turn.recordTerminal({ status: "abandon" });
		await turn.recordSnapshot();
		const view1 = captureView(ledger, s);
		mode = "bill";
		await turn.recordTerminal({ status: "abort", waitUsage: cached });
		const view2 = captureView(ledger, s);
		mode = "counts";
		await turn.recordSnapshot();
		const view3 = captureView(ledger, s);
		return {
			root, s,
			observations: {
				render0,
				agent1: view1.agents[0],
				containsPrivateCredential: JSON.stringify(view1).includes("private credential"),
				render1: command.renderCursorUsageView(view1),
				terminals: view2.records.filter((r) => r.kind === "terminal").map((r) => r.status),
				render2: command.renderCursorUsageView(view2),
				render3: command.renderCursorUsageView(view3),
			},
		};
	});

	await run("late-completion-original-journal", ["late completion", "switched branch isolation"], async ({ ledger }) => {
		const { root, s } = make(upstream, 3);
		const oldLeaf = s.manager.getLeafId();
		let resolveBill;
		const turn = await s.start(agent({ getUsage: () => new Promise((resolve) => { resolveBill = resolve; }) }));
		const oldFile = s.manager.getSessionFile();
		const pending = turn.recordTerminal({ status: "success" });
		await drain();
		const nextDir = join(root, "next");
		mkdirSync(nextDir, { recursive: true });
		writeFileSync(join(nextDir, "next.jsonl"), `${JSON.stringify({ type: "session", version: 3, id: sid(30), timestamp: "2026-10-22T00:00:00.000Z", cwd: root })}\n`);
		s.replace(openFixed(upstream.SessionManager, join(nextDir, "next.jsonl"), sid(30)));
		s.manager.appendMessage(makeAssistantMessage());
		await s.invokeEvent("session_start", { type: "session_start", reason: "resume" });
		const writesBefore = s.appended.length;
		resolveBill(bill());
		await pending;
		const writesAfter = s.appended.length;
		const switchedView = captureView(ledger, s);
		s.replace(upstream.SessionManager.open(oldFile));
		await s.invokeEvent("session_start", { type: "session_start", reason: "resume" });
		const reopenedView = captureView(ledger, s);
		s.manager.branch(oldLeaf);
		s.manager.appendMessage({ role: "user", content: "sibling", timestamp: 2 });
		await s.invokeEvent("session_tree", { type: "session_tree", oldLeafId: oldLeaf, newLeafId: s.manager.getLeafId() });
		const before = s.appended.length;
		turn.observeRawTurn({ inputTokens: 1, outputTokens: 1, cacheReadTokens: 0, cacheWriteTokens: 0 });
		await drain();
		return {
			root, s,
			observations: { writesBefore, writesAfter, switchedAgents: switchedView.agents, reopenedWholeAgent: reopenedView.agents[0]?.wholeAgent?.usage, appendedOnBranchChange: s.appended.length - before },
		};
	});

	await run("failed-persistence-torn-recovery", ["failed persistence", "guarded warning", "torn recovery"], async ({ ledger, command }) => {
		const { root, s } = make(upstream, 4);
		const turn = await s.start(agent());
		const path = s.onlyJournal();
		const original = readFileSync(path);
		writeFileSync(path, `${original.toString("utf8")}{"version":1`);
		const thrown = [];
		try { await turn.recordSnapshot(); } catch (error) { thrown.push(error.message); }
		turn.notePersistenceFailure(new Error("secret"));
		turn.notePersistenceFailure(new Error("secret"));
		const view = captureView(ledger, s);
		const bytes = readFileSync(path);
		const exported = join(root, "torn-export.json");
		await s.commands.get("cursor-usage").handler(`export ${exported}`, s.ctx());
		const exportedValue = JSON.parse(readFileSync(exported, "utf8"));
		const bytesUnchanged = Buffer.compare(readFileSync(path), bytes) === 0;
		try { await s.start(agent()); thrown.push("start did not throw"); } catch (error) { thrown.push(error.message); }
		const forkDir = join(root, "fork");
		mkdirSync(forkDir, { recursive: true });
		s.replace(forkFixed(upstream.SessionManager, s.manager.getSessionFile(), root, forkDir, sid(40)));
		const recovered = await s.start(agent());
		await recovered.recordSnapshot();
		const recoveryView = captureView(ledger, s);
		return {
			root, s,
			observations: {
				thrown,
				notify: normalize(s.notify, root),
				view: { agents: view.agents, persistenceStatus: view.persistenceStatus, incompleteJournals: view.incompleteJournals },
				exported: { persistenceStatus: exportedValue.persistenceStatus, records: exportedValue.records.map((r) => ({ kind: r.kind })) },
				bytesUnchanged,
				recoveryWholeAgent: recoveryView.agents[0]?.wholeAgent?.usage,
				recoveryIncompleteJournals: recoveryView.incompleteJournals.length,
				originalJournalSha: sha256(original.toString("utf8").split(root).join("<root>")),
			},
		};
	});

	await run("abandon-without-fetch", ["abandon terminal", "no fetch on abandon"], async ({ ledger }) => {
		const { root, s } = make(upstream, 5);
		let calls = 0;
		const turn = await s.start(agent({ getUsage: async () => { calls += 1; return bill(); } }));
		const abandoned = await turn.recordTerminal({ status: "abandon" });
		const callsAfterAbandon = calls;
		s.replace(upstream.SessionManager.open(s.manager.getSessionFile()));
		const terminal1 = captureView(ledger, s).records.filter((r) => r.kind === "terminal");
		await turn.recordTerminal({ status: "abort" });
		return { root, s, observations: { abandoned: abandoned === undefined ? "undefined" : abandoned, callsAfterAbandon, terminal1, callsAfterAbort: calls } };
	});

	await run("growing-snapshots-linear", ["linear growth", "row revision", "deletion"], async ({ ledger, command }) => {
		const { root, s } = make(upstream, 6);
		let current;
		const turn = await s.start(agent({ getUsage: async () => current }));
		const path = s.onlyJournal();
		const tokens = { inputTokens: 1, outputTokens: 1, cacheReadTokens: 0, cacheWriteTokens: 0, totalTokens: 2 };
		const rows = [];
		let halfJournal = 0;
		let halfSession = 0;
		for (let i = 1; i <= 32; i += 1) {
			rows.push({ runId: `uuid-${i}`, usage: tokens });
			current = { usage: { ...tokens, inputTokens: i, outputTokens: i, totalTokens: i * 2 }, runs: [...rows] };
			await turn.recordSnapshot();
			if (i === 16) { halfJournal = readFileSync(path).length; halfSession = readFileSync(s.manager.getSessionFile()).length; }
		}
		const journalLength = readFileSync(path).length;
		const sessionLength = readFileSync(s.manager.getSessionFile()).length;
		const runs32 = captureView(ledger, s).agents[0]?.wholeAgent?.runs?.length;
		current = { usage: tokens, cost: { rawCostCents: 0.01, chargedCents: 0 }, runs: [{ runId: "uuid-1", usage: tokens, cost: { rawCostCents: 0.01, chargedCents: 0 } }] };
		await turn.recordSnapshot();
		s.replace(upstream.SessionManager.open(s.manager.getSessionFile()));
		const revised = captureView(ledger, s).agents[0]?.wholeAgent;
		current = undefined;
		try { await turn.recordSnapshot(); } catch { /* expected unavailable */ }
		const view = captureView(ledger, s);
		return {
			root, s,
			observations: {
				journalLength, sessionLength, halfJournal, halfSession, runs32, revised,
				agent: { billingStatus: view.agents[0]?.billingStatus, wholeAgent: view.agents[0]?.wholeAgent },
				billingRecords: view.records.filter((r) => r.kind === "billing").length,
				renderContainsLatest: command.renderCursorUsageView(view).includes("Latest known billed tokens: 2"),
			},
		};
	});

	await run("refuse-malformed-symlink-directory", ["malformed journal", "symlink journal", "directory journal"], async ({ ledger }) => {
		const results = {};
		let untouched = null;
		for (const [index, kind] of ["malformed", "symlink", "directory"].entries()) {
			const { root, s } = make(upstream, 70 + index);
			await s.start(agent());
			const path = s.onlyJournal();
			if (kind === "directory") { rmSync(path); mkdirSync(path); }
			else if (kind === "symlink") { const target = join(root, "target"); writeFileSync(target, "untouched"); rmSync(path); symlinkSync(target, path); }
			else writeFileSync(path, `${readFileSync(path, "utf8")}{}\n{"torn":`);
			try { ledger.readCursorUsageView(s.pi, s.ctx()); results[kind] = "no throw"; } catch (error) { results[kind] = `threw: ${error.message}`; }
			if (kind === "symlink") untouched = readFileSync(join(root, "target"), "utf8");
			rmSync(root, { recursive: true, force: true });
		}
		return { observations: { results, untouched } };
	});

	await run("fork-shared-lineage", ["fork lineage", "shared spend"], async ({ ledger, command }) => {
		const { root, s } = make(upstream, 8);
		const original = await s.start(agent());
		await original.recordSnapshot();
		const forkDir = join(root, "fork");
		mkdirSync(forkDir, { recursive: true });
		s.replace(forkFixed(upstream.SessionManager, s.manager.getSessionFile(), root, forkDir, sid(80)));
		const inherited = await s.start(agent(), { resumed: true, newlyCreated: false });
		await inherited.recordSnapshot();
		const view = captureView(ledger, s);
		return {
			root, s,
			observations: {
				agent: { sharedAcrossBranches: view.agents[0]?.sharedAcrossBranches, unknownHistory: view.agents[0]?.unknownHistory, billingStatus: view.agents[0]?.billingStatus, wholeAgent: view.agents[0]?.wholeAgent, latestAggregate: view.agents[0]?.latestAggregate, observationUsages: view.agents[0]?.observationsByOrigin.map((o) => o.latestAggregate?.usage) },
				sessionIds: view.records.map((r) => r.origin.sessionId),
				render: command.renderCursorUsageView(view),
			},
		};
	});

	await run("journal-only-descendant", ["mirror failure", "descendant ownership"], async ({ ledger }) => {
		const { root, s } = make(upstream, 9);
		const ancestor = s.manager.getLeafId();
		s.pi.appendEntry = (kind, data) => { if (kind === "pi-cursor-sdk:usage-v1") throw new Error("optional mirror failed"); s.manager.appendCustomEntry(kind, data); };
		const turn = await s.start(agent());
		await turn.recordSnapshot();
		s.manager.appendMessage(makeAssistantMessage());
		s.manager.appendMessage({ role: "user", content: "later", timestamp: 2 });
		s.replace(upstream.SessionManager.open(s.manager.getSessionFile()));
		const wholeAgent = captureView(ledger, s).agents[0]?.wholeAgent?.usage;
		s.manager.branch(ancestor);
		s.manager.appendMessage({ role: "user", content: "sibling", timestamp: 3 });
		const sibling = captureView(ledger, s);
		return { root, s, observations: { wholeAgent, siblingAgents: sibling.agents, otherBranchRecords: sibling.otherBranchRecords.length, unclaimedRecords: sibling.unclaimedRecords } };
	});

	await run("append-chronology-backward-clock", ["append chronology", "backward wall clock"], async ({ ledger }) => {
		const { root, s } = make(upstream, 10);
		let current = bill();
		const turn = await s.start(agent({ getUsage: async () => current }));
		await turn.recordSnapshot();
		current = bill(cached, 0.1);
		await turn.recordSnapshot();
		current = bill(cached, 0.05);
		await turn.recordSnapshot();
		const path = s.onlyJournal();
		const frames = readFileSync(path, "utf8").trim().split("\n").map((line) => JSON.parse(line));
		frames[1].timestamp = "2030-01-01T00:00:00Z";
		frames[2].timestamp = "1990-01-01T00:00:00Z";
		frames[3].timestamp = "1990-01-01T00:00:00Z";
		writeFileSync(path, `${frames.map((frame) => JSON.stringify(frame)).join("\n")}\n`);
		const view = captureView(ledger, s);
		return { root, s, observations: { chargedCents: view.agents[0]?.wholeAgent?.cost?.chargedCents, timestamps: view.records.map((r) => r.timestamp) } };
	});

	await run("cloud-unqualified-invalid-local", ["cloud telemetry", "invalid local partition"], async ({ ledger }) => {
		const { root, s } = make(upstream, 11);
		const cloud = await s.start(agent(), { runtime: "cloud" });
		cloud.observeRawTurn(cached);
		await drain();
		const local = await s.start(agent());
		local.observeRawTurn(cached);
		await drain();
		const raw = captureView(ledger, s).records.filter((r) => r.kind === "raw");
		return { root, s, observations: { normalization: raw.map((r) => r.normalization), correctedPresent: raw.map((r) => r.corrected !== undefined), reported: raw.map((r) => r.reported) } };
	});

	await run("pricing-tiers-immutable", ["pricing tiers", "metadata rejection"], async ({ ledger }) => {
		const { root, s } = make(upstream, 12);
		const model = makeModel();
		const tier = { ...model.cost, inputTokensAbove: 100, input: 7 };
		model.cost = { ...model.cost, tiers: [tier] };
		await s.start(agent(), { model });
		tier.input = 99;
		const start = captureView(ledger, s).records.find((r) => r.kind === "start");
		const thrown = [];
		try { await s.start(agent(), { modelSelection: { id: "model", secret: "not allowed" } }); } catch (error) { thrown.push(error.message); }
		try { await s.start(agent(), { resumed: true, newlyCreated: true }); } catch (error) { thrown.push(error.message); }
		return { root, s, observations: { tierInput: start?.data?.model?.cost?.tiers?.[0]?.input, thrown } };
	});

	await run("cross-directory-fork-unavailable", ["cross-directory fork", "unavailable inherited journal"], async ({ ledger, command }) => {
		const { root, s } = make(upstream, 13);
		const turn = await s.start(agent());
		await turn.recordSnapshot();
		const source = s.manager.getSessionFile();
		const target = join(root, "target");
		mkdirSync(target, { recursive: true });
		s.replace(forkFixed(upstream.SessionManager, source, target, target, sid(130)));
		let view = captureView(ledger, s);
		const beforeDelete = { wholeAgent: view.agents[0]?.wholeAgent?.usage };
		rmSync(join(root, "sessions", readdirSync(join(root, "sessions")).find((name) => name.endsWith(".journal"))));
		view = captureView(ledger, s);
		const observations = {
			beforeDelete,
			latestAggregate: view.agents[0]?.latestAggregate,
			lineageGaps: view.lineageGaps,
			persistenceStatus: view.persistenceStatus,
			wholeAgent: view.agents[0]?.wholeAgent,
			observation: view.agents[0]?.observationsByOrigin[0],
			aggregateOnly: view.agents[0]?.aggregateOnly,
			render: command.renderCursorUsageView(view),
		};
		rmSync(target, { recursive: true, force: true });
		return { root, s, observations };
	});

	await run("aggregate-revisions-missing-mirrors", ["aggregate revisions", "missing mirrors"], async ({ ledger, command }) => {
		const { root, s } = make(upstream, 14);
		let current = bill();
		const turn = await s.start(agent({ getUsage: async () => current }));
		await turn.recordSnapshot();
		const append = s.pi.appendEntry;
		s.pi.appendEntry = (kind, data) => { if (kind !== "pi-cursor-sdk:usage-v1") append(kind, data); };
		const lower = { usage: { ...cached, cacheReadTokens: 0, totalTokens: 139 }, cost: { rawCostCents: 0.15, chargedCents: 0.1 }, runs: [] };
		current = lower;
		await turn.recordSnapshot();
		s.pi.appendEntry = append;
		await turn.recordSnapshot();
		current = undefined;
		try { await turn.recordSnapshot(); } catch { /* expected unavailable */ }
		rmSync(join(root, "sessions", readdirSync(join(root, "sessions")).find((name) => name.endsWith(".journal"))));
		const view = captureView(ledger, s);
		const exported = join(root, "incomplete-export.json");
		await s.commands.get("cursor-usage").handler(`export ${exported}`, s.ctx());
		const exportedAgent = JSON.parse(readFileSync(exported, "utf8")).agents[0];
		return {
			root, s,
			observations: {
				persistenceStatus: view.persistenceStatus,
				agent: { wholeAgent: view.agents[0]?.wholeAgent, lastAttempt: view.agents[0]?.lastAttempt, latestAggregate: view.agents[0]?.latestAggregate, aggregateOnly: view.agents[0]?.aggregateOnly, observation: view.agents[0]?.observationsByOrigin[0] },
				billingRecords: view.records.filter((r) => r.kind === "billing").length,
				render: command.renderCursorUsageView(view),
				exportedAgent,
			},
		};
	});

	await run("other-branch-claims-fork", ["other-branch claims"], async ({ ledger }) => {
		const { root, s } = make(upstream, 15);
		const ancestor = s.manager.getLeafId();
		const turn = await s.start(agent());
		await turn.recordSnapshot();
		s.manager.branch(ancestor);
		s.manager.appendMessage({ role: "user", content: "fork from sibling", timestamp: 2 });
		await s.start(agent({ agentId: "agent-sibling" }));
		const forkDir = join(root, "fork");
		mkdirSync(forkDir, { recursive: true });
		s.replace(forkFixed(upstream.SessionManager, s.manager.getSessionFile(), root, forkDir, sid(150)));
		const view = captureView(ledger, s);
		return { root, s, observations: { unclaimedRecords: view.unclaimedRecords, otherBranchRecords: view.otherBranchRecords.length, records: view.records.length, inheritedUnclaimedRecords: view.inheritedUnclaimedRecords } };
	});

	await run("first-turn-orphan-unclaimed", ["orphan intent", "lost claim"], async ({ ledger }) => {
		const { root, s } = make(upstream, 16);
		const beforeClaim = readFileSync(s.manager.getSessionFile(), "utf8");
		const turn = await s.start(agent());
		await turn.recordSnapshot();
		const recoveredFile = join(root, "recovered-same-native-id.jsonl");
		writeFileSync(recoveredFile, beforeClaim);
		s.replace(upstream.SessionManager.open(recoveredFile));
		const view = captureView(ledger, s);
		await s.start(agent({ agentId: "agent-sibling" }));
		const forkDir = join(root, "fork");
		mkdirSync(forkDir, { recursive: true });
		s.replace(forkFixed(upstream.SessionManager, s.manager.getSessionFile(), root, forkDir, sid(160)));
		const inherited = captureView(ledger, s);
		return { root, s, observations: { view: { records: view.records, agents: view.agents, unclaimed: view.unclaimedRecords.length }, inherited: { records: inherited.records.length, unclaimed: inherited.unclaimedRecords, otherBranch: inherited.otherBranchRecords, inheritedUnclaimed: inherited.inheritedUnclaimedRecords.length } } };
	});

	await run("help-missing-export-non-ui", ["help", "missing export", "stdout untouched"], async () => {
		const { root, s } = make(upstream, 18);
		await s.start(agent());
		const command = s.commands.get("cursor-usage");
		const stderr = [];
		const stdout = [];
		const realErr = process.stderr.write;
		const realOut = process.stdout.write;
		process.stderr.write = (chunk) => { stderr.push(String(chunk)); return true; };
		process.stdout.write = (chunk) => { stdout.push(String(chunk)); return true; };
		try {
			const ctx = { ...s.ctx(), hasUI: false };
			await command.handler("--help", ctx);
			await command.handler("export", ctx);
		} finally {
			process.stderr.write = realErr;
			process.stdout.write = realOut;
		}
		return { root, s, observations: { stderr: normalize(stderr.join("\n"), root), stdoutCalls: stdout.length } };
	});

	await run("refresh-bounded-exclusive-export", ["bounded refresh", "exclusive export"], async ({ ledger }) => {
		const { root, s } = make(upstream, 19);
		await s.start(agent());
		let fetchCount = 0;
		const fetch = async () => { fetchCount += 1; return { status: "observed-pending-settlement", snapshot: bill() }; };
		const view = normalize(json(await ledger.refreshCursorUsageView(s.pi, s.ctx(), fetch)), root);
		const path = join(root, "export.json");
		const command = s.commands.get("cursor-usage");
		await command.handler(`export ${path}`, s.ctx());
		const content = readFileSync(path, "utf8");
		await command.handler(`export ${path}`, s.ctx());
		const replaced = readFileSync(path, "utf8") !== content;
		return { root, s, observations: { fetchCount, wholeAgentCost: view.agents[0]?.wholeAgent?.cost, records: JSON.parse(content).records.length, containsNotExported: content.includes("not exported"), replaced } };
	});

	await run("fake-sdk-command-refresh", ["fake SDK usage endpoint", "command refresh"], async ({ ledger }) => {
		const { root, s } = make(upstream, 20);
		await s.start(agent());
		process.env.RPI_CURSOR_USAGE_SDK_RESPONSE = JSON.stringify({ usage: cached, cost: { rawCostCents: 0.3, chargedCents: 0.2 }, runs: [{ runId: "uuid-sdk", usage: cached, cost: { rawCostCents: 0.3, chargedCents: 0.2 } }] });
		try {
			await s.commands.get("cursor-usage").handler("refresh", s.ctx());
		} finally {
			delete process.env.RPI_CURSOR_USAGE_SDK_RESPONSE;
		}
		const view = captureView(ledger, s);
		return { root, s, observations: { agent: view.agents[0], notify: normalize(s.notify, root) } };
	});

	await run("fileless-ephemeral-fixed", ["ephemeral session"], async ({ ledger }) => {
		const { root, s } = make(upstream, 21);
		s.override(fileless(s));
		const turn = await s.start(agent());
		turn.observeRawTurn({ inputTokens: 2, outputTokens: 1, cacheReadTokens: 0, cacheWriteTokens: 0 });
		await drain();
		await turn.recordTerminal({ status: "success", waitUsage: { invalid: "not stored" } });
		const view = captureView(ledger, s);
		return { root, s, observations: { durability: view.durability, correctedRawTotal: view.correctedRawTotal, terminal: view.records.find((r) => r.kind === "terminal"), journalFiles: s.journalFiles().length } };
	});

	const recovery = await buildRecovery();
	return { schema: "rpi-cursor-c-usage-fixture/1", scenarios, recovery };
}

async function buildRecovery() {
	const durable = await import(join(REFERENCE_ROOT, "node_modules/pi-cursor-sdk/dist/cursor-durable-fs.js"));
	const records = [
		{ version: 1, id: sid(900), kind: "start", data: { agentId: "agent-test" } },
		{ version: 1, id: sid(901), kind: "raw", reported: cached },
	];
	const cases = [
		{ name: "empty-journal", prefix: null },
		{ name: "complete-frame", prefix: `${JSON.stringify(records[0])}\n` },
		{ name: "torn-tail", prefix: `${JSON.stringify(records[0])}\n{"torn":` },
		{ name: "no-newline", prefix: `${JSON.stringify(records[0])}` },
	];
	return cases.map((entry) => {
		const root = mkdtempSync(join(homedir(), ".rpi-c-usage-recovery-"));
		const path = join(root, "cursor-usage.journal");
		if (entry.prefix !== null) writeFileSync(path, entry.prefix);
		let outcome = "committed";
		try { durable.appendDurableFrame(path, records[1]); } catch (error) { outcome = `threw: ${error.message}`; }
		const bytes = readFileSync(path, "utf8").split(root).join("<root>");
		rmSync(root, { recursive: true, force: true });
		return {
			name: entry.name,
			prefixBytesBase64: entry.prefix === null ? null : Buffer.from(entry.prefix, "utf8").toString("base64"),
			frameLineBase64: Buffer.from(`${JSON.stringify(records[1])}\n`, "utf8").toString("base64"),
			outcome,
			bytesBase64: Buffer.from(bytes, "utf8").toString("base64"),
			bytesSha256: sha256(bytes),
		};
	});
}
