#!/usr/bin/env node
// C-USAGE reference: drives the UNCHANGED pinned upstream usage modules with the
// real Pi 1.3.0 `SessionManager`, an injected clock/randomness/IDs and a fake
// `@cursor/sdk` usage/billing endpoint. It records complete observable values for
// the C07 usage/billing refresh/export/interrupted-recovery cases.
//
//   node tests/c-usage/usage-reference.mjs            check mode
//   node tests/c-usage/usage-reference.mjs --write    regenerate the fixture
//   node tests/c-usage/usage-reference.mjs --only NAME
//
// Determinism: the parent runs the child twice and requires byte-identical
// output. The child preloads `inject.cjs` (fixed Date and crypto IDs) and
// `register.mjs` (the fake @cursor/sdk resolve hook).
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const FIXTURE_PATH = join(HERE, "usage-fixture.json");
const EVIDENCE_PATH = join(HERE, "evidence.json");

const args = process.argv.slice(2);
const WRITE = args.includes("--write");
const ONLY = args.includes("--only") ? args[args.indexOf("--only") + 1] : undefined;
const CHILD = process.env.RPI_CURSOR_USAGE_INJECTED === "1";

const sha256 = (value) => createHash("sha256").update(value).digest("hex");

if (CHILD) {
	await runChild();
} else {
	await orchestrate();
}

async function runChild() {
	const { runAll } = await import(join(HERE, "scenarios.mjs"));
	const fixture = await runAll({ only: ONLY });
	process.stdout.write(`${JSON.stringify(fixture, null, 2)}\n`);
}

async function orchestrate() {
	const referenceRoot = process.env.RPI_CURSOR_REFERENCE ?? resolve(HERE, "../../../../../provider-references/integrated");
	const children = [];
	for (let attempt = 1; attempt <= 2; attempt += 1) {
		const result = spawnSync(process.execPath, [
			"--require", join(HERE, "inject.cjs"),
			"--import", join(HERE, "register.mjs"),
			fileURLToPath(import.meta.url),
			...(ONLY ? ["--only", ONLY] : []),
		], {
			env: {
				...process.env,
				RPI_CURSOR_USAGE_INJECTED: "1",
				// Activate the fake @cursor/sdk resolve hook for the whole child.
				RPI_CURSOR_USAGE_SDK_RESPONSE: process.env.RPI_CURSOR_USAGE_SDK_RESPONSE ?? "{}",
				RPI_CURSOR_REFERENCE: referenceRoot,
			},
			encoding: "utf8",
			maxBuffer: 64 * 1024 * 1024,
		});
		if (result.status !== 0) {
			process.stderr.write(result.stderr ?? "");
			process.exitCode = 1;
			return;
		}
		children.push(result.stdout);
	}
	const deterministic = children[0] === children[1];
	const fixture = JSON.parse(children[0]);
	const problems = [];
	if (!deterministic) problems.push("child output differs between two runs");
	for (const scenario of fixture.scenarios) for (const problem of scenario.problems ?? []) problems.push(`${scenario.name}: ${problem}`);
	if (process.env.RPI_CURSOR_USAGE_DUMP) {
		writeFileSync(process.env.RPI_CURSOR_USAGE_DUMP, children[0]);
	}
	const evidence = buildEvidence(referenceRoot, fixture, problems);
	const text = `${JSON.stringify(fixture, null, 2)}\n`;
	const evidenceText = `${JSON.stringify(evidence, null, 2)}\n`;
	if (WRITE) {
		writeFileSync(FIXTURE_PATH, text);
		writeFileSync(EVIDENCE_PATH, evidenceText);
		process.stdout.write(`wrote ${FIXTURE_PATH}\n`);
	} else {
		if (!existsSync(FIXTURE_PATH)) throw new Error("usage-fixture.json missing; run with --write");
		if (readFileSync(FIXTURE_PATH, "utf8") !== text) problems.push("usage-fixture.json is stale");
		if (!existsSync(EVIDENCE_PATH)) problems.push("evidence.json missing; run with --write");
		else if (readFileSync(EVIDENCE_PATH, "utf8") !== evidenceText) problems.push("evidence.json is stale");
		for (const scenario of fixture.scenarios) {
			process.stdout.write(`${scenario.problems?.length ? "FAIL" : "PASS"} ${scenario.name}${scenario.problems?.length ? `: ${scenario.problems.join("; ")}` : ""}\n`);
		}
	}
	if (problems.length > 0) {
		for (const problem of problems) process.stderr.write(`- ${problem}\n`);
		process.exitCode = 1;
	}
}

function hashFile(path) {
	return existsSync(path) ? sha256(readFileSync(path)) : null;
}

function buildEvidence(referenceRoot, fixture, problems) {
	const upstreamFiles = {
		"pi-cursor-sdk/dist/cursor-usage-ledger.js": join(referenceRoot, "node_modules/pi-cursor-sdk/dist/cursor-usage-ledger.js"),
		"pi-cursor-sdk/dist/cursor-usage-command.js": join(referenceRoot, "node_modules/pi-cursor-sdk/dist/cursor-usage-command.js"),
		"pi-cursor-sdk/dist/cursor-durable-fs.js": join(referenceRoot, "node_modules/pi-cursor-sdk/dist/cursor-durable-fs.js"),
		"pi-cursor-sdk/dist/cursor-sdk-billed-usage.js": join(referenceRoot, "node_modules/pi-cursor-sdk/dist/cursor-sdk-billed-usage.js"),
		"pi-coding-agent/dist/core/session-manager.js": join(referenceRoot, "node_modules/@earendil-works/pi-coding-agent/dist/core/session-manager.js"),
	};
	const upstream = {};
	for (const [name, path] of Object.entries(upstreamFiles)) {
		if (!existsSync(path)) throw new Error(`reference file missing: ${path}`);
		upstream[name] = hashFile(path);
	}
	const packageJson = JSON.parse(readFileSync(join(referenceRoot, "node_modules/pi-cursor-sdk/package.json"), "utf8"));
	const harness = ["harness.mjs", "scenarios.mjs", "sdk.mjs", "hooks.mjs", "register.mjs", "inject.cjs"];
	return {
		status: problems.length === 0 && fixture.scenarios.every((scenario) => !scenario.problems?.length) ? "pass" : "fail",
		schema: "rpi-cursor-c-usage-evidence/1",
		purpose: "Offline differential of the C07 usage/billing view, refresh, export and interrupted-recovery behaviour of the unchanged pinned pi-cursor-sdk usage modules, run against the real Pi 1.3.0 SessionManager with an injected clock/randomness/IDs and a fake @cursor/sdk usage endpoint. No Cursor network calls.",
		reference: {
			piCursorSdkVersion: packageJson.version,
			revision: packageJson.repository?.url ?? null,
			files: upstream,
		},
		harness: Object.fromEntries(harness.map((name) => [name, hashFile(join(HERE, name))])),
		injection: {
			clock: "Date is fixed at 2026-10-22T00:00:00.123Z via inject.cjs.",
			randomness: "node:crypto randomUUID/randomBytes/randomInt are replaced by monotonic deterministic values via a CJS preload, so entry ids, turn ids and record ids replay.",
			sessions: "Session files are prefixed with a fixed header id; interrupted-recovery forks reuse that id.",
			fakeSdk: "The bare @cursor/sdk specifier is redirected to tests/c-usage/sdk.mjs; Agent.getUsage answers from RPI_CURSOR_USAGE_SDK_RESPONSE.",
			paths: "The per-run temporary root is replaced by <root> in every captured string and journal byte.",
		},
		scenarios: fixture.scenarios.map((scenario) => ({
			name: scenario.name,
			covers: scenario.covers,
			status: scenario.problems?.length ? "fail" : "pass",
			problems: scenario.problems ?? [],
			observations: scenario.observations,
			journals: scenario.journals?.length ?? 0,
			journalFrames: scenario.journals?.reduce((sum, journal) => sum + journal.frameLines.length, 0) ?? 0,
		})),
		recovery: fixture.recovery?.map((entry) => ({ name: entry.name, outcome: entry.outcome, bytes: entry.bytesSha256 })),
		counts: {
			scenarios: fixture.scenarios.length,
			scenarioPass: fixture.scenarios.filter((scenario) => !scenario.problems?.length).length,
			recoveryCases: fixture.recovery?.length ?? 0,
			journals: fixture.scenarios.reduce((sum, scenario) => sum + (scenario.journals?.length ?? 0), 0),
			frames: fixture.scenarios.reduce((sum, scenario) => sum + (scenario.journals?.reduce((n, journal) => n + journal.frameLines.length, 0) ?? 0), 0),
		},
		components: [
			"raw cache correction and repeated/revised aggregate bills",
			"unknown inherited history, unavailable observation, late terminal and billing facts",
			"late completion on the original journal and switched branch isolation",
			"failed persistence, guarded warning, export and torn-journal recovery",
			"abandon without fetch, then eventual billing",
			"linear growing snapshots, row revision and deletion",
			"malformed, symlink and directory journal refusal",
			"fork lineage and shared-spend ambiguity",
			"journal-only descendant facts after mirror failure",
			"append-chronology revision order under backward wall clocks",
			"unqualified Cloud and invalid LOCAL telemetry",
			"immutable pricing tiers and metadata rejection",
			"cross-directory fork with an unavailable inherited journal",
			"available aggregate revisions with missing mirrors",
			"other-branch claims copied by a native fork",
			"first-turn orphan intent without a native claim",
			"fileless ephemeral sessions",
			"help and missing export arguments in non-UI mode",
			"bounded refresh through the command and an exclusive export",
			"fake @cursor/sdk endpoint refresh through the command",
		],
		commands: [
			"node tests/c-usage/usage-reference.mjs        (check mode)",
			"node tests/c-usage/usage-reference.mjs --write",
			"node tests/c-usage/usage-reference.mjs --only <name>",
		],
		rust: "tests/c_usage_reference.rs replays every recorded journal frame sequence and recovery prefix through the native JournalPort and compares complete bytes and outcomes.",
		notTested: [
			"Live Cursor billing, real cloud agents and the raw REST usage fallback (/v1/agents/{id}/usage) are not exercised.",
			"Interactive/TUI/RPC rendering is not exercised; only the non-UI notification and export paths are recorded.",
			"The native rpi worker's own copy of the usage ledger is exercised only at the journal-port boundary by the Rust replay, not end-to-end through the DLL.",
			"C09 lifecycle stop-admission/cancel/close/flush/reap is covered separately by lifecycle-reference.mjs and the Rust worker tests.",
		],
		problems,
	};
}
