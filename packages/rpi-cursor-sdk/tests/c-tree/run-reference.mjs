#!/usr/bin/env node
// Reference runner for ledger C04 (branch continuity, compaction/summary agents,
// store/lineage/exact-ID cleanup).
//
// Runs the unchanged Pi 1.3.0 CLI (provider-references/integrated) with the pinned
// pi-cursor-sdk@0.5.3 extension and the scenario-driven fake @cursor/sdk for every
// reference scenario in scenarios/. Each scenario runs twice in a fresh temporary
// root and must produce identical normalized C04 session entries and fake-SDK
// counters.
//
//   node run-reference.mjs            check mode (default): compare with reference-evidence.json
//   node run-reference.mjs --write    regenerate reference-evidence.json
//   node run-reference.mjs --keep DIR also keep raw stdout/stderr/fake logs
//   node run-reference.mjs --only NAME run one scenario
//
// Never touches ~/.pi or ~/.cursor: HOME, PI_CODING_AGENT_DIR and every fake path
// live in a temporary directory created under $HOME and removed afterwards.
import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { C04_TYPES, normalizeC04Entries } from "./normalize.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const SCENARIO_DIR = join(HERE, "scenarios");
const EVIDENCE_PATH = join(HERE, "reference-evidence.json");
// Lane worktrees sit directly under the Projects directory, so references are five
// levels above this file. RPI_PROVIDER_REFERENCES overrides that location.
const REF_ROOT = resolve(process.env.RPI_PROVIDER_REFERENCES ?? join(HERE, "../../../../../provider-references"), "integrated");
const PI_DIR = join(REF_ROOT, "node_modules/@earendil-works/pi-coding-agent");
const PI_CLI = join(PI_DIR, "dist/bundle/cli.js");
const PI_CURSOR_SDK_DIR = join(REF_ROOT, "node_modules/pi-cursor-sdk");
const REGISTER = join(HERE, "fake-sdk/register.mjs");
const TIMEOUT_MS = 180_000;

const args = process.argv.slice(2);
const WRITE = args.includes("--write");
const KEEP = args.includes("--keep") ? resolve(args[args.indexOf("--keep") + 1]) : undefined;
const ONLY = args.includes("--only") ? args[args.indexOf("--only") + 1] : undefined;

const sha256 = (text) => createHash("sha256").update(text).digest("hex");
const sha256File = (path) => sha256(readFileSync(path));

const UUID = /[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/g;
const ISO = /\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z/g;
const SESSION_TIME = /\d{4}-\d{2}-\d{2}T\d{2}-\d{2}-\d{2}-\d{3}Z/g;

// Normalization applied ONLY to the Pi JSON event stream (never to the C04
// session-entry comparison, which has its own document in normalize.mjs).
function normalizeJsonLine(line, tmpRoot) {
	const value = JSON.parse(line);
	const walk = (node, key) => {
		if (Array.isArray(node)) return node.map((item) => walk(item));
		if (node && typeof node === "object") {
			const out = {};
			for (const [name, child] of Object.entries(node)) out[name] = walk(child, name);
			return out;
		}
		if (typeof node === "number" && (key === "timestamp" || key === "durationMs" || key === "executionTime")) return 0;
		if (typeof node === "string") {
			if (key === "id" || key === "parentId" || key === "anchorId") {
				if (/^[0-9a-f]{8}$/.test(node)) return "<entry>";
			}
			// Opaque hashes derived from Pi's random entry ids, timestamps or the
			// process system prompt cannot be equal across fresh runs.
			if (key === "branchPathHash" || key === "systemHash" || key === "contextFingerprint") return "<hash>";
			return node
				.split(tmpRoot).join("<root>")
				.replace(UUID, "<uuid>")
				.replace(ISO, "<iso>")
				.replace(SESSION_TIME, "<session-time>")
				.replace(/cursor-replay-\d+-\d+-/g, "cursor-replay-<n>-<n>-")
				.replace(/cursor-pi-bridge-[0-9a-f]{32}-t\d+/g, "cursor-pi-bridge-<run>-t<n>")
				.replace(/pi-sessions\/[0-9a-f]{32}/g, "pi-sessions/<store>");
		}
		return node;
	};
	return JSON.stringify(walk(value));
}

function readJsonLines(text) {
	return text.split("\n").filter((line) => line.length > 0);
}

function aggregateFakeLog(logText) {
	const records = readJsonLines(logText).map((line) => JSON.parse(line));
	const totals = { sdkCalls: {}, runs: {}, bridgeToolExecutions: 0, toolExecutions: {}, nativeToolCalls: {}, violations: 0 };
	for (const record of records) {
		if (record.kind !== "process_counters") continue;
		const counters = record.counters;
		for (const [api, n] of Object.entries(counters.sdkCalls)) totals.sdkCalls[api] = (totals.sdkCalls[api] ?? 0) + n;
		for (const [status, n] of Object.entries(counters.runs)) {
			if (status === "cancel_requested") continue;
			totals.runs[status] = (totals.runs[status] ?? 0) + n;
		}
		for (const [name, n] of Object.entries(counters.toolExecutions)) {
			totals.toolExecutions[name] = (totals.toolExecutions[name] ?? 0) + n;
			totals.bridgeToolExecutions += n;
		}
		for (const [name, n] of Object.entries(counters.nativeToolCalls)) totals.nativeToolCalls[name] = (totals.nativeToolCalls[name] ?? 0) + n;
		totals.violations += counters.violations;
	}
	return { records, totals };
}

function summarizePi(stdoutText, tmpRoot) {
	const events = readJsonLines(stdoutText).map((line) => JSON.parse(line));
	const counts = {};
	const piToolExecutions = {};
	const entries = [];
	for (const event of events) {
		counts[event.type] = (counts[event.type] ?? 0) + 1;
		if (event.type === "tool_execution_end") piToolExecutions[event.toolName] = (piToolExecutions[event.toolName] ?? 0) + 1;
		if (event.type === "entry_appended" && event.entry && C04_TYPES.has(event.entry.customType)) {
			entries.push({ customType: event.entry.customType, data: event.entry.data });
		}
	}
	const normalized = readJsonLines(stdoutText).map((line) => normalizeJsonLine(line, tmpRoot));
	return {
		eventCounts: counts,
		piToolExecutions,
		entries: normalizeC04Entries(entries, tmpRoot),
		normalizedSha256: sha256(`${normalized.join("\n")}\n`),
		eventCountTotal: events.length,
	};
}

function runPi(scenario, scenarioPath, tmpRoot, invocation) {
	const workspace = join(tmpRoot, "workspace");
	const home = join(tmpRoot, "home");
	const agentDir = join(tmpRoot, "agent");
	const stateHome = join(tmpRoot, "cursor-home");
	const tmpDir = join(tmpRoot, "tmp");
	const logPath = join(tmpRoot, "fake.jsonl");
	const extensionArgs = [];
	for (const extension of invocation.extensions ?? []) extensionArgs.push("-e", join(HERE, extension));
	const argv = [
		PI_CLI,
		"-ne",
		"-e",
		PI_CURSOR_SDK_DIR,
		...extensionArgs,
		"--provider",
		"cursor",
		"--model",
		invocation.model ?? scenario.run.model,
		"--mode",
		"json",
		"--session-dir",
		join(tmpRoot, "sessions"),
		...(invocation.args ?? []),
		invocation.prompt,
		...(invocation.followUps ?? []),
	];
	const env = {
		PATH: `${dirname(process.execPath)}:/usr/local/bin:/usr/bin:/bin`,
		HOME: home,
		TMPDIR: tmpDir,
		LC_ALL: "en_US.UTF-8",
		TERM: "dumb",
		PI_CODING_AGENT_DIR: agentDir,
		CURSOR_API_KEY: process.env.CURSOR_API_KEY ?? "offline-fixture",
		NODE_OPTIONS: `--import ${REGISTER}`,
		RPI_CURSOR_FAKE_SCENARIO: scenarioPath,
		RPI_CURSOR_FAKE_LOG: logPath,
		RPI_CURSOR_FAKE_LOG_ROOT: tmpRoot,
		RPI_CURSOR_FAKE_HOME: stateHome,
		...(scenario.env ?? {}),
	};
	return new Promise((resolvePromise, reject) => {
		const child = spawn(process.execPath, argv, { cwd: workspace, env, stdio: ["ignore", "pipe", "pipe"] });
		let stdout = "";
		let stderr = "";
		child.stdout.setEncoding("utf8").on("data", (chunk) => { stdout += chunk; });
		child.stderr.setEncoding("utf8").on("data", (chunk) => { stderr += chunk; });
		const timer = setTimeout(() => child.kill("SIGKILL"), TIMEOUT_MS);
		child.on("error", reject);
		child.on("close", (code, signal) => {
			clearTimeout(timer);
			resolvePromise({ code, signal, stdout, stderr });
		});
	});
}

function normalizeStderr(text, tmpRoot) {
	return text.split(tmpRoot).join("<root>").replace(UUID, "<uuid>").replace(ISO, "<iso>");
}

function compareExpect(expect, observed) {
	const problems = [];
	const check = (label, want, got) => {
		if (JSON.stringify(want) !== JSON.stringify(got)) problems.push(`${label}: expected ${JSON.stringify(want)}, observed ${JSON.stringify(got)}`);
	};
	check("exitCode", expect.exitCode, observed.exitCode);
	check("sdkCalls", expect.sdkCalls ?? {}, observed.sdkCalls);
	check("runs", expect.runs ?? {}, observed.runs);
	check("bridgeToolExecutions", expect.bridgeToolExecutions ?? 0, observed.bridgeToolExecutions);
	check("toolExecutions", expect.toolExecutions ?? {}, observed.toolExecutions);
	check("nativeToolCalls", expect.nativeToolCalls ?? {}, observed.nativeToolCalls);
	check("violations", expect.violations ?? 0, observed.violations);
	if (expect.piToolExecutions !== undefined) check("piToolExecutions", expect.piToolExecutions, observed.piToolExecutions);
	return problems;
}

function sortObject(object) {
	return Object.fromEntries(Object.entries(object).sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0)));
}

function invocationsOf(scenario) {
	if (Array.isArray(scenario.run.invocations)) return scenario.run.invocations;
	return [{ prompt: scenario.run.prompt, followUps: scenario.run.followUps ?? [], extensions: scenario.run.extensions ?? [], args: scenario.run.args ?? [] }];
}

async function runScenario(file) {
	const scenarioPath = join(SCENARIO_DIR, file);
	const scenario = JSON.parse(readFileSync(scenarioPath, "utf8"));
	const attempts = [];
	for (let attempt = 1; attempt <= 2; attempt++) {
		const tmpRoot = mkdtempSync(join(homedir(), ".rpi-c-tree-ref-"));
		for (const dir of ["workspace", "home", "agent", "cursor-home", "tmp", "sessions"]) {
			mkdirSync(join(tmpRoot, dir), { recursive: true, mode: 0o700 });
		}
		// User-scoped Cursor config, read by the retained adapter for subagents / settings.
		if (scenario.config) writeFileSync(join(tmpRoot, "agent", "cursor-sdk.json"), `${JSON.stringify(scenario.config, null, 2)}\n`);
		const logPath = join(tmpRoot, "fake.jsonl");
		const attemptsEntries = [];
		let exitCode = 0;
		let stderr = "";
		let stdout = "";
		for (const invocation of invocationsOf(scenario)) {
			const run = await runPi(scenario, scenarioPath, tmpRoot, invocation);
			exitCode = run.code ?? exitCode;
			stdout += run.stdout;
			stderr += run.stderr;
			attemptsEntries.push(...summarizePi(run.stdout, tmpRoot).entries);
			if (run.code !== 0) break;
		}
		const fakeLog = existsSync(logPath) ? readFileSync(logPath, "utf8") : "";
		const { records, totals } = aggregateFakeLog(fakeLog);
		const pi = summarizePi(stdout, tmpRoot);
		const observed = {
			exitCode,
			sdkCalls: sortObject(totals.sdkCalls),
			runs: sortObject(totals.runs),
			bridgeToolExecutions: totals.bridgeToolExecutions,
			toolExecutions: sortObject(totals.toolExecutions),
			nativeToolCalls: sortObject(totals.nativeToolCalls),
			violations: totals.violations,
			piToolExecutions: sortObject(pi.piToolExecutions),
		};
		const normalizedLog = fakeLog.split(tmpRoot).join("<root>");
		if (KEEP) {
			const dir = join(KEEP, scenario.name, `run-${attempt}`);
			mkdirSync(dir, { recursive: true });
			writeFileSync(join(dir, "stdout.jsonl"), stdout);
			writeFileSync(join(dir, "stderr.txt"), normalizeStderr(stderr, tmpRoot));
			writeFileSync(join(dir, "fake.jsonl"), normalizedLog);
			writeFileSync(join(dir, "entries.json"), `${JSON.stringify(attemptsEntries, null, 2)}\n`);
		}
		attempts.push({
			observed,
			entries: attemptsEntries,
			fakeLogSha256: sha256(normalizedLog),
			fakeLogRecords: records.length,
			normalizedPiSha256: pi.normalizedSha256,
			piEventCounts: sortObject(pi.eventCounts),
			piEventTotal: pi.eventCountTotal,
			stderrLines: stderr.split("\n").filter(Boolean).length,
		});
		rmSync(tmpRoot, { recursive: true, force: true });
	}
	const [first, second] = attempts;
	const determinism = {
		fakeLogIdentical: first.fakeLogSha256 === second.fakeLogSha256,
		normalizedPiIdentical: first.normalizedPiSha256 === second.normalizedPiSha256,
		entriesIdentical: JSON.stringify(first.entries) === JSON.stringify(second.entries),
	};
	const problems = compareExpect(scenario.expect, first.observed);
	if (!determinism.fakeLogIdentical) problems.push("fake log differs between two runs");
	if (!determinism.normalizedPiIdentical) problems.push("normalized Pi JSON differs between two runs");
	if (!determinism.entriesIdentical) problems.push("normalized C04 entries differ between two runs");
	if (JSON.stringify(first.observed) !== JSON.stringify(second.observed)) problems.push("observed counts differ between two runs");
	return {
		name: scenario.name,
		scenarioFile: `tests/c-tree/scenarios/${file}`,
		scenarioSha256: sha256File(scenarioPath),
		run: { model: scenario.run.model, invocations: invocationsOf(scenario).map(({ prompt, followUps, args, extensions }) => ({ prompt, followUps: followUps ?? [], args: args ?? [], extensions: extensions ?? [] })) },
		expect: scenario.expect,
		observed: first.observed,
		c04Entries: first.entries,
		piEventCounts: first.piEventCounts,
		piEventTotal: first.piEventTotal,
		fakeLogRecords: first.fakeLogRecords,
		fakeLogSha256: first.fakeLogSha256,
		normalizedPiSha256: first.normalizedPiSha256,
		stderrLines: first.stderrLines,
		determinism,
		problems,
		status: problems.length === 0 ? "pass" : "fail",
	};
}

function referenceProvenance() {
	const files = {
		"@cursor/sdk/package.json": join(REF_ROOT, "node_modules/@cursor/sdk/package.json"),
		"@earendil-works/pi-coding-agent/package.json": join(PI_DIR, "package.json"),
		"@earendil-works/pi-coding-agent/dist/bundle/cli.js": PI_CLI,
		"pi-cursor-sdk/package.json": join(PI_CURSOR_SDK_DIR, "package.json"),
		"pi-cursor-sdk/dist/index.js": join(PI_CURSOR_SDK_DIR, "dist/index.js"),
		"integrated/package-lock.json": join(REF_ROOT, "package-lock.json"),
	};
	const out = {};
	for (const [name, path] of Object.entries(files)) {
		if (!existsSync(path)) throw new Error(`reference file missing: ${path}`);
		out[name] = sha256File(path);
	}
	return {
		files: out,
		piVersion: JSON.parse(readFileSync(join(PI_DIR, "package.json"), "utf8")).version,
		cursorSdkVersion: JSON.parse(readFileSync(join(REF_ROOT, "node_modules/@cursor/sdk/package.json"), "utf8")).version,
		piCursorSdkVersion: JSON.parse(readFileSync(join(PI_CURSOR_SDK_DIR, "package.json"), "utf8")).version,
	};
}

const C04_UPSTREAM = [
	"src/cursor-session-agent-lineage.ts",
	"src/cursor-session-agent-resume.ts",
	"src/cursor-session-agent-cleanup.ts",
	"src/cursor-session-store.ts",
	"src/cursor-session-scope.ts",
	"src/cursor-session-compaction-prep.ts",
	"src/cursor-provider-turn-summary.ts",
];

function upstreamProvenance() {
	// upstream.json is the committed pin of every retained upstream file; the C04
	// modules are the source of the behavior exercised here. Remove the placeholder
	// entry that does not exist upstream.
	const upstream = JSON.parse(readFileSync(join(HERE, "../../upstream.json"), "utf8"));
	const files = {};
	for (const name of C04_UPSTREAM) {
		if (upstream.files[name]) files[name] = upstream.files[name];
	}
	return { repository: upstream.repository, revision: upstream.revision, sdk: upstream.sdk, files };
}

function fakeProvenance() {
	const files = ["fake-sdk/sdk.mjs", "fake-sdk/hooks.mjs", "fake-sdk/register.mjs", "fake-sdk/sqlite.mjs", "fake-sdk/fixtures/echo-extension.mjs", "normalize.mjs", "run-reference.mjs"];
	return Object.fromEntries(files.map((name) => [name, sha256File(join(HERE, name))]));
}

export const NORMALIZATION = {
	c04Entries:
		"sessionId-><session-id>, sessionFile-><session-file>, scopeKey-><scope-key>, cwd-><cwd>, timestamps-><iso>, " +
		"branchPathHash-><hash>, contextFingerprint-><fingerprint> (Pi carries its system prompt as a leading session message; the native port carries it in context.systemPrompt, so the opaque hash and message count are not comparable), " +
		"storeIdentity.stateRoot-><store-root>, poolKey environment components (scope/cwd/16-hex store hash) normalized. " +
		"agentId, cleanup action/phase and candidate/protected/deleted/failed agent ids are compared exactly.",
	piJsonStream:
		"Each JSON line is re-serialized with timestamp, durationMs, executionTime set to 0; UUIDs to <uuid>; ISO-8601 timestamps to <iso>; " +
		"session-file timestamps to <session-time>; Pi's random 8-hex entry id/parentId/anchorId to <entry>; " +
		"cursor-replay-<ms>-<n>- to cursor-replay-<n>-<n>-; cursor-pi-bridge run IDs to <run>; pi-sessions/<32-hex store hash> to pi-sessions/<store>; the temporary root to <root>.",
	fakeLog: "No normalization: the fake writes no clocks, PIDs or random IDs; only the temporary root is replaced by <root>.",
};

async function main() {
	if (!existsSync(PI_CLI)) throw new Error(`Pi CLI missing at ${PI_CLI}`);
	const nodeVersion = process.versions.node;
	if (Number(nodeVersion.split(".")[0]) < 24) throw new Error(`Node 24+ required for the reference runs, found ${nodeVersion}`);
	const files = readdirSync(SCENARIO_DIR)
		.filter((name) => name.endsWith(".json") && !JSON.parse(readFileSync(join(SCENARIO_DIR, name), "utf8")).rpiOnly)
		.sort();
	const selected = ONLY ? files.filter((name) => name === `${ONLY}.json`) : files;
	if (selected.length === 0) throw new Error("no scenarios selected");
	const results = [];
	for (const file of selected) {
		process.stdout.write(`running ${file} ...\n`);
		results.push(await runScenario(file));
	}
	const evidence = {
		status: results.every((result) => result.status === "pass") ? "pass" : "fail",
		schema: "rpi-cursor-c04-reference-evidence/1",
		purpose: "Offline scenario runs of the unchanged Pi 1.3.0 reference and the pinned pi-cursor-sdk@0.5.3 adapter against the fake @cursor/sdk@1.0.37. No Cursor network calls.",
		runtime: { node: nodeVersion, platform: process.platform, arch: process.arch },
		toolchains: { node: nodeVersion, rustMinimum: "1.78.0", rustDefault: "1.95.0", sdk: "1.0.37" },
		reference: referenceProvenance(),
		upstream: upstreamProvenance(),
		fake: fakeProvenance(),
		normalization: NORMALIZATION,
		comparison: "Whole-value comparison of the ordered C04 session entries (cursor-sdk-agent-lineage/resume/cleanup) and of the fake-SDK counters for every reference scenario. rpi-only scenarios are checked by run-rpi.mjs against recorded expectations.",
		scenarios: results,
		counts: {
			referenceScenarios: results.length,
			rpiOnly: readdirSync(SCENARIO_DIR).filter((name) => name.endsWith(".json") && JSON.parse(readFileSync(join(SCENARIO_DIR, name), "utf8")).rpiOnly).length,
			c04Entries: results.reduce((total, result) => total + result.c04Entries.length, 0),
		},
		notTested: [
			"Pi's built-in /compact command is not dispatched in --print/--mode json, so compaction is exercised rpi-only by run-rpi.mjs.",
			"Interactive/TUI and RPC modes are not exercised.",
			"Images, cloud agents, tools/MCP execution and the cursor-usage command are outside C04.",
		],
		commands: [
			"node tests/c-tree/run-reference.mjs        (check mode)",
			"node tests/c-tree/run-reference.mjs --write",
		],
	};
	const text = `${JSON.stringify(evidence, null, 2)}\n`;
	if (WRITE) {
		writeFileSync(EVIDENCE_PATH, text);
		process.stdout.write(`wrote ${EVIDENCE_PATH} (${evidence.status})\n`);
	} else {
		if (!existsSync(EVIDENCE_PATH)) throw new Error("reference-evidence.json missing; run with --write");
		const committed = JSON.parse(readFileSync(EVIDENCE_PATH, "utf8"));
		const regenerated = JSON.parse(text);
		// Compare everything except the host runtime triple, so a changed
		// normalization rule, count or provenance also fails the check.
		const diffs = [];
		for (const key of new Set([...Object.keys(committed), ...Object.keys(regenerated)])) {
			if (key === "runtime") continue;
			if (JSON.stringify(committed[key]) !== JSON.stringify(regenerated[key])) diffs.push(key);
		}
		if (diffs.length > 0) {
			process.stdout.write(`reference-evidence.json is stale: ${diffs.join(", ")}\n`);
			process.exitCode = 1;
		}
	}
	for (const result of results) {
		process.stdout.write(`${result.status.toUpperCase()} ${result.name}: ${JSON.stringify(result.observed.sdkCalls)} entries=${result.c04Entries.length}\n`);
		for (const problem of result.problems) process.stdout.write(`  - ${problem}\n`);
		if (result.status !== "pass") process.exitCode = 1;
	}
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
	main().catch((error) => {
		process.stderr.write(`${error?.stack ?? error}\n`);
		process.exit(1);
	});
}
