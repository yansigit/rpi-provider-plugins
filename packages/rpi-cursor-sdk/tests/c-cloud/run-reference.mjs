#!/usr/bin/env node
// Reference half of the ledger C06 cloud differential.
//
// Runs every scenario through the UNCHANGED Pi 1.3.0 CLI with the pinned
// `pi-cursor-sdk@0.5.3` adapter and the scenario-driven fake `@cursor/sdk`, then
// records the complete cloud SDK interactions, the ordered `cursor-cloud-lifecycle`
// session entries and the durable journal entries. The wall clock and the SDK agent
// ids are fixed (RPI_CURSOR_FAKE_CLOCK + the fake's deterministic UUIDs), so the
// output is byte-identical across the two runs.
//
//   node tests/c-cloud/run-reference.mjs             check mode
//   node tests/c-cloud/run-reference.mjs --write     regenerate reference-evidence.json
//   node tests/c-cloud/run-reference.mjs --only NAME run one scenario
//   node tests/c-cloud/run-reference.mjs --keep DIR  keep raw runs under DIR
//
// Never touches ~/.pi, ~/.rpi or the network: HOME, PI_CODING_AGENT_DIR, TMPDIR and
// every fake path live inside a temporary directory under $HOME and are removed.
import { spawn } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import {
	FROZEN_CLOCK,
	SCHEMA,
	fakeRecords,
	lifecycleFromPiEvents,
	observedFromRecords,
	readJournals,
	sendErrorFromPiEvents,
	sha256,
	sha256File,
	writeEvidence,
} from "./lib.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const HOME_REF = resolve(HERE, "../../../../..");
const REF_ROOT = resolve(process.env.RPI_PROVIDER_REFERENCES ?? join(HOME_REF, "provider-references"), "integrated");
const PI_DIR = join(REF_ROOT, "node_modules/@earendil-works/pi-coding-agent");
const PI_CLI = join(PI_DIR, "dist/bundle/cli.js");
const PINNED_ADAPTER = join(REF_ROOT, "node_modules/pi-cursor-sdk");
const REGISTER = join(HERE, "fake-sdk/register.mjs");
const SCENARIO_DIR = join(HERE, "scenarios");
const EVIDENCE_PATH = join(HERE, "reference-evidence.json");
const TIMEOUT_MS = 180_000;

const args = process.argv.slice(2);
const WRITE = args.includes("--write");
const KEEP = args.includes("--keep") ? resolve(args[args.indexOf("--keep") + 1]) : undefined;
const ONLY = args.includes("--only") ? args[args.indexOf("--only") + 1] : undefined;

function runPi(scenario, scenarioPath) {
	const tmpRoot = mkdtempSync(join(homedir(), ".rpi-c-cloud-"));
	const workspace = join(tmpRoot, "workspace");
	const home = join(tmpRoot, "home");
	const agentDir = join(tmpRoot, "agent");
	const tmpDir = join(tmpRoot, "tmp");
	const sessions = join(tmpRoot, "sessions");
	const logPath = join(tmpRoot, "fake.jsonl");
	for (const dir of [workspace, home, agentDir, tmpDir, sessions]) mkdirSync(dir, { recursive: true });
	const argv = [
		PI_CLI, "-ne", "-e", PINNED_ADAPTER,
		"--provider", "cursor", "--model", scenario.run.model, "--mode", "json",
		"--session-dir", sessions,
		...(scenario.run.args ?? []),
		scenario.run.prompt,
		...(scenario.run.followUps ?? []),
	];
	const env = {
		PATH: `${dirname(process.execPath)}:/usr/local/bin:/usr/bin:/bin`,
		HOME: home, TMPDIR: tmpDir, LC_ALL: "en_US.UTF-8", TERM: "xterm-256color",
		PI_CODING_AGENT_DIR: agentDir, CURSOR_API_KEY: "fake",
		NODE_OPTIONS: `--import ${REGISTER}`,
		RPI_CURSOR_FAKE_SCENARIO: scenarioPath,
		RPI_CURSOR_FAKE_LOG: logPath,
		RPI_CURSOR_FAKE_LOG_ROOT: tmpRoot,
		RPI_CURSOR_FAKE_HOME: join(tmpRoot, "cursor-home"),
		RPI_CURSOR_FAKE_CLOCK: FROZEN_CLOCK,
	};
	return new Promise((resolvePromise, reject) => {
		const child = spawn(process.execPath, argv, { cwd: workspace, env, stdio: ["ignore", "pipe", "pipe"] });
		let stdout = "";
		let stderr = "";
		child.stdout.setEncoding("utf8").on("data", (chunk) => { stdout += chunk; });
		child.stderr.setEncoding("utf8").on("data", (chunk) => { stderr += chunk; });
		const timer = setTimeout(() => child.kill("SIGKILL"), TIMEOUT_MS);
		child.on("error", reject);
		child.on("close", (code) => {
			clearTimeout(timer);
			const fakeLog = existsSync(logPath) ? readFileSync(logPath, "utf8") : "";
			const journals = readJournals(sessions);
			resolvePromise({ code, stdout, stderr, fakeLog, journals, tmpRoot, cleanup: () => rmSync(tmpRoot, { recursive: true, force: true }) });
		});
	});
}

function normalizeStderr(text, tmpRoot) {
	return text
		.split(tmpRoot).join("<root>")
		.replace(/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/g, "<uuid>")
		.replace(/\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z/g, "<iso>");
}

async function runScenario(file) {
	const scenarioPath = join(SCENARIO_DIR, file);
	const scenario = JSON.parse(readFileSync(scenarioPath, "utf8"));
	const attempts = [];
	for (let attempt = 1; attempt <= 2; attempt++) {
		const run = await runPi(scenario, scenarioPath);
		try {
			const records = fakeRecords(run.fakeLog);
			const lifecycle = lifecycleFromPiEvents(run.stdout);
			const sendError = sendErrorFromPiEvents(run.stdout);
			attempts.push({
				observed: observedFromRecords(records, lifecycle, run.journals, sendError === undefined ? null : normalizeStderr(sendError, run.tmpRoot)),
				fakeLogRecords: records.length,
				stderr: normalizeStderr(run.stderr, run.tmpRoot),
				raw: { stdout: run.stdout, fakeLog: run.fakeLog },
			});
			if (KEEP) {
				const dir = join(KEEP, scenario.name, `run-${attempt}`);
				mkdirSync(dir, { recursive: true });
				writeFileSync(join(dir, "stdout.jsonl"), run.stdout);
				writeFileSync(join(dir, "fake.jsonl"), run.fakeLog);
				writeFileSync(join(dir, "stderr.txt"), normalizeStderr(run.stderr, run.tmpRoot));
			}
		} finally {
			run.cleanup();
		}
	}
	const [first, second] = attempts;
	const problems = [];
	if (JSON.stringify(first.observed) !== JSON.stringify(second.observed)) {
		problems.push("reference output differs between two runs");
	}
	if (scenario.expect?.sdkCalls) {
		const want = Object.fromEntries(Object.entries(scenario.expect.sdkCalls).sort());
		if (JSON.stringify(want) !== JSON.stringify(first.observed.sdkCalls)) {
			problems.push(`sdkCalls: expected ${JSON.stringify(want)}, observed ${JSON.stringify(first.observed.sdkCalls)}`);
		}
	}
	if (scenario.expect?.sendErrorIncludes !== undefined) {
		const error = first.observed.sendError ?? "";
		if (!error.includes(scenario.expect.sendErrorIncludes)) {
			problems.push(`sendError: expected to include ${JSON.stringify(scenario.expect.sendErrorIncludes)}, observed ${JSON.stringify(error)}`);
		}
	}
	if (scenario.expect?.noSendError && first.observed.sendError !== null) {
		problems.push(`sendError: expected none, observed ${JSON.stringify(first.observed.sendError)}`);
	}
	return {
		name: scenario.name,
		scenarioFile: `tests/c-cloud/scenarios/${file}`,
		scenarioSha256: sha256File(scenarioPath),
		observed: first.observed,
		fakeLogRecords: first.fakeLogRecords,
		stderrLines: first.stderr.split("\n").filter(Boolean).length,
		determinism: { reference: problems.length === 0 },
		problems,
		status: problems.length === 0 ? "pass" : "fail",
	};
}

function provenance() {
	const files = {
		"@cursor/sdk/package.json": join(REF_ROOT, "node_modules/@cursor/sdk/package.json"),
		"@earendil-works/pi-coding-agent/package.json": join(PI_DIR, "package.json"),
		"@earendil-works/pi-coding-agent/dist/bundle/cli.js": PI_CLI,
		"pi-cursor-sdk/package.json": join(PINNED_ADAPTER, "package.json"),
		"pi-cursor-sdk/dist/index.js": join(PINNED_ADAPTER, "dist/index.js"),
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
		piCursorSdkVersion: JSON.parse(readFileSync(join(PINNED_ADAPTER, "package.json"), "utf8")).version,
	};
}

function fakeProvenance() {
	const files = ["lib.mjs", "fake-sdk/sdk.mjs", "fake-sdk/hooks.mjs", "fake-sdk/register.mjs", "fake-sdk/sqlite.mjs", "run-reference.mjs"];
	return Object.fromEntries(files.map((name) => [name, sha256File(join(HERE, name))]));
}

async function main() {
	if (!existsSync(PI_CLI)) throw new Error(`Pi CLI missing at ${PI_CLI}`);
	const node = process.versions.node;
	if (Number(node.split(".")[0]) < 24) throw new Error(`Node 24+ required, found ${node}`);
	const files = readdirSync(SCENARIO_DIR).filter((name) => name.endsWith(".json")).sort();
	const selected = ONLY ? files.filter((name) => name === `${ONLY}.json`) : files;
	if (selected.length === 0) throw new Error("no scenarios selected");
	const results = [];
	for (const file of selected) {
		process.stdout.write(`running reference ${file} ...\n`);
		results.push(await runScenario(file));
	}
	const evidence = {
		status: results.every((result) => result.status === "pass") ? "pass" : "fail",
		schema: SCHEMA,
		role: "reference",
		purpose: "Offline cloud scenarios through the unchanged Pi 1.3.0 CLI, the pinned pi-cursor-sdk@0.5.3 adapter and the unchanged fake @cursor/sdk. No Cursor network access; wall clock and SDK ids are fixed.",
		runtime: { node, platform: process.platform, arch: process.arch },
		clock: FROZEN_CLOCK,
		reference: provenance(),
		upstream: { piCursorSdkCommit: "f337dc4", piVersion: "1.3.0", cursorSdkVersion: "1.0.37", piCursorSdkVersion: "0.5.3" },
		toolchains: { rustMinimum: "1.78.0", rustDefault: "1.95.0", node: "24.22.0" },
		scenarioCount: results.length,
		caseCount: results.length,
		normalization: {
			cloudCalls: "Agent.create/archive/delete/get/list and cloud agent.send records are kept whole; only agent.send message text and image bytes are dropped because prompt/image assembly is C01, not C06.",
			journal: "Whole durable entries are kept; only the per-run sessionId, sessionFile and anchorEntryId values are replaced by <session>/<file>/<anchor>.",
			lifecycle: "Complete entry data payloads from the cursor-cloud-lifecycle session entries.",
			clock: "RPI_CURSOR_FAKE_CLOCK freezes Date for both runtimes; fake SDK agent ids come from a counter.",
		},
		compared: ["sdkCalls", "cloudCalls", "lifecycle", "lifecycleCounts", "journal", "sendError"],
		files: fakeProvenance(),
		scenarios: results,
		notTested: [
			"Live Cursor cloud resources, billing and the real Cursor backend are never contacted.",
			"Prompt/image assembly is compared by the C01 prompt lane, not here; agent.send prompt text is projected out.",
			"Interactive/TUI cloud acknowledgement dialogs are not rendered; only the non-interactive flag path is exercised.",
		],
		commands: [
			"node tests/c-cloud/run-reference.mjs        (check mode)",
			"node tests/c-cloud/run-reference.mjs --write",
		],
	};
	writeEvidence(EVIDENCE_PATH, evidence, WRITE);
	for (const result of results) {
		process.stdout.write(`${result.status.toUpperCase()} ${result.name}: ${JSON.stringify(result.observed.sdkCalls)} lifecycle=${JSON.stringify(result.observed.lifecycleCounts)}\n`);
		for (const problem of result.problems) process.stdout.write(`  - ${problem}\n`);
		if (result.status !== "pass") process.exitCode = 1;
	}
}

main().catch((error) => {
	process.stderr.write(`${error?.stack ?? error}\n`);
	process.exitCode = 1;
});
