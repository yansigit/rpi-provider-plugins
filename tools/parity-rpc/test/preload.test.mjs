import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import path from "node:path";
import { test } from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";
import { promisify } from "node:util";

const run = promisify(execFile);
const HERE = path.dirname(fileURLToPath(import.meta.url));
const PRELOAD = pathToFileURL(path.join(HERE, "..", "preload.mjs")).href;

// Prints a fixed sequence of clock and randomness reads as JSON.
const PROBE = `
  const reads = [
    Date.now(),
    new Date().getTime(),
    Math.random(),
    crypto.randomUUID(),
    globalThis.crypto.randomUUID(),
    crypto.randomBytes(8).toString("hex"),
    Array.from(globalThis.crypto.getRandomValues(new Uint8Array(6))).join(","),
    Date.now(),
  ];
  process.stdout.write(JSON.stringify(reads));
`;

async function probe() {
  const { stdout } = await run(process.execPath, ["--import", PRELOAD, "--input-type=module", "-e", `import crypto from "node:crypto";${PROBE}`]);
  return JSON.parse(stdout);
}

test("two processes see the same clock and randomness", async () => {
  const first = await probe();
  const second = await probe();
  assert.deepEqual(first, second);
});

test("clock advances per read and randomUUID has the v4 shape", async () => {
  const [now1, date1, , uuid1, uuid2, , , now2] = await probe();
  assert.equal(date1, now1 + 1, "new Date() shares the Date.now() counter");
  assert.equal(now2 > date1, true, "the clock is strictly increasing");
  assert.match(uuid1, /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
  assert.notEqual(uuid1, uuid2, "successive UUIDs differ");
});
