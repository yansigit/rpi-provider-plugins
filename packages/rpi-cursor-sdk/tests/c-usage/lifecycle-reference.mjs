#!/usr/bin/env node
// C-USAGE C09 lifecycle reference: flush and reap transitions at the durable
// boundary of the unchanged pinned pi-cursor-sdk modules.
//
// It records (a) usage-journal append/refusal after a crashed writer, (b) cloud
// lifecycle journal append/repair after a crashed writer, and (c) intent-then-
// result sequences that must survive a reload. Each fixture operation carries the
// exact bytes the unchanged writer added, the outcome, and the resulting bytes.
//
//   node tests/c-usage/lifecycle-reference.mjs            check mode
//   node tests/c-usage/lifecycle-reference.mjs --write    regenerate the fixture
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const FIXTURE_PATH = join(HERE, "lifecycle-fixture.json");
const EVIDENCE_PATH = join(HERE, "lifecycle-evidence.json");
const args = process.argv.slice(2);
const WRITE = args.includes("--write");
const CHILD = process.env.RPI_CURSOR_LIFECYCLE_INJECTED === "1";
const sha256 = (value) => createHash("sha256").update(value).digest("hex");

if (CHILD) {
	process.stdout.write(`${JSON.stringify(await runCases(), null, 2)}\n`);
} else {
	await orchestrate();
}

async function orchestrate() {
	const referenceRoot = process.env.RPI_CURSOR_REFERENCE ?? resolve(HERE, "../../../../../provider-references/integrated");
	const outputs = [];
	for (let attempt = 0; attempt < 2; attempt += 1) {
		const result = spawnSync(process.execPath, ["--require", join(HERE, "inject.cjs"), fileURLToPath(import.meta.url)], {
			env: { ...process.env, RPI_CURSOR_LIFECYCLE_INJECTED: "1", RPI_CURSOR_REFERENCE: referenceRoot },
			encoding: "utf8",
			maxBuffer: 16 * 1024 * 1024,
		});
		if (result.status !== 0) {
			process.stderr.write(result.stderr ?? "");
			process.exitCode = 1;
			return;
		}
		outputs.push(result.stdout);
	}
	const deterministic = outputs[0] === outputs[1];
	const fixture = JSON.parse(outputs[0]);
	const files = {
		"pi-cursor-sdk/dist/cursor-durable-fs.js": join(referenceRoot, "node_modules/pi-cursor-sdk/dist/cursor-durable-fs.js"),
		"pi-cursor-sdk/dist/cursor-cloud-lifecycle.js": join(referenceRoot, "node_modules/pi-cursor-sdk/dist/cursor-cloud-lifecycle.js"),
	};
	const reference = {};
	for (const [name, path] of Object.entries(files)) {
		if (!existsSync(path)) throw new Error(`reference file missing: ${path}`);
		reference[name] = sha256(readFileSync(path));
	}
	const evidence = {
		status: deterministic ? "pass" : "fail",
		schema: "rpi-cursor-c09-lifecycle-evidence/1",
		purpose: "Durable flush/recovery boundary for C09: a crashed writer leaves a torn or complete last frame; the unchanged pinned writers refuse or repair the next append, and intent-then-result cloud sequences survive a reload. The Rust test replays the recorded bytes through the native JournalPort.",
		reference,
		injection: {
			clock: "Date fixed at 2024-10-15T13:46:40.123Z via inject.cjs.",
			paths: "The per-run temporary root is replaced by <root> in every recorded byte.",
		},
		cases: fixture.cases.map((entry) => ({ name: entry.name, kind: entry.kind, operations: entry.ops.length, outcomes: entry.ops.map((op) => op.outcome) })),
		counts: {
			cases: fixture.cases.length,
			usage: fixture.cases.filter((entry) => entry.kind === "usage").length,
			cloud: fixture.cases.filter((entry) => entry.kind === "cloud").length,
			operations: fixture.cases.reduce((sum, entry) => sum + entry.ops.length, 0),
		},
		rust: "tests/c_usage_reference.rs replays each operation through JournalPort and compares committed/refused outcomes and complete bytes.",
		notTested: [
			"Stop admission, cancellation, MCP/SDK close and worker reaping are Rust owner behaviour with no upstream TypeScript counterpart; tests/worker.rs covers them (crash partial preservation, admitted host drain, one-shot cancellation, capacity).",
			"Interactive reload/shutdown of the installed DLL is covered by tests/shutdown-evidence.json, not re-run here.",
		],
		commands: ["node tests/c-usage/lifecycle-reference.mjs", "node tests/c-usage/lifecycle-reference.mjs --write"],
	};
	const text = `${JSON.stringify(fixture, null, 2)}\n`;
	const evidenceText = `${JSON.stringify(evidence, null, 2)}\n`;
	if (WRITE) {
		writeFileSync(FIXTURE_PATH, text);
		writeFileSync(EVIDENCE_PATH, evidenceText);
		process.stdout.write(`wrote ${FIXTURE_PATH}\n`);
	} else {
		let ok = deterministic;
		if (!existsSync(FIXTURE_PATH) || readFileSync(FIXTURE_PATH, "utf8") !== text) { ok = false; process.stderr.write("- lifecycle-fixture.json is stale\n"); }
		if (!existsSync(EVIDENCE_PATH) || readFileSync(EVIDENCE_PATH, "utf8") !== evidenceText) { ok = false; process.stderr.write("- lifecycle-evidence.json is stale\n"); }
		for (const entry of fixture.cases) process.stdout.write(`PASS ${entry.name}\n`);
		if (!ok) process.exitCode = 1;
	}
}

async function runCases() {
	const referenceRoot = process.env.RPI_CURSOR_REFERENCE ?? resolve(HERE, "../../../../../provider-references/integrated");
	const durable = await import(join(referenceRoot, "node_modules/pi-cursor-sdk/dist/cursor-durable-fs.js"));
	const cloud = await import(join(referenceRoot, "node_modules/pi-cursor-sdk/dist/cursor-cloud-lifecycle.js"));

	const record1 = { version: 1, id: "33333333-3333-4333-8333-000000000001", kind: "start", data: { agentId: "agent-test" } };
	const record2 = { version: 1, id: "33333333-3333-4333-8333-000000000002", kind: "raw", reported: { inputTokens: 1 } };

	const cases = [];
	const normalize = (value, root) => value.split(root).join("<root>");

	const runUsage = (name, prefix, record) => {
		const root = mkdtempSync(join(homedir(), ".rpi-c-lifecycle-"));
		const path = join(root, "cursor-usage.journal");
		if (prefix !== null) writeFileSync(path, prefix);
		const before = existsSync(path) ? readFileSync(path) : Buffer.alloc(0);
		let outcome = "committed";
		try { durable.appendDurableFrame(path, record); } catch (error) { outcome = `threw: ${error.message}`; }
		const after = existsSync(path) ? readFileSync(path) : Buffer.alloc(0);
		const frame = Buffer.from(`${JSON.stringify(record)}\n`, "utf8");
		const delta = before.length <= after.length ? after.subarray(before.length) : Buffer.alloc(0);
		cases.push({
			name, kind: "usage",
			prefixBase64: prefix === null ? null : Buffer.from(normalize(prefix, root), "utf8").toString("base64"),
			ops: [{
				frameBase64: Buffer.from(normalize(frame.toString("utf8"), root), "utf8").toString("base64"),
				deltaBase64: Buffer.from(normalize(delta.toString("utf8"), root), "utf8").toString("base64"),
				outcome,
				bytesAfterBase64: Buffer.from(normalize(after.toString("utf8"), root), "utf8").toString("base64"),
				bytesAfterSha256: sha256(normalize(after.toString("utf8"), root)),
			}],
		});
		rmSync(root, { recursive: true, force: true });
	};

	const runCloud = (name, prefix, reports) => {
		const root = mkdtempSync(join(homedir(), ".rpi-c-lifecycle-"));
		const sessionFile = join(root, "session.jsonl");
		writeFileSync(sessionFile, "fixture\n");
		const path = cloud.__testUtils.durableLedgerPath(sessionFile, "fixture");
		if (prefix !== null) writeFileSync(path, prefix);
		const ops = [];
		for (const report of reports) {
			const before = existsSync(path) ? readFileSync(path) : Buffer.alloc(0);
			cloud.__testUtils.reset();
			const entries = [];
			const handlers = new Map();
			const pi = { on: (n, h) => handlers.set(n, h), appendEntry: (customType, data) => entries.push({ id: `entry-${entries.length + 1}`, type: "custom", customType, data }) };
			cloud.registerCursorCloudLifecycleLedger(pi);
			handlers.get("session_start")({}, { sessionManager: { getSessionFile: () => sessionFile, getSessionId: () => "fixture", getBranch: () => entries } });
			let outcome = "committed";
			try {
				const recorded = cloud.recordCursorCloudLifecycleRun(report);
				if (!recorded) outcome = "refused";
			} catch (error) { outcome = `threw: ${error.message}`; }
			const after = existsSync(path) ? readFileSync(path) : Buffer.alloc(0);
			const delta = before.length <= after.length ? after.subarray(before.length) : Buffer.alloc(0);
			// The cloud writer repairs a missing boundary by prepending one newline.
			// Record the frame itself; the native port performs the same repair.
			const frame = before.length > 0 && before[before.length - 1] !== 0x0a && delta[0] === 0x0a ? delta.subarray(1) : delta;
			const normalized = normalize(frame.toString("utf8"), root);
			const normalizedDelta = normalize(delta.toString("utf8"), root);
			ops.push({
				frameBase64: Buffer.from(normalized, "utf8").toString("base64"),
				deltaBase64: Buffer.from(normalizedDelta, "utf8").toString("base64"),
				outcome,
				bytesAfterBase64: Buffer.from(normalize(after.toString("utf8"), root), "utf8").toString("base64"),
				bytesAfterSha256: sha256(normalize(after.toString("utf8"), root)),
			});
		}
		cases.push({ name, kind: "cloud", prefixBase64: prefix === null ? null : Buffer.from(normalize(prefix, root), "utf8").toString("base64"), ops });
		rmSync(root, { recursive: true, force: true });
	};

	const report = (n) => ({ agentId: `bc-00000000-0000-0000-0000-00000000000${n}`, runId: `run-${n}`, branches: [] });
	const frame1 = `${JSON.stringify(record1)}\n`;

	runUsage("usage-empty", null, record1);
	runUsage("usage-existing-frame", frame1, record2);
	runUsage("usage-torn-tail-refused", `${frame1}{"torn":`, record2);
	runUsage("usage-no-newline-refused", JSON.stringify(record1), record2);

	runCloud("cloud-empty", null, [report(1)]);
	runCloud("cloud-torn-tail-repaired", `${frame1}{"torn":`, [report(2)]);
	runCloud("cloud-crash-reload-after-intent", null, [report(3), report(3)]);
	runCloud("cloud-crash-mid-result-reload", `${frame1}{"torn":`, [report(4), report(5)]);

	return { schema: "rpi-cursor-c09-lifecycle-fixture/1", cases };
}
