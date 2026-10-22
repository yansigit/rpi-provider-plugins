// R03 combined three-provider run. Requires the Pi 1.3.0 bundle and, for the
// differential case, a staged rpi side:
//
//   PARITY_RPI_BIN=<rpi>
//   PARITY_RPI_ANTIGRAVITY=<librpi_antigravity_auth.so>
//   PARITY_RPI_CURSOR=<librpi_cursor_sdk.so>
//   PARITY_RPI_RUNTIME=<packages/rpi-cursor-sdk/runtime>
//   [PARITY_RPI_MULTIPROVIDER=<librpi_multiprovider.so>]
//
// The reference case always runs. The differential case fails with a clear
// message when the candidate artifacts are not provided; it is never skipped.
import test from "node:test";
import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdtemp, rm } from "node:fs/promises";
import { homedir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const TOOL = path.join(HERE, "..", "tools", "combined", "run.mjs");
const STAGE = path.join(HERE, "..", "tools", "combined", "stage-rpi.mjs");

function runNode(args, env) {
  return new Promise((resolve) => {
    execFile(process.execPath, [TOOL, ...args], {
      env: { ...process.env, ...env },
      maxBuffer: 16 * 1024 * 1024,
    }, (error, stdout, stderr) => {
      resolve({ code: error?.code ?? 0, stdout, stderr });
    });
  });
}

function runStage(args) {
  return new Promise((resolve, reject) => {
    execFile(process.execPath, [STAGE, ...args], { maxBuffer: 16 * 1024 * 1024 }, (error, stdout) => {
      if (error) reject(error);
      else resolve(stdout.trim());
    });
  });
}

async function workDir() {
  return mkdtemp(path.join(homedir(), ".r03-test-"));
}

test("combined reference run: six Pi segments pass their self-checks", async () => {
  const work = await workDir();
  try {
    const result = await runNode(["--side", "pi", "--work", work], {});
    assert.equal(result.code, 0, `${result.stdout}\n${result.stderr}`);
    const summary = JSON.parse(
      await import("node:fs/promises").then((fs) => fs.readFile(path.join(work, "out", "summary.json"), "utf8")),
    );
    assert.deepEqual(summary.problems, []);
    assert.deepEqual(summary.normalizationErrors, []);
    assert.deepEqual(summary.segments.pi, [
      "agy-a",
      "agy-b",
      "cursor-a",
      "cursor-b",
      "cursor-pooled",
      "virtual-composer",
      "virtual-gemini",
    ]);
    assert.deepEqual(summary.side.pi.providers, ["cursor", "google", "parity-virtual"]);
    // The two negative segments must pin the upstream Cursor ownership refusal.
    assert.ok(summary.blocked.some((line) => line.includes("Cursor ownership receipt")));
  } finally {
    await rm(work, { recursive: true, force: true });
  }
});

test("combined differential run: candidate segments pass and the pinned diff matches", async () => {
  const rpi = process.env.PARITY_RPI_BIN;
  const antigravity = process.env.PARITY_RPI_ANTIGRAVITY;
  const cursor = process.env.PARITY_RPI_CURSOR;
  const runtime = process.env.PARITY_RPI_RUNTIME;
  assert.ok(rpi, "PARITY_RPI_BIN is required for the differential case");
  assert.ok(antigravity, "PARITY_RPI_ANTIGRAVITY is required for the differential case");
  assert.ok(cursor, "PARITY_RPI_CURSOR is required for the differential case");
  assert.ok(runtime, "PARITY_RPI_RUNTIME is required for the differential case");

  const work = await workDir();
  try {
    const staged = await runStage([
      "--out", path.join(work, "libs"),
      "--antigravity", antigravity,
      "--cursor", cursor,
      ...(process.env.PARITY_RPI_MULTIPROVIDER
        ? ["--multiprovider", process.env.PARITY_RPI_MULTIPROVIDER]
        : []),
      "--runtime", runtime,
    ]);
    const result = await runNode([
      "--rpi", rpi,
      "--rpi-antigravity", path.join(staged, "librpi_antigravity_auth.so"),
      "--rpi-cursor", path.join(staged, "librpi_cursor_sdk.so"),
      ...(process.env.PARITY_RPI_MULTIPROVIDER
        ? ["--rpi-multiprovider", path.join(staged, "librpi_multiprovider.so")]
        : []),
      "--work", work,
    ], {});
    assert.equal(result.code, 0, `${result.stdout}\n${result.stderr}`);

    const fs = await import("node:fs/promises");
    const summary = JSON.parse(await fs.readFile(path.join(work, "out", "summary.json"), "utf8"));
    assert.deepEqual(summary.problems, []);
    assert.deepEqual(summary.normalizationErrors, []);
    assert.deepEqual(summary.segments.rpi, ["agy-a", "agy-b", "cursor-a", "cursor-b"]);
    for (const id of ["agy-a", "agy-b", "cursor-a", "cursor-b"]) {
      assert.equal(summary.providerRequests[id].identical, true, `${id}: Antigravity request projection`);
      assert.equal(summary.comparison[id].identical, false, `${id}: transcript diff must stay pinned`);
      assert.ok(summary.comparison[id].mismatchCount > 0);
    }
    // The candidate registers no pool wrapper or virtual provider at this base.
    assert.equal(summary.side.rpi.multiproviderRegistered, false);
    assert.ok(summary.blocked.some((line) => line.includes("multiprovider pool registration")));
  } finally {
    await rm(work, { recursive: true, force: true });
  }
});
