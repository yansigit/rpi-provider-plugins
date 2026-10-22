#!/usr/bin/env node
// Differential RPC runner: drives Pi 1.3.0 and rpi with the same scripted
// session and compares their normalized transcripts.
//
//   node tools/parity-rpc/run.mjs <scenario.json> --pi <pi cli.js> --rpi <rpi bin>
//        [--rpi-extension <lib> ...] [--pi-extension <path> ...]
//        [--out <dir>] [--work <dir>]
//
// Exit status: 0 transcripts identical, 1 transcripts differ, 2 run error.
import { spawn } from "node:child_process";
import http from "node:http";
import { mkdir, mkdtemp, readFile, realpath, writeFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import { homedir } from "node:os";
import path from "node:path";
import { StringDecoder } from "node:string_decoder";
import { fileURLToPath, pathToFileURL } from "node:url";
import { startFakeOpenAI } from "./fixtures/fake-openai.mjs";
import {
  answerFor,
  canonicalJson,
  compareNormalized,
  isDialogRequest,
  normalizeTranscript,
  NormalizeError,
} from "./normalize.mjs";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const PRELOAD = path.join(HERE, "preload.mjs");
const DEFAULT_STEP_TIMEOUT_MS = 30000;
const EXIT_TIMEOUT_MS = 15000;
const DEFAULT_SYSTEM_PROMPT = "Parity fixed system prompt.";
const DEFAULT_CHUNK_CHARS = 8;
const DIALOG_KINDS = new Set(["select", "confirm", "input"]);

export class RunnerError extends Error {
  constructor(code, message) {
    super(message);
    this.name = "RunnerError";
    this.code = code;
  }
}

const isObject = (value) => value !== null && typeof value === "object" && !Array.isArray(value);
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

export function validateScenario(scenario) {
  const fail = (message) => {
    throw new RunnerError("scenario", `invalid scenario: ${message}`);
  };
  if (!isObject(scenario)) fail("top level must be an object");
  if (typeof scenario.name !== "string" || scenario.name === "") fail("name is required");
  const provider = scenario.provider ?? {};
  if (!isObject(provider)) fail("provider must be an object");
  const replies = provider.replies;
  if (provider.chunkChars !== undefined && (!Number.isInteger(provider.chunkChars) || provider.chunkChars < 1)) {
    fail("provider.chunkChars must be a positive integer");
  }
  if (provider.chunkDelayMs !== undefined && (!Number.isInteger(provider.chunkDelayMs) || provider.chunkDelayMs < 0)) {
    fail("provider.chunkDelayMs must be a non-negative integer");
  }
  if (!Array.isArray(replies) || replies.length === 0 || replies.some((item) => typeof item !== "string")) {
    fail("provider.replies must be a non-empty array of strings");
  }
  if (scenario.args !== undefined && (!Array.isArray(scenario.args) || scenario.args.some((a) => typeof a !== "string"))) {
    fail("args must be an array of strings");
  }
  const timeoutMs = scenario.timeoutMs ?? DEFAULT_STEP_TIMEOUT_MS;
  if (!Number.isInteger(timeoutMs) || timeoutMs <= 0) fail("timeoutMs must be a positive integer");
  if (!Array.isArray(scenario.steps) || scenario.steps.length === 0) fail("steps must be a non-empty array");
  for (const [index, step] of scenario.steps.entries()) {
    if (!isObject(step) || (step.send === undefined && step.await === undefined)) {
      fail(`steps[${index}] needs send and/or await`);
    }
    if (step.send !== undefined && (!isObject(step.send) || typeof step.send.type !== "string")) {
      fail(`steps[${index}].send needs a string type`);
    }
    const awaits = step.await === undefined ? [] : [].concat(step.await);
    for (const spec of awaits) {
      if (!isObject(spec) || typeof spec.type !== "string") fail(`steps[${index}].await needs a string type`);
    }
  }
  const dialogs = scenario.dialogs ?? [];
  if (!Array.isArray(dialogs)) fail("dialogs must be an array");
  for (const [index, dialog] of dialogs.entries()) {
    if (!isObject(dialog) || !DIALOG_KINDS.has(dialog.kind)) {
      fail(`dialogs[${index}].kind must be select, confirm or input`);
    }
    if (dialog.cancel !== true && !isObject(dialog.answer)) {
      fail(`dialogs[${index}] needs an answer or cancel: true`);
    }
  }
  return {
    name: scenario.name,
    description: scenario.description ?? "",
    provider: {
      name: provider.name ?? "parity",
      model: provider.model ?? "parity-model",
      replies,
      chunkChars: provider.chunkChars ?? DEFAULT_CHUNK_CHARS,
      chunkDelayMs: provider.chunkDelayMs ?? 0,
    },
    systemPrompt: scenario.systemPrompt ?? DEFAULT_SYSTEM_PROMPT,
    args: scenario.args ?? [],
    timeoutMs,
    steps: scenario.steps,
    dialogs,
  };
}

function matcher(spec) {
  return (record) =>
    record?.type === spec.type && (spec.id === undefined || record.id === spec.id);
}

// Environment for a child: no inherited Pi/rpi state, no credentials, and a
// private HOME so ~/.pi and ~/.rpi are never read.
function childEnv(side, dirs) {
  const prefix = side === "pi" ? "PI" : "RPI";
  const env = {};
  for (const [key, value] of Object.entries(process.env)) {
    if (/^(PI|RPI)_/.test(key)) continue;
    if (/(_API_KEY|_AUTH_TOKEN|_ACCESS_TOKEN|_TOKEN|_SECRET|_BASE_URL|_PROXY)$/i.test(key)) continue;
    if (/^(HTTP|HTTPS|ALL|NO)_PROXY$/i.test(key) || key === "http_proxy" || key === "https_proxy") continue;
    env[key] = value;
  }
  Object.assign(env, {
    HOME: dirs.home,
    XDG_CONFIG_HOME: path.join(dirs.home, ".config"),
    XDG_CACHE_HOME: path.join(dirs.home, ".cache"),
    XDG_DATA_HOME: path.join(dirs.home, ".local", "share"),
    XDG_STATE_HOME: path.join(dirs.home, ".local", "state"),
    [`${prefix}_CODING_AGENT_DIR`]: dirs.agent,
    [`${prefix}_OFFLINE`]: "1",
    [`${prefix}_TELEMETRY`]: "0",
    PARITY_FAKE_SIDE: side,
    NO_COLOR: "1",
    TERM: "dumb",
    LC_ALL: "en_US.UTF-8",
  });
  if (side === "pi") env.PI_SKIP_VERSION_CHECK = "1";
  return env;
}

function launchSpec(side, config) {
  const { scenario, piCli, rpiBin, piExtensions, rpiExtensions } = config;
  const common = [
    "--mode",
    "rpc",
    "--no-session",
    "--no-tools",
    "--no-context-files",
    "--no-skills",
    "--no-prompt-templates",
    "--offline",
    "--provider",
    scenario.provider.name,
    "--model",
    scenario.provider.model,
    "--system-prompt",
    scenario.systemPrompt,
    ...scenario.args,
  ];
  if (side === "pi") {
    return {
      command: process.execPath,
      args: [
        "--import",
        pathToFileURL(PRELOAD).href,
        piCli,
        ...common,
        ...piExtensions.flatMap((extension) => ["-e", extension]),
      ],
    };
  }
  const isScript = /\.m?js$/.test(rpiBin);
  return {
    command: isScript ? process.execPath : rpiBin,
    args: [
      ...(isScript ? [rpiBin] : []),
      ...common,
      ...rpiExtensions.flatMap((extension) => ["-e", extension]),
    ],
  };
}

async function prepareSide(workDir, side, scenario, port) {
  const root = path.join(workDir, side);
  const dirs = {
    root,
    home: path.join(root, "home"),
    agent: path.join(root, "agent"),
    cwd: path.join(root, "cwd"),
  };
  for (const dir of [dirs.home, dirs.agent, dirs.cwd]) await mkdir(dir, { recursive: true });
  const provider = await startFakeOpenAI({
    replies: scenario.provider.replies,
    port,
    chunkChars: scenario.provider.chunkChars,
    chunkDelayMs: scenario.provider.chunkDelayMs,
  });
  const models = {
    providers: {
      [scenario.provider.name]: {
        api: "openai-completions",
        baseUrl: `${provider.baseUrl}/v1`,
        apiKey: "parity-test-key",
        models: [{ id: scenario.provider.model }],
      },
    },
  };
  await writeFile(path.join(dirs.agent, "models.json"), `${JSON.stringify(models, null, 2)}\n`);
  const realRoot = await realpath(root);
  return { side, dirs, provider, roots: [root, realRoot] };
}

class Side {
  constructor(name, prepared, config) {
    this.name = name;
    this.prepared = prepared;
    this.scenario = config.scenario;
    this.dialogCursor = 0;
    this.rawLines = [];
    this.records = [];
    this.inputs = [];
    this.stderr = "";
    this.failure = null;
    this.exit = null;
    this.decoder = new StringDecoder("utf8");
    this.buffer = "";
    const { command, args } = launchSpec(name, config);
    this.child = spawn(command, args, {
      cwd: prepared.dirs.cwd,
      env: childEnv(name, prepared.dirs),
      stdio: ["pipe", "pipe", "pipe"],
    });
    this.child.stdout.on("data", (chunk) => this.onData(chunk));
    this.child.stdout.on("end", () => this.onEnd());
    this.child.stderr.on("data", (chunk) => {
      this.stderr += chunk.toString("utf8");
    });
    this.child.stdin.on("error", (error) => this.fail("side", `stdin write failed: ${error.message}`));
    this.child.on("exit", (code, signal) => {
      this.exit = { code, signal };
    });
    this.child.on("error", (error) => this.fail("side", `spawn failed: ${error.message}`));
  }

  fail(code, message) {
    if (!this.failure) this.failure = { code, message: `${this.name}: ${message}` };
  }

  onData(chunk) {
    this.buffer += this.decoder.write(chunk);
    let index = this.buffer.indexOf("\n");
    while (index >= 0) {
      let line = this.buffer.slice(0, index);
      this.buffer = this.buffer.slice(index + 1);
      if (line.endsWith("\r")) line = line.slice(0, -1);
      if (line !== "") this.onLine(line);
      index = this.buffer.indexOf("\n");
    }
  }

  onEnd() {
    this.buffer += this.decoder.end();
    if (this.buffer !== "") {
      this.fail("protocol", "stdout ended with an unterminated record");
      this.buffer = "";
    }
  }

  onLine(line) {
    this.rawLines.push(line);
    let record;
    try {
      record = JSON.parse(line);
    } catch {
      this.fail("protocol", `non-JSON stdout line ${this.rawLines.length}`);
      return;
    }
    this.records.push(record);
    if (isDialogRequest(this.name, record)) this.answer(record);
  }

  send(record) {
    this.inputs.push(record);
    this.child.stdin.write(`${JSON.stringify(record)}\n`);
  }

  // Answer the next scripted dialog. Kind mismatches and unscripted requests
  // fail the side; they are never silently skipped.
  answer(request) {
    if (this.failure) return;
    const scripted = this.scenario.dialogs[this.dialogCursor];
    if (scripted === undefined) {
      this.fail("dialog", `unscripted dialog request ${JSON.stringify(request)}`);
      return;
    }
    this.dialogCursor += 1;
    try {
      this.send(answerFor(this.name, request, scripted));
    } catch (error) {
      this.fail(error instanceof NormalizeError ? error.code : "dialog", error.message);
    }
  }
}

async function waitFor(side, predicate, { from, count = 1, timeoutMs, description }) {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    if (side.failure) throw new RunnerError(side.failure.code, side.failure.message);
    const found = side.records.slice(from).filter(predicate).length;
    if (found >= count) return;
    if (side.exit) {
      throw new RunnerError("exit", `${side.name} exited (${JSON.stringify(side.exit)}) while waiting for ${description}`);
    }
    if (Date.now() > deadline) {
      throw new RunnerError("timeout", `${side.name}: timed out after ${timeoutMs} ms waiting for ${description}`);
    }
    await sleep(10);
  }
}

async function drive(side) {
  const { scenario } = side;
  for (const [index, step] of scenario.steps.entries()) {
    const from = side.records.length;
    if (step.send !== undefined) {
      side.send(step.send);
      if (step.send.id !== undefined) {
        await waitFor(
          side,
          (record) => record.type === "response" && record.id === step.send.id,
          { from, timeoutMs: scenario.timeoutMs, description: `response to ${step.send.id}` },
        );
      }
    }
    for (const spec of step.await === undefined ? [] : [].concat(step.await)) {
      await waitFor(side, matcher(spec), {
        from,
        count: spec.count ?? 1,
        timeoutMs: scenario.timeoutMs,
        description: `${spec.type}${spec.id === undefined ? "" : ` (${spec.id})`} in steps[${index}]`,
      });
    }
  }
}

async function closeSide(side) {
  if (!side.exit) {
    try {
      side.child.stdin.end();
    } catch {
      // The child may already have closed its end; the exit wait below decides.
    }
  }
  const deadline = Date.now() + EXIT_TIMEOUT_MS;
  while (!side.exit && Date.now() < deadline) await sleep(10);
  if (!side.exit) {
    side.child.kill("SIGKILL");
    side.fail("exit", "did not exit after stdin closed (killed)");
  } else if (side.exit.code !== 0 && !side.failure) {
    side.fail("exit", `exited with ${JSON.stringify(side.exit)}`);
  }
}

function killChild(side) {
  if (side && side.child.exitCode === null && side.child.signalCode === null) {
    side.child.kill("SIGKILL");
  }
}

function resolveExisting(name, value) {
  if (!value) throw new RunnerError("setup", `${name} is required`);
  const resolved = path.resolve(value);
  if (!existsSync(resolved)) throw new RunnerError("setup", `${name}: ${resolved} does not exist`);
  return resolved;
}

// Run one scenario. Returns { status, exitCode, summary, outDir }.
export async function runScenario(options) {
  const scenario = validateScenario(options.scenario);
  const piCli = resolveExisting("--pi", options.piCli);
  const rpiBin = resolveExisting("--rpi", options.rpiBin);
  const piExtensions = (options.piExtensions ?? []).map((item) => resolveExisting("--pi-extension", item));
  const rpiExtensions = (options.rpiExtensions ?? []).map((item) => resolveExisting("--rpi-extension", item));

  const workDir = options.workDir
    ? path.resolve(options.workDir)
    : await mkdtemp(path.join(await ensureCacheDir(), "run-"));
  const outDir = options.outDir ? path.resolve(options.outDir) : path.join(workDir, "out");
  await mkdir(outDir, { recursive: true });

  const config = { scenario, piCli, rpiBin, piExtensions, rpiExtensions };
  // The sides run one after the other on one provider port, so the endpoint
  // written into both transcripts is the same value.
  const port = await pickFreePort();
  const runs = {};
  for (const name of ["pi", "rpi"]) {
    runs[name] = await runSide(name, workDir, config, port);
  }
  return finish({ runs, scenario, outDir });
}

async function pickFreePort() {
  const probe = http.createServer();
  await new Promise((resolve, reject) => {
    probe.once("error", reject);
    probe.listen(0, "127.0.0.1", resolve);
  });
  const { port } = probe.address();
  await new Promise((resolve) => probe.close(resolve));
  return port;
}

// Run one side to completion (or failure) and always release its resources.
async function runSide(name, workDir, config, port) {
  const prepared = await prepareSide(workDir, name, config.scenario, port);
  const side = new Side(name, prepared, config);
  try {
    try {
      await drive(side);
    } catch (error) {
      side.fail(error instanceof RunnerError ? error.code : "runner", error.message);
    }
    await closeSide(side);
    if (!side.failure && side.dialogCursor !== config.scenario.dialogs.length) {
      side.fail("dialog", `${config.scenario.dialogs.length - side.dialogCursor} scripted dialog(s) were never requested`);
    }
  } finally {
    killChild(side);
    await prepared.provider.close();
  }
  return { side, roots: prepared.roots, requests: prepared.provider.requests, unusedReplies: prepared.provider.unusedReplies() };
}

async function ensureCacheDir() {
  const dir = path.join(homedir(), ".cache", "parity-rpc");
  await mkdir(dir, { recursive: true });
  return dir;
}

async function finish({ runs, scenario, outDir }) {
  const write = (name, content) => writeFile(path.join(outDir, name), content);
  for (const name of ["pi", "rpi"]) {
    const { side, requests } = runs[name];
    await write(`${name}.raw.jsonl`, side.rawLines.map((line) => `${line}\n`).join(""));
    await write(`${name}.stderr.log`, side.stderr);
    await write(`${name}.inputs.jsonl`, side.inputs.map((record) => `${JSON.stringify(record)}\n`).join(""));
    await write(`${name}.provider-requests.jsonl`, requests.map((request) => `${JSON.stringify(request)}\n`).join(""));
  }

  const failures = [runs.pi.side.failure, runs.rpi.side.failure].filter(Boolean);
  let error = failures[0] ?? null;
  let normalized = null;
  let comparison = null;
  if (!error) {
    try {
      normalized = {
        pi: normalizeTranscript("pi", runs.pi.side.records, runs.pi.roots),
        rpi: normalizeTranscript("rpi", runs.rpi.side.records, runs.rpi.roots),
      };
      comparison = compareNormalized(normalized.pi, normalized.rpi);
    } catch (caught) {
      if (!(caught instanceof NormalizeError)) throw caught;
      error = { code: caught.code, message: caught.message };
    }
  }

  if (normalized) {
    await write("pi.normalized.json", canonicalJson(normalized.pi));
    await write("rpi.normalized.json", canonicalJson(normalized.rpi));
    const details = comparison.mismatches.map((mismatch) => ({
      ...mismatch,
      pi: normalized.pi[mismatch.index] ?? null,
      rpi: normalized.rpi[mismatch.index] ?? null,
    }));
    await write("diff.json", canonicalJson({ identical: comparison.identical, mismatches: details }));
  }

  // Event-type order per side, without any field comparison: shows where the
  // two streams have different event kinds, independent of payload differences.
  const typeSequence = normalized
    ? { pi: normalized.pi.map((record) => record.type), rpi: normalized.rpi.map((record) => record.type) }
    : null;

  const status = error ? "error" : comparison.identical ? "identical" : "different";
  const summary = {
    scenario: scenario.name,
    status,
    error,
    identical: comparison?.identical ?? false,
    piRecords: comparison?.piRecords ?? runs.pi.side.records.length,
    rpiRecords: comparison?.rpiRecords ?? runs.rpi.side.records.length,
    mismatches: comparison?.mismatches ?? [],
    typeSequence,
    exit: { pi: runs.pi.side.exit, rpi: runs.rpi.side.exit },
    providerRequests: { pi: runs.pi.requests.length, rpi: runs.rpi.requests.length },
    unusedReplies: { pi: runs.pi.unusedReplies, rpi: runs.rpi.unusedReplies },
  };
  await write("summary.json", canonicalJson(summary));
  const exitCode = status === "identical" ? 0 : status === "different" ? 1 : 2;
  return { status, exitCode, summary, outDir };
}

function parseArgs(argv) {
  const options = {
    scenario: null,
    piCli: process.env.PARITY_PI_CLI,
    rpiBin: process.env.PARITY_RPI_BIN,
    piExtensions: [],
    rpiExtensions: [],
    outDir: null,
    workDir: null,
    help: false,
  };
  for (let index = 0; index < argv.length; index++) {
    const arg = argv[index];
    const value = () => {
      if (index + 1 >= argv.length) throw new RunnerError("usage", `${arg} needs a value`);
      index += 1;
      return argv[index];
    };
    if (arg === "--help" || arg === "-h") options.help = true;
    else if (arg === "--pi") options.piCli = value();
    else if (arg === "--rpi") options.rpiBin = value();
    else if (arg === "--pi-extension") options.piExtensions.push(value());
    else if (arg === "--rpi-extension") options.rpiExtensions.push(value());
    else if (arg === "--out") options.outDir = value();
    else if (arg === "--work") options.workDir = value();
    else if (arg.startsWith("--")) throw new RunnerError("usage", `unknown option ${arg}`);
    else if (options.scenario === null) options.scenario = arg;
    else throw new RunnerError("usage", `unexpected argument ${arg}`);
  }
  if (!options.help && options.scenario === null) throw new RunnerError("usage", "a scenario file is required");
  return options;
}

const USAGE =
  "usage: run.mjs <scenario.json> --pi <pi cli.js> --rpi <rpi binary> [--rpi-extension <lib> ...] [--pi-extension <path> ...] [--out <dir>] [--work <dir>]\n";

async function main(argv) {
  // A closed reader (for example `| head`) must not turn into a stack trace.
  process.stdout.on("error", () => {});
  let options;
  try {
    options = parseArgs(argv);
  } catch (error) {
    process.stderr.write(`${error.message}\n${USAGE}`);
    return 2;
  }
  if (options.help) {
    process.stdout.write(USAGE);
    return 0;
  }
  let scenario;
  try {
    scenario = JSON.parse(await readFile(path.resolve(options.scenario), "utf8"));
  } catch (error) {
    process.stderr.write(`cannot read scenario ${options.scenario}: ${error.message}\n`);
    return 2;
  }
  try {
    const result = await runScenario({
      scenario,
      piCli: options.piCli,
      rpiBin: options.rpiBin,
      piExtensions: options.piExtensions,
      rpiExtensions: options.rpiExtensions,
      outDir: options.outDir,
      workDir: options.workDir,
    });
    const { summary } = result;
    process.stdout.write(`${summary.scenario}: ${result.status}\n`);
    if (summary.error) process.stdout.write(`  ${summary.error.code}: ${summary.error.message}\n`);
    for (const mismatch of summary.mismatches.slice(0, 20)) {
      process.stdout.write(`  record ${mismatch.index} (pi ${mismatch.pi}, rpi ${mismatch.rpi}): ${mismatch.paths.slice(0, 5).join(", ")}\n`);
    }
    if (summary.mismatches.length > 20) process.stdout.write(`  ... ${summary.mismatches.length - 20} more mismatched records\n`);
    process.stdout.write(`  output: ${result.outDir}\n`);
    return result.exitCode;
  } catch (error) {
    process.stderr.write(`${error.code ?? "runner"}: ${error.message}\n`);
    return 2;
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  process.exitCode = await main(process.argv.slice(2));
}
