#!/usr/bin/env node
/**
 * Purpose: Prove the staged sidecar runtime executes from its archive layout.
 * Responsibilities: Stage `.package/runtime`, point its node_modules at the installed
 * runtime dependencies, open the packaged launcher through the real peer harness, and
 * assert the retained adapter registers. No Cursor SDK call and no network access.
 * Usage: `node --test tests/r01-package-runtime.mjs`.
 * Invariants: Only reads the local worker build and the local runtime install.
 */
import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdtemp, readFile, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { test } from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";
import { promisify } from "node:util";

const run = promisify(execFile);
const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const staged = join(root, ".package");
const entry = join(staged, "runtime", "main.mjs");
const { start } = await import(join(root, "worker", "test", "rpi", "peer-harness.mjs"));

async function stage() {
	await run(process.execPath, [join(root, "scripts", "stage-package-runtime.mjs"), "--out", staged], { cwd: root });
	await rm(join(staged, "runtime", "node_modules"), { recursive: true, force: true });
	await symlink(join(root, "runtime", "node_modules"), join(staged, "runtime", "node_modules"), "dir");
}

test("packaged runtime opens with external npm dependencies from the sidecar layout", { timeout: 30_000 }, async t => {
	await stage();
	const cwd = await mkdtemp(join(tmpdir(), "rpi-cursor-packaged-"));
	t.after(() => rm(cwd, { recursive: true, force: true }));
	const peer = await start(t, { entry: pathToFileURL(entry), cwd });
	peer.send({
		id: "h1",
		kind: "request",
		method: "runtime.open",
		input: { cwd, agentDir: join(cwd, "rpi-agent"), sessionDir: join(cwd, "sessions") },
	});
	const ready = await peer.take(frame => frame.id === "h1" && frame.kind === "event");
	assert.equal(ready.event.type, "ready", JSON.stringify(ready));
	assert.equal(ready.event.manifest.provider.id, "cursor");
	assert.equal(ready.event.manifest.commands.length, 10);
	assert.ok(ready.event.manifest.flags.some(flag => flag.name === "cursor-no-fast"));
	assert.ok(ready.event.manifest.tools.some(tool => tool.name === "cursor_activate_skill"));
	peer.send({ id: "h1", kind: "cancel" });
});

test("the retained dispatcher suite passes against the bundled sidecar runtime", { timeout: 120_000 }, async () => {
	await stage();
	// Reuse the retained suite by moving it beside the packaged runtime: its
	// relative harness import still resolves, and only the launcher URL changes.
	const source = await readFile(join(root, "tests", "native-runtime.mjs"), "utf8");
	const entry = 'new URL("../runtime/main.mjs", import.meta.url)';
	if (!source.includes(entry)) throw new Error("native-runtime.mjs no longer names the staged launcher");
	const generated = join(staged, "native-runtime-packaged.mjs");
	await writeFile(generated, source.split(entry).join('new URL("./runtime/main.mjs", import.meta.url)'));
	const environment = { ...process.env };
	delete environment.NODE_TEST_CONTEXT;
	try {
		const { stdout } = await run(process.execPath, ["--test", generated], {
			cwd: root,
			env: environment,
			timeout: 110_000,
		});
		if (!stdout.includes("pass 2") || !stdout.includes("fail 0")) throw new Error(`nested suite did not pass:\n${stdout}`);
	} finally {
		await rm(generated, { force: true });
	}
});
