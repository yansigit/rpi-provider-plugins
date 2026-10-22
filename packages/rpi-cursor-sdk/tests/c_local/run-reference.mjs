#!/usr/bin/env node
// Runs the unchanged Pi 1.3.0 CLI (provider-references/integrated) against the
// fake @cursor/sdk for every scenario in scenarios/, twice each, and checks:
//   - exit code, SDK call counts, run outcomes, bridge/native/Pi tool counts and
//     fake-violation counts equal each scenario's `expect` block exactly;
//   - the fake log and the normalized Pi JSON stream are byte-identical across the
//     two runs of a scenario.
//
//   node run-reference.mjs            check mode (default): compare with evidence.json
//   node run-reference.mjs --write    regenerate evidence.json
//   node run-reference.mjs --keep DIR also keep each run's raw stdout/stderr/fake log under DIR
//   node run-reference.mjs --only NAME  run one scenario
//
// Never touches ~/.pi or ~/.cursor: HOME, PI_CODING_AGENT_DIR and every fake path are
// inside a temporary directory created under $HOME and removed afterwards.
import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const PACKAGE_ROOT = resolve(HERE, "../..");
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

// Normalization applied ONLY to the Pi JSON event stream (never to the fake log):
// wall-clock values, random session/turn IDs, and the temporary root directory.
// Each replacement is listed in evidence.json under normalization.
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
			return node
				.split(tmpRoot).join("<root>")
				.replace(UUID, "<uuid>")
				.replace(ISO, "<iso>")
				.replace(SESSION_TIME, "<session-time>")
				.replace(/cursor-replay-\d+-\d+-/g, "cursor-replay-<n>-<n>-")
				.replace(/cursor-pi-bridge-[0-9a-f]{32}-t\d+/g, "cursor-pi-bridge-<run>-t<n>")
				.replace(/pi-sessions\/[0-9a-f]{32}/g, "pi-sessions/<scope>");
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
		if (record.kind === "process_counters") {
			const c = record.counters;
			for (const [api, n] of Object.entries(c.sdkCalls)) totals.sdkCalls[api] = (totals.sdkCalls[api] ?? 0) + n;
			for (const [status, n] of Object.entries(c.runs)) {
				if (status === "cancel_requested") continue;
				totals.runs[status] = (totals.runs[status] ?? 0) + n;
			}
			for (const [name, n] of Object.entries(c.toolExecutions)) {
				totals.toolExecutions[name] = (totals.toolExecutions[name] ?? 0) + n;
				totals.bridgeToolExecutions += n;
			}
			for (const [name, n] of Object.entries(c.nativeToolCalls)) totals.nativeToolCalls[name] = (totals.nativeToolCalls[name] ?? 0) + n;
			totals.violations += c.violations;
		}
	}
	return { records, totals };
}

function summarizePi(stdoutText, tmpRoot) {
	const events = readJsonLines(stdoutText).map((line) => JSON.parse(line));
	const counts = {};
	const piToolExecutions = {};
	for (const event of events) {
		counts[event.type] = (counts[event.type] ?? 0) + 1;
		if (event.type === "tool_execution_end") piToolExecutions[event.toolName] = (piToolExecutions[event.toolName] ?? 0) + 1;
	}
	const normalized = readJsonLines(stdoutText).map((line) => normalizeJsonLine(line, tmpRoot));
	return { eventCounts: counts, piToolExecutions, normalizedSha256: sha256(`${normalized.join("\n")}\n`), eventCountTotal: events.length };
}

function runPi(scenario, scenarioPath) {
	const tmpRoot = mkdtempSync(join(homedir(), ".rpi-fake-sdk-"));
	const workspace = join(tmpRoot, "workspace");
	const home = join(tmpRoot, "home");
	const agentDir = join(tmpRoot, "agent");
	const stateHome = join(tmpRoot, "cursor-home");
	const tmpDir = join(tmpRoot, "tmp");
	const logPath = join(tmpRoot, "fake.jsonl");
	for (const dir of [workspace, home, agentDir, stateHome, tmpDir]) mkdirSync(dir, { recursive: true, mode: 0o700 });
	// User-scoped Cursor config, read by the retained adapter for subagents / settings.
	if (scenario.config) writeFileSync(join(agentDir, "cursor-sdk.json"), `${JSON.stringify(scenario.config, null, 2)}\n`);
	const extensionArgs = [];
	for (const extension of scenario.run.extensions ?? []) extensionArgs.push("-e", join(HERE, extension));
	const argv = [
		PI_CLI,
		"-ne",
		"-e",
		PI_CURSOR_SDK_DIR,
		...extensionArgs,
		"--provider",
		"cursor",
		"--model",
		scenario.run.model,
		"--mode",
		"json",
		"--session-dir",
		join(tmpRoot, "sessions"),
		...(scenario.run.args ?? []),
		scenario.run.prompt,
		...(scenario.run.followUps ?? []),
	];
	const env = {
		// Node 24 first (the reference runtime), then the system directories Pi needs for git and shells.
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
			const fakeLog = existsSync(logPath) ? readFileSync(logPath, "utf8") : "";
			resolvePromise({ code, signal, stdout, stderr, fakeLog, tmpRoot, logPath, cleanup: () => rmSync(tmpRoot, { recursive: true, force: true }) });
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

async function runScenario(file) {
	const scenarioPath = join(SCENARIO_DIR, file);
	const scenario = JSON.parse(readFileSync(scenarioPath, "utf8"));
	const attempts = [];
	for (let attempt = 1; attempt <= 2; attempt++) {
		const run = await runPi(scenario, scenarioPath);
		const { records, totals } = aggregateFakeLog(run.fakeLog);
		const pi = summarizePi(run.stdout, run.tmpRoot);
		const observed = {
			exitCode: run.code,
			sdkCalls: sortObject(totals.sdkCalls),
			runs: sortObject(totals.runs),
			bridgeToolExecutions: totals.bridgeToolExecutions,
			toolExecutions: sortObject(totals.toolExecutions),
			nativeToolCalls: sortObject(totals.nativeToolCalls),
			violations: totals.violations,
			piToolExecutions: sortObject(pi.piToolExecutions),
		};
		const fakeLogSha = sha256(run.fakeLog.split(run.tmpRoot).join("<root>"));
		const normalizedLog = run.fakeLog.split(run.tmpRoot).join("<root>");
		if (KEEP) {
			const dir = join(KEEP, scenario.name, `run-${attempt}`);
			mkdirSync(dir, { recursive: true });
			writeFileSync(join(dir, "stdout.jsonl"), run.stdout);
			writeFileSync(join(dir, "stderr.txt"), normalizeStderr(run.stderr, run.tmpRoot));
			writeFileSync(join(dir, "fake.jsonl"), normalizedLog);
		}
		attempts.push({
			observed,
			fakeLogSha256: fakeLogSha,
			fakeLogRecords: records.length,
			normalizedPiSha256: pi.normalizedSha256,
			piEventCounts: sortObject(pi.eventCounts),
			piEventTotal: pi.eventCountTotal,
			stderrLines: run.stderr.split("\n").filter(Boolean).length,
			normalizedLog,
		});
		run.cleanup();
	}
	const [first, second] = attempts;
	const determinism = {
		fakeLogIdentical: first.fakeLogSha256 === second.fakeLogSha256,
		normalizedPiIdentical: first.normalizedPiSha256 === second.normalizedPiSha256,
	};
	const problems = compareExpect(scenario.expect, first.observed);
	if (!determinism.fakeLogIdentical) problems.push("fake log differs between two runs");
	if (!determinism.normalizedPiIdentical) problems.push("normalized Pi JSON differs between two runs");
	if (JSON.stringify(first.observed) !== JSON.stringify(second.observed)) problems.push("observed counts differ between two runs");
	return {
		name: scenario.name,
		scenarioFile: `tests/c_local/scenarios/${file}`,
		scenarioSha256: sha256File(scenarioPath),
		run: { model: scenario.run.model, prompt: scenario.run.prompt, followUps: scenario.run.followUps ?? [], extensions: scenario.run.extensions ?? [], args: scenario.run.args ?? [] },
		expect: scenario.expect,
		observed: first.observed,
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
	const piVersion = JSON.parse(readFileSync(join(PI_DIR, "package.json"), "utf8")).version;
	const sdkVersion = JSON.parse(readFileSync(join(REF_ROOT, "node_modules/@cursor/sdk/package.json"), "utf8")).version;
	return { files: out, piVersion, cursorSdkVersion: sdkVersion, piCursorSdkVersion: JSON.parse(readFileSync(join(PI_CURSOR_SDK_DIR, "package.json"), "utf8")).version };
}

function fakeProvenance() {
	const files = ["fake-sdk/sdk.mjs", "fake-sdk/hooks.mjs", "fake-sdk/register.mjs", "fake-sdk/sqlite.mjs", "fake-sdk/fixtures/echo-extension.mjs", "run-reference.mjs"];
	return Object.fromEntries(files.map((name) => [name, sha256File(join(HERE, name))]));
}

async function main() {
	if (!existsSync(PI_CLI)) throw new Error(`Pi CLI missing at ${PI_CLI}`);
	const nodeVersion = process.versions.node;
	if (Number(nodeVersion.split(".")[0]) < 24) throw new Error(`Node 24+ required for the reference runs, found ${nodeVersion}`);
	const files = readdirSync(SCENARIO_DIR).filter((name) => name.endsWith(".json") && !JSON.parse(readFileSync(join(SCENARIO_DIR, name), "utf8")).rpiOnly).sort();
	const selected = ONLY ? files.filter((name) => name === `${ONLY}.json`) : files;
	if (selected.length === 0) throw new Error("no scenarios selected");
	const results = [];
	for (const file of selected) {
		process.stdout.write(`running ${file} ...\n`);
		results.push(await runScenario(file));
	}
	const evidence = {
		status: results.every((result) => result.status === "pass") ? "pass" : "fail",
		schema: "rpi-cursor-c-local-reference-evidence/1",
		purpose: "Offline scenario runs of the unchanged Pi 1.3.0 reference against the fake @cursor/sdk@1.0.37. No Cursor network calls.",
		runtime: { node: nodeVersion, platform: process.platform, arch: process.arch },
		reference: referenceProvenance(),
		fake: fakeProvenance(),
		normalization: {
			piJsonStream: "Each JSON line is re-serialized with timestamp, durationMs, executionTime set to 0; UUIDs to <uuid>; ISO-8601 timestamps to <iso>; session-file timestamps to <session-time>; Pi's random 8-hex entry id/parentId/anchorId to <entry>; cursor-replay-<ms>-<n>- to cursor-replay-<n>-<n>-; cursor-pi-bridge run IDs to <run>; pi-sessions/<32-hex store hash> to pi-sessions/<scope>; the temporary root to <root>.",
			fakeLog: "No normalization: the fake writes no clocks, PIDs or random IDs; only the temporary root is replaced by <root>.",
		},
		scenarios: results,
		notTested: [
			"Image prompts (Pi @image.png attachments) are not exercised.",
			"Turn-level errors (sendError/error), mid-run crashes and cloud billing are not exercised by these scenarios.",
			"Cancellation and concurrent owners are rpi-only checks (see tests/c_local/run-rpi.mjs); the Pi 1.3.0 print CLI cannot drive them deterministically.",
			"Interactive/TUI mode, RPC mode and the cursor-usage command are not exercised.",
		],
		commands: [
			"node tests/c_local/run-reference.mjs        (check mode)",
			"node tests/c_local/run-reference.mjs --write",
		],
	};
	const text = `${JSON.stringify(evidence, null, 2)}\n`;
	if (WRITE) {
		writeFileSync(EVIDENCE_PATH, text);
		process.stdout.write(`wrote ${EVIDENCE_PATH} (${evidence.status})\n`);
	} else {
		if (!existsSync(EVIDENCE_PATH)) throw new Error("evidence.json missing; run with --write");
		const committed = JSON.parse(readFileSync(EVIDENCE_PATH, "utf8"));
		const regenerated = JSON.parse(text);
		const diffs = [];
		if (committed.status !== regenerated.status) diffs.push("status");
		for (const result of regenerated.scenarios) {
			const old = committed.scenarios?.find((candidate) => candidate.name === result.name);
			if (!old) diffs.push(`scenario ${result.name} missing from evidence.json`);
			else if (JSON.stringify(old) !== JSON.stringify(result)) diffs.push(`scenario ${result.name} differs from evidence.json`);
		}
		if (JSON.stringify(committed.reference) !== JSON.stringify(regenerated.reference)) diffs.push("reference provenance");
		if (JSON.stringify(committed.fake) !== JSON.stringify(regenerated.fake)) diffs.push("fake provenance");
		for (const result of results) {
			process.stdout.write(`${result.status.toUpperCase()} ${result.name}: ${JSON.stringify(result.observed.sdkCalls)} runs=${JSON.stringify(result.observed.runs)} bridge=${result.observed.bridgeToolExecutions}\n`);
			for (const problem of result.problems) process.stdout.write(`  - ${problem}\n`);
		}
		if (diffs.length > 0) {
			process.stdout.write(`evidence.json is stale: ${diffs.join(", ")}\n`);
			process.exitCode = 1;
		}
	}
	for (const result of results) {
		if (result.status !== "pass") process.exitCode = 1;
	}
}

main().catch((error) => {
	process.stderr.write(`${error?.stack ?? error}\n`);
	process.exitCode = 1;
});
