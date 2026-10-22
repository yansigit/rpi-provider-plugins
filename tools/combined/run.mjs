#!/usr/bin/env node
// Combined three-provider differential run for R03.
//
// One scenario, six segments:
//   agy-a, agy-b            Antigravity through a two-account multiprovider pool
//   cursor-a, cursor-b      Cursor through a two-account multiprovider pool
//   virtual-gemini          a virtual provider model backed by Antigravity
//   virtual-composer        a virtual provider model backed by Cursor
//
// The reference side is unchanged Pi 1.3.0 with the three pinned upstream
// extensions. The candidate side is rpi with the three ported cdylibs. The
// Antigravity upstream is a loopback CONNECT/TLS server and the Cursor SDK is
// the vendored scenario fake; no real credential or network is used.
//
//   node tools/combined/run.mjs --pi <pi cli.js> --rpi <rpi binary>
//        [--work <dir>] [--write] [--only <segment>]
//
// Without --write the pinned expectations in fixtures/expected/ are checked.
// Exit status: 0 all checks pass, 1 a comparison/expectation differs,
// 2 a run failed.
import { spawn } from "node:child_process";
import { createHash as createHashImpl } from "node:crypto";
import { chmod, mkdir, mkdtemp, readFile, realpath, writeFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import { homedir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { startAgyServer } from "./agy-server.mjs";
import { canonicalJson, compareNormalized, normalizeTranscript, NormalizeError } from "./normalize.mjs";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const PRELOAD = path.join(HERE, "preload.mjs");
const FAKE_SDK = path.join(HERE, "fake-sdk");
const REGISTER = path.join(FAKE_SDK, "register.mjs");
const CERT = path.join(HERE, "fixtures/tls/agy-server-cert.pem");
const EXPECTED = path.join(HERE, "fixtures/expected/combined.diff.json");
const STEP_TIMEOUT_MS = 120000;
const EXIT_TIMEOUT_MS = 30000;

export const SCENARIO = {
  name: "combined",
  systemPrompt: "Parity fixed system prompt.",
  agy: {
    tokens: [
      { token: "fixture-agy-account-a", label: "agy-a", project: "fixture-project-a" },
      { token: "fixture-agy-account-b", label: "agy-b", project: "fixture-project-b" },
    ],
    replies: [
      "Antigravity reply one.",
      "Antigravity reply two.",
      "Antigravity virtual reply.",
    ],
    usage: { promptTokenCount: 11, candidatesTokenCount: 7, totalTokenCount: 18 },
  },
  cursor: {
    accounts: [
      { key: "fixture-cursor-a", label: "cursor-a" },
      { key: "fixture-cursor-b", label: "cursor-b" },
    ],
  },
  segments: [
    {
      id: "agy-a",
      kind: "agy",
      provider: "google",
      model: "antigravity-gemini-3.8-flash",
      credential: "agy-a",
      order: ["agy-a", "agy-b"],
      prompt: "Antigravity parity prompt one.",
      reply: "Antigravity reply one.",
      virtual: false,
    },
    {
      id: "agy-b",
      kind: "agy",
      provider: "google",
      model: "antigravity-gemini-3.8-flash",
      credential: "agy-b",
      order: ["agy-b", "agy-a"],
      prompt: "Antigravity parity prompt two.",
      reply: "Antigravity reply two.",
      virtual: false,
    },
    {
      id: "cursor-a",
      kind: "cursor",
      provider: "cursor",
      model: "composer-2",
      credential: "cursor-a",
      order: ["cursor-a", "cursor-b"],
      prompt: "Cursor parity prompt one.",
      reply: "Cursor reply one.",
      fakeScenario: "combined-cursor-a.json",
      virtual: false,
      cursorPool: false,
    },
    {
      id: "cursor-b",
      kind: "cursor",
      provider: "cursor",
      model: "composer-2",
      credential: "cursor-b",
      order: ["cursor-b", "cursor-a"],
      prompt: "Cursor parity prompt two.",
      reply: "Cursor reply two.",
      fakeScenario: "combined-cursor-b.json",
      virtual: false,
      cursorPool: false,
    },
    {
      id: "virtual-gemini",
      kind: "agy",
      provider: "parity-virtual",
      model: "virtual-gemini",
      credential: "agy-a",
      order: ["agy-a", "agy-b"],
      prompt: "Antigravity virtual prompt.",
      reply: "Antigravity virtual reply.",
      fakeScenario: "combined-cursor-a.json",
      virtual: true,
      launchProvider: "google",
      launchModel: "antigravity-gemini-3.8-flash",
      selectModel: true,
    },
    {
      id: "virtual-composer",
      kind: "cursor",
      provider: "parity-virtual",
      model: "virtual-composer",
      credential: "cursor-a",
      order: ["cursor-a", "cursor-b"],
      prompt: "Cursor virtual prompt.",
      reply: "Cursor virtual reply.",
      fakeScenario: "combined-cursor-v.json",
      virtual: true,
      launchProvider: "cursor",
      launchModel: "composer-2",
      selectModel: true,
      cursorPool: false,
      expectFailure: "Cursor provider ownership is unavailable",
    },
    {
      id: "cursor-pooled",
      kind: "cursor",
      provider: "cursor",
      model: "composer-2",
      credential: "cursor-a",
      order: ["cursor-a", "cursor-b"],
      prompt: "Cursor pooled prompt.",
      reply: "Cursor pooled reply.",
      fakeScenario: "combined-cursor-pooled.json",
      virtual: false,
      cursorPool: true,
      expectFailure: "Cursor provider ownership is unavailable",
    },
  ],
};

class RunError extends Error {
  constructor(code, message) {
    super(message);
    this.name = "RunError";
    this.code = code;
  }
}

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

function parseArgs(argv) {
  const options = {
    pi: process.env.PARITY_PI_CLI ??
      "/home/user/Projects/provider-references/integrated/node_modules/@earendil-works/pi-coding-agent/dist/cli.js",
    rpi: process.env.PARITY_RPI_BIN ?? "/home/user/Projects/pi-rust/target/debug/rpi",
    rpiLibs: {
      antigravity: process.env.PARITY_RPI_ANTIGRAVITY ?? "",
      cursor: process.env.PARITY_RPI_CURSOR ?? "",
      multiprovider: process.env.PARITY_RPI_MULTIPROVIDER ?? "",
    },
    piExtensions: {
      antigravity:
        process.env.PARITY_PI_ANTIGRAVITY ??
        "/home/user/Projects/provider-references/integrated/node_modules/@cortexkit/pi-antigravity-auth",
      cursor:
        process.env.PARITY_PI_CURSOR ??
        "/home/user/Projects/provider-references/integrated/node_modules/pi-cursor-sdk",
      multiprovider:
        process.env.PARITY_PI_MULTIPROVIDER ??
        "/home/user/Projects/provider-references/integrated/node_modules/pi-multiprovider/extensions/multiprovider.ts",
    },
    work: null,
    write: false,
    only: null,
    sides: ["pi", "rpi"],
    keep: process.env.R03_KEEP === "1",
  };
  for (let index = 0; index < argv.length; index++) {
    const arg = argv[index];
    const value = () => {
      index += 1;
      if (index >= argv.length) throw new RunError("usage", `${arg} needs a value`);
      return argv[index];
    };
    if (arg === "--pi") options.pi = value();
    else if (arg === "--rpi") options.rpi = value();
    else if (arg === "--rpi-antigravity") options.rpiLibs.antigravity = value();
    else if (arg === "--rpi-cursor") options.rpiLibs.cursor = value();
    else if (arg === "--rpi-multiprovider") options.rpiLibs.multiprovider = value();
    else if (arg === "--pi-antigravity") options.piExtensions.antigravity = value();
    else if (arg === "--pi-cursor") options.piExtensions.cursor = value();
    else if (arg === "--pi-multiprovider") options.piExtensions.multiprovider = value();
    else if (arg === "--work") options.work = value();
    else if (arg === "--only") options.only = value();
    else if (arg === "--side") options.sides = [value()];
    else if (arg === "--write") options.write = true;
    else if (arg === "--keep") options.keep = true;
    else if (arg === "--help" || arg === "-h") options.help = true;
    else throw new RunError("usage", `unknown argument ${arg}`);
  }
  return options;
}

async function ensurePrivate(directory) {
  await mkdir(directory, { recursive: true, mode: 0o700 });
  await chmod(directory, 0o700);
}

function childEnv(side, dirs, extra) {
  const prefix = side === "pi" ? "PI" : "RPI";
  const env = {};
  for (const [key, value] of Object.entries(process.env)) {
    if (/^(PI|RPI)_/.test(key)) continue;
    if (/(_API_KEY|_AUTH_TOKEN|_ACCESS_TOKEN|_TOKEN|_SECRET|_BASE_URL|_PROXY)$/i.test(key)) continue;
    if (/^(HTTP|HTTPS|ALL|NO)_PROXY$/i.test(key) || key === "http_proxy" || key === "https_proxy") continue;
    if (key === "NODE_OPTIONS") continue;
    env[key] = value;
  }
  Object.assign(env, {
    HOME: dirs.home,
    TMPDIR: dirs.tmp,
    XDG_CONFIG_HOME: path.join(dirs.home, ".config"),
    XDG_CACHE_HOME: path.join(dirs.home, ".cache"),
    XDG_DATA_HOME: path.join(dirs.home, ".local", "share"),
    XDG_STATE_HOME: path.join(dirs.home, ".local", "state"),
    [`${prefix}_CODING_AGENT_DIR`]: dirs.agent,
    [`${prefix}_OFFLINE`]: "1",
    [`${prefix}_TELEMETRY`]: "0",
    NO_COLOR: "1",
    TERM: "dumb",
    LC_ALL: "en_US.UTF-8",
    NODE_EXTRA_CA_CERTS: CERT,
  });
  if (side === "pi") env.PI_SKIP_VERSION_CHECK = "1";
  return Object.assign(env, extra);
}

function piArgs(options, segment) {
  const extensions = [
    options.piExtensions.antigravity,
    options.piExtensions.cursor,
    options.piExtensions.multiprovider,
  ];
  return [
    "--mode", "rpc",
    "--no-tools",
    "--no-context-files",
    "--no-skills",
    "--no-prompt-templates",
    "--offline",
    ...extensions.flatMap((item) => ["-e", item]),
    "--provider", segment.launchProvider ?? segment.provider,
    "--model", segment.launchModel ?? segment.model,
  ];
}

function rpiArgs(options, segment, dirs) {
  const libs = [options.rpiLibs.antigravity, options.rpiLibs.cursor, options.rpiLibs.multiprovider].filter(Boolean);
  return [
    "--mode", "rpc",
    "--session-dir", dirs.sessions,
    "--no-tools",
    "--no-context-files",
    "--no-skills",
    "--no-prompt-templates",
    "--offline",
    ...libs.flatMap((item) => ["-e", item]),
    "--provider", segment.launchProvider ?? segment.provider,
    "--model", segment.launchModel ?? segment.model,
  ];
}

const DIALOG_METHODS = new Set(["select", "confirm", "input", "editor"]);

class Child {
  constructor(command, args, { cwd, env }) {
    this.lines = [];
    this.records = [];
    this.stderr = "";
    this.exit = null;
    this.buffer = "";
    this.failure = null;
    this.child = spawn(command, args, { cwd, env, stdio: ["pipe", "pipe", "pipe"] });
    this.child.stdout.setEncoding("utf8");
    this.child.stdout.on("data", (chunk) => {
      this.buffer += chunk;
      let index = this.buffer.indexOf("\n");
      while (index >= 0) {
        const line = this.buffer.slice(0, index);
        this.buffer = this.buffer.slice(index + 1);
        if (line !== "") this.onLine(line);
        index = this.buffer.indexOf("\n");
      }
    });
    this.child.stderr.setEncoding("utf8");
    this.child.stderr.on("data", (chunk) => {
      this.stderr += chunk;
    });
    this.child.on("exit", (code, signal) => {
      this.exit = { code, signal };
    });
    this.child.stdin.on("error", (error) => this.fail("stdin", error.message));
    this.child.on("error", (error) => this.fail("spawn", error.message));
  }

  fail(code, message) {
    if (!this.failure) this.failure = { code, message };
  }

  onLine(line) {
    this.lines.push(line);
    let record;
    try {
      record = JSON.parse(line);
    } catch {
      this.fail("protocol", `non-JSON stdout line: ${line.slice(0, 120)}`);
      return;
    }
    this.records.push(record);
    if (record.type === "extension_ui_request" && DIALOG_METHODS.has(record.method)) {
      this.fail("dialog", `unexpected dialog ${record.method}: ${JSON.stringify(record).slice(0, 200)}`);
    }
    if (record.type === "ui_dialog_request") {
      this.fail("dialog", `unexpected native dialog: ${JSON.stringify(record).slice(0, 200)}`);
    }
  }

  send(record) {
    this.child.stdin.write(`${JSON.stringify(record)}\n`);
  }

  async waitFor(predicate, description, timeoutMs = STEP_TIMEOUT_MS) {
    const deadline = Date.now() + timeoutMs;
    for (;;) {
      if (this.failure) throw new RunError(this.failure.code, `${description}: ${this.failure.message}`);
      const found = this.records.find(predicate);
      if (found) return found;
      if (this.exit) throw new RunError("exit", `${description}: child exited ${JSON.stringify(this.exit)}\n${this.stderr.slice(-2000)}`);
      if (Date.now() > deadline) throw new RunError("timeout", `${description}: timed out after ${timeoutMs} ms`);
      await sleep(10);
    }
  }

  async close() {
    if (!this.exit) {
      try {
        this.child.stdin.end();
      } catch {
        // The exit wait below decides.
      }
    }
    const deadline = Date.now() + EXIT_TIMEOUT_MS;
    while (!this.exit && Date.now() < deadline) await sleep(10);
    if (!this.exit) {
      this.child.kill("SIGKILL");
      this.fail("exit", "did not exit after stdin closed");
    } else if (this.exit.code !== 0 && !this.failure) {
      this.fail("exit", `exited ${JSON.stringify(this.exit)}\n${this.stderr.slice(-2000)}`);
    }
    if (this.failure) throw new RunError(this.failure.code, this.failure.message);
  }
}

function agyEntry(label) {
  return SCENARIO.agy.tokens.find((entry) => entry.label === label);
}

function cursorEntry(label) {
  return SCENARIO.cursor.accounts.find((entry) => entry.label === label);
}

// Pi's Credential `expires` is epoch milliseconds; rpi's is epoch seconds.
function piAuth(segment) {
  if (segment.kind === "agy") {
    const account = agyEntry(segment.credential);
    return {
      google: {
        type: "oauth",
        access: account.token,
        refresh: `fixture-refresh-${segment.credential}|${account.project}`,
        expires: 4102444800000,
      },
    };
  }
  return {};
}

function rpiAuth(segment) {
  if (segment.kind === "agy") {
    const account = agyEntry(segment.credential);
    return {
      google: {
        type: "oauth",
        access: account.token,
        refresh: `fixture-refresh-${segment.credential}|${account.project}`,
        expires: 4102444800,
      },
    };
  }
  const account = cursorEntry(segment.credential);
  return { cursor: { type: "api_key", key: account.key } };
}

function multiproviderState(segment) {
  const pool = (kind, order) => {
    const accounts =
      kind === "agy"
        ? order.map((label) => {
            const account = agyEntry(label);
            return {
              id: label,
              label,
              authKind: "oauth",
              enabled: true,
              weight: 1,
              priority: order.indexOf(label) + 1,
              createdAt: "2026-01-01T00:00:00.000Z",
              updatedAt: "2026-01-01T00:00:00.000Z",
              credential: {
                type: "oauth",
                access: account.token,
                refresh: `fixture-refresh-${label}|${account.project}`,
                expires: 4102444800000,
              },
            };
          })
        : order.map((label) => {
            const account = cursorEntry(label);
            return {
              id: label,
              label,
              authKind: "api-key",
              enabled: true,
              weight: 1,
              priority: order.indexOf(label) + 1,
              createdAt: "2026-01-01T00:00:00.000Z",
              updatedAt: "2026-01-01T00:00:00.000Z",
              credential: { type: "api_key", key: account.key },
            };
          });
    return { policy: "round-robin", affinity: false, includeUpstream: false, accounts };
  };
  return {
    version: 1,
    providers: {
      google: pool("agy", segment.kind === "agy" ? segment.order : ["agy-a", "agy-b"]),
      ...(segment.cursorPool
        ? { cursor: pool("cursor", segment.kind === "cursor" ? segment.order : ["cursor-a", "cursor-b"]) }
        : {}),
    },
    virtuals: {
      "parity-virtual": {
        id: "parity-virtual",
        label: "Parity Virtual",
        models: [
          {
            id: "virtual-gemini",
            label: "Virtual Gemini",
            backends: [
              {
                providerId: "google",
                modelId: "antigravity-gemini-3.8-flash",
                template: {
                  api: "google-generative-ai",
                  baseUrl: "https://cloudcode-pa.googleapis.com",
                  reasoning: true,
                  input: ["text", "image"],
                  cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
                  contextWindow: 1048576,
                  maxTokens: 65536,
                },
              },
            ],
          },
          {
            id: "virtual-composer",
            label: "Virtual Composer",
            backends: [
              {
                providerId: "cursor",
                modelId: "composer-2",
                template: {
                  api: "cursor-sdk",
                  baseUrl: "https://api.cursor.com",
                  reasoning: true,
                  input: ["text", "image"],
                  cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
                  contextWindow: 200000,
                  maxTokens: 64000,
                },
              },
            ],
          },
        ],
      },
    },
  };
}

async function runSegment(side, segment, options, dirs, servers) {
  const requestStart = servers.agy.requests.length;
  const replyStart = servers.agy.streamCount();
  const env = childEnv(side, dirs, {
    HTTPS_PROXY: servers.agy.proxyUrl,
    CURSOR_API_KEY: cursorEntry(segment.kind === "cursor" ? segment.credential : "cursor-a").key,
  });
  let command;
  let args;
  if (side === "pi") {
    command = process.execPath;
    args = ["--import", PRELOAD, options.pi, ...piArgs(options, segment)];
    env.NODE_OPTIONS = `--import ${REGISTER}`;
  } else {
    command = options.rpi;
    args = rpiArgs(options, segment, dirs);
    // The Cursor worker is a Node child of the Rust host; NODE_OPTIONS reaches
    // it unchanged. The preload keeps the worker's clock deterministic too.
    env.NODE_OPTIONS = `--import ${REGISTER} --import ${PRELOAD}`;
  }
  env.RPI_CURSOR_FAKE_SCENARIO = path.join(FAKE_SDK, "scenarios", segment.fakeScenario ?? "combined-cursor-a.json");
  env.RPI_CURSOR_FAKE_LOG = dirs.fakeLog;
  env.RPI_CURSOR_FAKE_LOG_ROOT = dirs.root;
  env.RPI_CURSOR_FAKE_HOME = dirs.cursorHome;

  const child = new Child(command, args, { cwd: dirs.cwd, env });
  try {
    // Ask for the catalog before the prompt: this records the providers the
    // side registers and makes a missing multiprovider registration visible.
    const modelsId = `${segment.id}-models`;
    child.send({ id: modelsId, type: "get_available_models" });
    const models = await child.waitFor(
      (record) => record.type === "response" && record.id === modelsId,
      `${side} ${segment.id} model catalog`,
    );
    const providerIds = [
      ...new Set(((models.data?.models ?? []).map((model) => model.provider)).filter(Boolean)),
    ].sort();

    if (segment.selectModel) {
      const selectId = `${segment.id}-select`;
      child.send({ id: selectId, type: "set_model", provider: segment.provider, modelId: segment.model });
      const selected = await child.waitFor(
        (record) => record.type === "response" && record.id === selectId,
        `${side} ${segment.id} set_model`,
      );
      if (selected.success !== true) {
        throw new RunError("set-model", `${side} ${segment.id} set_model failed: ${JSON.stringify(selected)}`);
      }
    }

    const promptId = `${segment.id}-prompt`;
    child.send({ id: promptId, type: "prompt", message: segment.prompt });
    await child.waitFor(
      (record) => record.type === "response" && record.id === promptId,
      `${side} ${segment.id} prompt response`,
    );
    await child.waitFor((record) => record.type === "agent_settled", `${side} ${segment.id} agent_settled`);
    await child.close();
    return {
      records: child.records,
      raw: child.lines,
      stderr: child.stderr,
      providerIds,
      requests: servers.agy.requests.slice(requestStart),
      replyIndex: replyStart,
      fakeLog: existsSync(dirs.fakeLog) ? await readFile(dirs.fakeLog, "utf8") : "",
    };
  } finally {
    if (child.child.exitCode === null && child.child.signalCode === null) child.child.kill("SIGKILL");
  }
}

function assistantTexts(records) {
  return records
    .filter((record) => record.type === "message_end" && record.message?.role === "assistant")
    .map((record) =>
      (record.message.content ?? [])
        .filter((part) => part?.type === "text")
        .map((part) => part.text)
        .join(""),
    );
}

function fakeAccounts(log) {
  const accounts = [];
  for (const line of log.split("\n")) {
    if (line === "") continue;
    let record;
    try {
      record = JSON.parse(line);
    } catch {
      continue;
    }
    if (record.kind === "sdk_call" && record.api === "Agent.create") {
      accounts.push(record.apiKeyDigest);
    }
  }
  return accounts;
}

function requestProjection(requests) {
  return requests
    .filter((request) => request.target.startsWith("/v1internal:streamGenerateContent"))
    .map((request) => ({
      method: request.method,
      target: request.target,
      account: request.account,
      project: request.body?.project ?? null,
    }));
}

async function main(argv) {
  const options = parseArgs(argv);
  if (options.help) {
    process.stdout.write(
      "usage: run.mjs --pi <cli.js> --rpi <rpi> [--rpi-antigravity <so>] [--rpi-cursor <so>] [--rpi-multiprovider <so>] [--work <dir>] [--write] [--only <segment>]\n",
    );
    return 0;
  }
  const root = options.work
    ? path.resolve(options.work)
    : await mkdtemp(path.join(homedir(), ".r03-combined-"));
  await ensurePrivate(root);

  const segments = options.only
    ? SCENARIO.segments.filter((segment) => segment.id === options.only)
    : SCENARIO.segments;
  if (segments.length === 0) throw new RunError("usage", `unknown segment ${options.only}`);

  const results = { pi: {}, rpi: {} };
  const sideServers = {};
  try {
    for (const side of options.sides) {
      const sideRoot = path.join(root, side);
      await ensurePrivate(sideRoot);
      const agy = await startAgyServer(SCENARIO.agy);
      sideServers[side] = agy;
      for (const segment of segments) {
        if (side === "rpi" && (segment.virtual || segment.cursorPool)) continue;
        const dirs = {
          root: path.join(sideRoot, segment.id),
          home: path.join(sideRoot, segment.id, "home"),
          agent: path.join(sideRoot, segment.id, "agent"),
          cwd: path.join(sideRoot, segment.id, "cwd"),
          tmp: path.join(sideRoot, segment.id, "tmp"),
          cursorHome: path.join(sideRoot, segment.id, "cursor-home"),
          sessions: path.join(sideRoot, segment.id, "sessions"),
          fakeLog: path.join(sideRoot, segment.id, "cursor-fake.jsonl"),
        };
        for (const dir of [dirs.root, dirs.home, dirs.agent, dirs.cwd, dirs.tmp, dirs.cursorHome, dirs.sessions]) {
          await ensurePrivate(dir);
        }
        const auth = side === "pi" ? piAuth(segment) : rpiAuth(segment);
        await writeFile(path.join(dirs.agent, "auth.json"), `${JSON.stringify(auth)}\n`, { mode: 0o600 });
        await writeFile(
          path.join(dirs.agent, "settings.json"),
          `${JSON.stringify({ retry: { enabled: false }, compaction: { enabled: false } })}\n`,
        );
        // The same pool/virtual store is written for both sides. rpi's
        // multiprovider port reads this file name; at base adc459b the library
        // registers nothing, so it is inert until the native wrapper lands.
        await writeFile(
          path.join(dirs.agent, "multiprovider-auth.json"),
          `${JSON.stringify(multiproviderState(segment), null, 2)}\n`,
          { mode: 0o600 },
        );
        const result = await runSegment(side, segment, options, dirs, { agy });
        results[side][segment.id] = {
          ...result,
          roots: [dirs.root, await realpath(dirs.root)],
          expectedReply: segment.kind === "agy" ? (SCENARIO.agy.replies[result.replyIndex] ?? segment.reply) : segment.reply,
          expectedAccount: segment.kind === "agy"
            ? segment.credential
            : cursorEntry(segment.credential).key,
        };
      }
      await agy.close();
    }
  } finally {
    for (const server of Object.values(sideServers)) {
      await server.close().catch(() => {});
    }
  }

  // Self-checks: both sides must have produced the scripted reply for every
  // segment they ran, and the expected pool account must have been used.
  const problems = [];
  for (const side of options.sides) {
    for (const [id, result] of Object.entries(results[side])) {
      const segment = segments.find((item) => item.id === id);
      const texts = assistantTexts(result.records);
      if (segment.expectFailure) {
        const errors = result.records
          .filter((record) => record.type === "message_end" && record.message?.role === "assistant")
          .map((record) => record.message.errorMessage ?? "");
        if (!errors.some((message) => message.includes(segment.expectFailure))) {
          problems.push(`${side} ${id}: expected failure containing ${JSON.stringify(segment.expectFailure)}, saw ${JSON.stringify(errors)}`);
        }
        if (segment.kind === "cursor" && fakeAccounts(result.fakeLog).length !== 0) {
          problems.push(`${side} ${id}: expected zero Cursor Agent.create calls`);
        }
        if (segment.kind === "agy" && requestProjection(result.requests ?? []).length !== 0) {
          problems.push(`${side} ${id}: expected zero Antigravity stream requests`);
        }
        continue;
      }
      if (texts.length === 0) problems.push(`${side} ${id}: no assistant message`);
      if (texts.length > 0 && texts[texts.length - 1] !== result.expectedReply) {
        problems.push(`${side} ${id}: reply ${JSON.stringify(texts[texts.length - 1])} != ${JSON.stringify(result.expectedReply)}`);
      }
      const requests = requestProjection(results[side][id].requests ?? []);
      if (segment.kind === "agy") {
        if (requests.length === 0) problems.push(`${side} ${id}: no Antigravity stream request`);
        else {
          const account = requests[0].account;
          if (account !== segment.credential) problems.push(`${side} ${id}: Antigravity account ${account} != ${segment.credential}`);
        }
      } else {
        const accounts = fakeAccounts(result.fakeLog);
        if (accounts.length === 0) problems.push(`${side} ${id}: no Cursor Agent.create`);
        else if (accounts[0] !== digest(result.expectedAccount)) {
          problems.push(`${side} ${id}: Cursor key digest ${accounts[0]} != ${digest(result.expectedAccount)}`);
        }
      }
    }
  }
  if (results.pi["virtual-gemini"] === undefined && segments.some((segment) => segment.id === "virtual-gemini")) {
    problems.push("pi virtual-gemini: segment missing");
  }

  // Normalize and compare the segments both sides ran.
  const outDir = path.join(root, "out");
  await ensurePrivate(outDir);
  const normalized = { pi: {}, rpi: {} };
  const comparison = {};
  const normalizationErrors = [];
  for (const side of options.sides) {
    for (const [id, result] of Object.entries(results[side])) {
      const writes = [];
      for (const name of ["raw.jsonl"]) {
        writes.push(writeFile(path.join(outDir, `${side}-${id}.${name}`), result.raw.map((line) => `${line}\n`).join("")));
      }
      writes.push(writeFile(path.join(outDir, `${side}-${id}.stderr.log`), result.stderr));
      writes.push(
        writeFile(
          path.join(outDir, `${side}-${id}.provider-requests.json`),
          canonicalJson(requestProjection(result.requests ?? [])),
        ),
      );
      writes.push(writeFile(path.join(outDir, `${side}-${id}.cursor-fake.jsonl`), result.fakeLog));
      await Promise.all(writes);
      try {
        normalized[side][id] = normalizeTranscript(side, result.records, result.roots);
        await writeFile(path.join(outDir, `${side}-${id}.normalized.json`), canonicalJson(normalized[side][id]));
      } catch (error) {
        if (!(error instanceof NormalizeError)) throw error;
        normalizationErrors.push({ side, segment: id, code: error.code, message: error.message });
      }
    }
  }
  for (const segment of segments) {
    if (!results.pi[segment.id] || !results.rpi[segment.id]) continue;
    if (!normalized.pi[segment.id] || !normalized.rpi[segment.id]) continue;
    comparison[segment.id] = compareNormalized(normalized.pi[segment.id], normalized.rpi[segment.id]);
  }

  // Provider requests and Cursor account digests are compared for the
  // segments both sides ran. Transcript comparison stays positional.
  const requestComparison = {};
  const cursorAccountComparison = {};
  for (const segment of segments) {
    if (!results.pi[segment.id] || !results.rpi[segment.id]) continue;
    const piRequests = requestProjection(results.pi[segment.id].requests ?? []);
    const rpiRequests = requestProjection(results.rpi[segment.id].requests ?? []);
    requestComparison[segment.id] = {
      identical: canonicalJson(piRequests) === canonicalJson(rpiRequests),
      pi: piRequests,
      rpi: rpiRequests,
    };
    if (!requestComparison[segment.id].identical) {
      problems.push(`${segment.id}: Antigravity request projection differs between sides`);
    }
    const piAccounts = fakeAccounts(results.pi[segment.id].fakeLog);
    const rpiAccounts = fakeAccounts(results.rpi[segment.id].fakeLog);
    cursorAccountComparison[segment.id] = { pi: piAccounts, rpi: rpiAccounts };
    if (canonicalJson(piAccounts) !== canonicalJson(rpiAccounts)) {
      problems.push(`${segment.id}: Cursor account digests differ between sides`);
    }
  }

  const candidateProviders = [
    ...new Set(Object.values(results.rpi).flatMap((result) => result.providerIds)),
  ].sort();
  const referenceProviders = [
    ...new Set(Object.values(results.pi).flatMap((result) => result.providerIds)),
  ].sort();
  const multiproviderRegistered = candidateProviders.includes("parity-virtual");

  const summary = {
    scenario: SCENARIO.name,
    side: {
      pi: { providers: referenceProviders },
      rpi: {
        providers: candidateProviders,
        multiproviderRegistered,
        virtualProviderBlocked: !multiproviderRegistered,
      },
    },
    segments: {
      pi: Object.keys(results.pi).sort(),
      rpi: Object.keys(results.rpi).sort(),
    },
    problems,
    blocked: [
      ...(multiproviderRegistered
        ? []
        : ["rpi multiprovider pool registration and virtual provider are absent from the candidate library set"]),
      "Pi 1.3.0 + pi-cursor-sdk 0.5.3 cannot be pooled or placed behind a virtual provider: the multiprovider lift replaces the headers object that carries the Cursor ownership receipt",
    ],
    normalizationErrors,
    providerRequests: requestComparison,
    cursorAccounts: cursorAccountComparison,
    comparison: Object.fromEntries(
      Object.entries(comparison).map(([id, value]) => [
        id,
        {
          identical: value.identical,
          piRecords: value.piRecords,
          rpiRecords: value.rpiRecords,
          mismatchCount: value.mismatches.length,
          mismatchedRecords: value.mismatches.map((item) => item.index),
          paths: value.mismatches.map((item) => item.paths.slice(0, 8)),
        },
      ]),
    ),
  };
  await writeFile(path.join(outDir, "summary.json"), canonicalJson(summary));
  await writeFile(path.join(outDir, "diff.json"), canonicalJson(comparison));

  // Pin the diff: a new or resolved divergence fails the check until reviewed.
  const pin = { scenario: SCENARIO.name, comparison: summary.comparison };
  if (options.sides.length < 2) {
    // Development run of one side; nothing to pin yet.
  } else if (options.write) {
    await mkdir(path.dirname(EXPECTED), { recursive: true });
    await writeFile(EXPECTED, canonicalJson(pin));
  } else if (existsSync(EXPECTED)) {
    const expected = JSON.parse(await readFile(EXPECTED, "utf8"));
    if (canonicalJson(expected.comparison) !== canonicalJson(pin.comparison)) {
      problems.push("pinned comparison differs; review diff.json and regenerate with --write");
    }
  } else {
    problems.push(`missing pinned comparison ${EXPECTED}`);
  }

  const status = problems.length > 0 ? "fail" : "pass";
  process.stdout.write(`${SCENARIO.name}: ${status}\n`);
  for (const problem of problems.slice(0, 40)) process.stdout.write(`  ${problem}\n`);
  if (problems.length > 40) process.stdout.write(`  ... ${problems.length - 40} more\n`);
  for (const [id, value] of Object.entries(summary.comparison)) {
    process.stdout.write(
      `  ${id}: ${value.identical ? "identical" : `${value.mismatchCount} mismatched record(s)`} (pi ${value.piRecords}, rpi ${value.rpiRecords})\n`,
    );
  }
  if (options.sides.includes("rpi") && !multiproviderRegistered) {
    process.stdout.write("  blocked: rpi multiprovider/virtual provider is not registered by the candidate libraries\n");
  }
  process.stdout.write(`  output: ${outDir}\n`);
  return status === "pass" ? 0 : 1;
}

function digest(text) {
  // Kept in sync with fake-sdk/sdk.mjs apiKeyTag.
  return createHashImpl("sha256").update(text).digest("hex").slice(0, 12);
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main(process.argv.slice(2))
    .then((code) => {
      process.exitCode = code;
    })
    .catch((error) => {
      process.stderr.write(`${error.code ?? "error"}: ${error.message}\n${process.env.R03_DEBUG ? error.stack : ""}\n`);
      process.exitCode = 2;
    });
}
