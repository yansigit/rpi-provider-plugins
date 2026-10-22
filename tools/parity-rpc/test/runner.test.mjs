// Runner mechanics, exercised with protocol-shaped fake agents (fixtures/fake-agent.mjs)
// so the dialog path and the normalizer run without the real binaries.
import assert from "node:assert/strict";
import { mkdir, mkdtemp, readFile, rm } from "node:fs/promises";
import { homedir } from "node:os";
import path from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { runScenario, validateScenario } from "../run.mjs";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const FAKE_AGENT = path.join(HERE, "..", "fixtures", "fake-agent.mjs");

async function workRoot() {
  const base = path.join(homedir(), ".cache", "parity-rpc", "tests");
  await mkdir(base, { recursive: true });
  return mkdtemp(path.join(base, "runner-"));
}

function scenario(name, message, dialogs = [], extra = {}) {
  return {
    name,
    provider: { replies: ["unused by fake agents"] },
    steps: [{ send: { id: "p1", type: "prompt", message }, await: [{ type: "agent_settled" }] }],
    dialogs,
    ...extra,
  };
}

async function runFake(testScenario) {
  const root = await workRoot();
  try {
    const result = await runScenario({
      scenario: testScenario,
      piCli: FAKE_AGENT,
      rpiBin: FAKE_AGENT,
      workDir: path.join(root, "work"),
      outDir: path.join(root, "out"),
    });
    return { ...result, root };
  } catch (error) {
    await rm(root, { recursive: true, force: true });
    throw error;
  }
}

async function cleanup(result) {
  if (process.env.PARITY_RPC_KEEP !== "1") await rm(result.root, { recursive: true, force: true });
}

for (const [label, message, dialog, text] of [
  ["select", "dialog:select", { kind: "select", answer: { choice: "Block" } }, "result:value=Block"],
  ["select cancelled", "dialog:select", { kind: "select", cancel: true }, "result:cancelled"],
  ["confirm", "dialog:confirm", { kind: "confirm", answer: { confirmed: true } }, "result:confirmed=true"],
  ["input", "dialog:input", { kind: "input", answer: { value: "typed" } }, "result:value=typed"],
]) {
  test(`${label} dialog is answered identically on both sides`, async () => {
    const result = await runFake(scenario(`dialog-${label}`, message, [dialog]));
    try {
      assert.equal(result.status, "identical", JSON.stringify(result.summary.error ?? result.summary.mismatches));
      assert.equal(result.exitCode, 0);
      const pi = JSON.parse(await readFile(path.join(result.outDir, "pi.normalized.json"), "utf8"));
      const rpi = JSON.parse(await readFile(path.join(result.outDir, "rpi.normalized.json"), "utf8"));
      assert.deepEqual(pi, rpi);
      assert.ok(
        pi.some((record) => record.type === "extension_ui_request" && record.message === text),
        `the notify carries ${text}`,
      );
    } finally {
      await cleanup(result);
    }
  });
}

test("a dialog of the wrong kind fails the run with a dialog-mismatch error", async () => {
  const result = await runFake(scenario("kind-mismatch", "dialog:select", [{ kind: "confirm", answer: { confirmed: true } }]));
  try {
    assert.equal(result.status, "error");
    assert.equal(result.summary.error.code, "dialog-mismatch");
    assert.equal(result.exitCode, 2);
  } finally {
    await cleanup(result);
  }
});

test("a dialog with no scripted answer fails the run", async () => {
  const result = await runFake(scenario("unscripted", "dialog:select", []));
  try {
    assert.equal(result.status, "error");
    assert.equal(result.summary.error.code, "dialog");
    assert.match(result.summary.error.message, /unscripted dialog request/);
  } finally {
    await cleanup(result);
  }
});

test("a scripted dialog that is never requested fails the run", async () => {
  const result = await runFake(scenario("unused", "plain", [{ kind: "select", answer: { choice: "Allow" } }]));
  try {
    assert.equal(result.status, "error");
    assert.match(result.summary.error.message, /never requested/);
  } finally {
    await cleanup(result);
  }
});

test("an event field outside the allowlist fails the run with unknown-field", async () => {
  const result = await runFake(scenario("unknown-field", "unknown-field"));
  try {
    assert.equal(result.status, "error");
    assert.equal(result.summary.error.code, "unknown-field");
  } finally {
    await cleanup(result);
  }
});

test("the same scenario produces byte-identical summaries across runs", async () => {
  const base = scenario("repeat", "dialog:select", [{ kind: "select", answer: { choice: "Allow" } }]);
  const first = await runFake(base);
  const second = await runFake(base);
  try {
    const a = await readFile(path.join(first.outDir, "summary.json"), "utf8");
    const b = await readFile(path.join(second.outDir, "summary.json"), "utf8");
    assert.equal(a, b);
    assert.equal(first.status, "identical");
  } finally {
    await cleanup(first);
    await cleanup(second);
  }
});

test("scenario validation rejects malformed input", () => {
  const good = scenario("ok", "plain");
  assert.equal(validateScenario(good).name, "ok");
  assert.throws(() => validateScenario({ ...good, provider: { replies: [] } }), /replies/);
  assert.throws(() => validateScenario({ ...good, steps: [] }), /steps/);
  assert.throws(() => validateScenario({ ...good, dialogs: [{ kind: "editor", answer: {} }] }), /kind/);
  assert.throws(() => validateScenario({ ...good, dialogs: [{ kind: "select" }] }), /answer or cancel/);
  assert.throws(() => validateScenario({ ...good, provider: { replies: ["x"], chunkChars: 0 } }), /chunkChars/);
});
