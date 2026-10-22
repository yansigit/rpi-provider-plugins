// End to end: real Pi 1.3.0 and real rpi, one scripted provider, one prompt.
//
// Transcripts are NOT identical today; the pinned summary records exactly how
// they differ. The test fails on any new or resolved difference, so a change on
// either side must update the pin on purpose:
//   PARITY_RPC_WRITE_EXPECTED=1 node --test tools/parity-rpc/test/e2e.test.mjs
// Paths: PARITY_PI_CLI (Pi cli.js) and PARITY_RPI_BIN (rpi binary).
import assert from "node:assert/strict";
import { existsSync } from "node:fs";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { homedir } from "node:os";
import path from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { runScenario } from "../run.mjs";
import { canonicalJson } from "../normalize.mjs";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const TOOL = path.join(HERE, "..");
const SCENARIO = path.join(TOOL, "fixtures", "scenarios", "prompt-basic.json");
const EXPECTED = path.join(TOOL, "fixtures", "expected", "prompt-basic.summary.json");
const REPLY = "Hello from the scripted provider.";

const PI_CLI =
  process.env.PARITY_PI_CLI ??
  "/home/user/Projects/provider-references/integrated/node_modules/@earendil-works/pi-coding-agent/dist/bundle/cli.js";
const RPI_BIN = process.env.PARITY_RPI_BIN ?? "/home/user/Projects/.targets/x-rpc/debug/rpi";

test("prompt-basic runs on real Pi and rpi and pins their known divergences", async () => {
  assert.ok(existsSync(PI_CLI), `Pi cli.js not found at ${PI_CLI} (set PARITY_PI_CLI)`);
  assert.ok(existsSync(RPI_BIN), `rpi binary not found at ${RPI_BIN} (set PARITY_RPI_BIN)`);

  const base = path.join(homedir(), ".cache", "parity-rpc", "tests");
  await mkdir(base, { recursive: true });
  const root = await mkdtemp(path.join(base, "e2e-"));
  try {
    const scenario = JSON.parse(await readFile(SCENARIO, "utf8"));
    const result = await runScenario({
      scenario,
      piCli: PI_CLI,
      rpiBin: RPI_BIN,
      workDir: path.join(root, "work"),
      outDir: path.join(root, "out"),
    });
    assert.equal(result.summary.error, null, `runner error: ${JSON.stringify(result.summary.error)}`);
    assert.notEqual(result.status, "error");

    // Content-level parity that does hold: both sides answer with the same text.
    const pi = JSON.parse(await readFile(path.join(result.outDir, "pi.normalized.json"), "utf8"));
    const rpi = JSON.parse(await readFile(path.join(result.outDir, "rpi.normalized.json"), "utf8"));
    const replyOf = (records) =>
      records
        .filter((record) => record.type === "message_end" && record.message?.role === "assistant")
        .map((record) => record.message.content.map((block) => block.text).join(""));
    assert.deepEqual(replyOf(pi), [REPLY]);
    assert.deepEqual(replyOf(rpi), [REPLY]);

    const pinned = {
      identical: result.summary.identical,
      piRecords: result.summary.piRecords,
      rpiRecords: result.summary.rpiRecords,
      typeSequence: result.summary.typeSequence,
      mismatches: result.summary.mismatches,
    };
    if (process.env.PARITY_RPC_WRITE_EXPECTED === "1") {
      await writeFile(EXPECTED, canonicalJson(pinned));
    } else {
      assert.equal(existsSync(EXPECTED), true, "missing pin; run with PARITY_RPC_WRITE_EXPECTED=1");
      const expected = JSON.parse(await readFile(EXPECTED, "utf8"));
      assert.deepEqual(pinned, expected, "the pinned divergence set changed; review and regenerate the pin");
    }
    assert.equal(result.summary.identical, false, "transcripts are known to differ; see the pin");
  } finally {
    if (process.env.PARITY_RPC_KEEP !== "1") await rm(root, { recursive: true, force: true });
  }
});
