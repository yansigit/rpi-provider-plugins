import { start } from "./peer-harness.mjs";
import assert from "node:assert/strict";
import { once } from "node:events";
import { spawn } from "node:child_process";
import { setTimeout as delay } from "node:timers/promises";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { encodeFrame, EVENT_CAPACITY, PAYLOAD_BYTES } from "../../dist/rpi/protocol.js";

test("invalid bootstrap does not echo private input in process errors", { timeout: 10_000 }, async () => {
	const child = spawn(process.execPath, [fileURLToPath(new URL("./fixture-worker.mjs", import.meta.url))], { stdio: ["pipe", "pipe", "pipe"] });
	let output = ""; child.stdout.on("data", chunk => { output += chunk; }); child.stderr.on("data", chunk => { output += chunk; });
	const exited = once(child, "close");
	child.stdin.end('{"token":"private-fixture-connection-secret",invalid}');
	const [code] = await exited;
	assert.notEqual(code, 0); assert.match(output, /Invalid worker bootstrap/);
	assert.doesNotMatch(output, /private-fixture-connection-secret/);
});


test("private framing supports nested host calls and never consumes stdio as messages", { timeout: 10_000 }, async t => {
	const peer = await start(t);
	peer.send({ id: "h1", kind: "request", method: "nested", input: { value: 42 } });
	const request = await peer.take(f => f.kind === "request");
	assert.equal(request.parent, "h1"); assert.equal(request.method, "fixture.host");
	peer.send({ id: request.id, kind: "reply", result: { answer: request.input.value }, lastEvent: 0 });
	assert.deepEqual((await peer.take(f => f.id === "h1")).result, { answer: 42 });
	peer.send({ id: "h2", kind: "request", method: "stdio", input: null });
	assert.equal((await peer.take(f => f.id === "h2")).result, true);
	peer.send({ id: "h3", kind: "request", method: "fail", input: null });
	assert.equal((await peer.take(f => f.id === "h3")).error, "Cursor worker request failed");
	await delay(20); assert.match(peer.output(), /stdout is not IPC/); assert.match(peer.output(), /stderr is not IPC/);
	assert.doesNotMatch(peer.output(), /fixture secret/);
});

test("cancellation reaches a nested host call and its late reply is drained", { timeout: 10_000 }, async t => {
	const peer = await start(t);
	peer.send({ id: "h1", kind: "request", method: "nestedAbort", input: {} });
	const request = await peer.take(f => f.kind === "request");
	peer.send({ id: "h1", kind: "cancel" });
	assert.equal((await peer.take(f => f.kind === "cancel")).id, request.id);
	assert.equal((await peer.take(f => f.id === "h1")).kind, "error");
	peer.send({ id: request.id, kind: "reply", result: { committed: true }, lastEvent: 0 });
	peer.send({ id: "h2", kind: "request", method: "echo", input: "still connected" });
	assert.equal((await peer.take(f => f.id === "h2")).result, "still connected");
});

test("an explicit waiter abort racing an already received reply keeps the connection usable", { timeout: 10_000 }, async t => {
	const peer = await start(t);
	peer.send({ id: "h1", kind: "request", method: "nestedSignalRace", input: {} });
	const request = await peer.take(frame => frame.kind === "request");
	peer.send({ id: request.id, kind: "reply", result: { committed: true }, lastEvent: 0 });
	assert.equal((await peer.take(frame => frame.id === "h1")).kind, "error");
	peer.send({ id: "h2", kind: "request", method: "echo", input: "connected" });
	assert.equal((await peer.take(frame => frame.id === "h2")).result, "connected");
});

test("count pressure is lossless and cancellation wakes a blocked synchronous producer", { timeout: 10_000 }, async t => {
	const peer = await start(t);
	peer.send({ id: "h1", kind: "request", method: "burst", input: { count: EVENT_CAPACITY + 1 } });
	await peer.take(f => f.kind === "event" && f.sequence === EVENT_CAPACITY);
	await delay(30); assert.equal(peer.frames.filter(f => f.kind === "event").length, EVENT_CAPACITY - 1);
	assert(!peer.frames.some(f => f.kind === "reply"));
	peer.send({ id: "h1", kind: "credit", sequence: 1 });
	const last = await peer.take(f => f.kind === "event" && f.sequence === EVENT_CAPACITY + 1);
	assert.equal(last.event.i, EVENT_CAPACITY);
	assert.equal((await peer.take(f => f.kind === "reply")).lastEvent, EVENT_CAPACITY + 1);
	for (let sequence = 2; sequence <= EVENT_CAPACITY + 1; sequence++) peer.send({ id: "h1", kind: "credit", sequence });
	peer.send({ id: "h2", kind: "request", method: "burst", input: { count: EVENT_CAPACITY + 1 } });
	await peer.take(f => f.id === "h2" && f.kind === "event" && f.sequence === EVENT_CAPACITY);
	peer.send({ id: "h2", kind: "cancel" });
	const cancelled = await peer.take(f => f.id === "h2" && f.kind === "error");
	assert.equal(cancelled.lastEvent, EVENT_CAPACITY);
	peer.send({ id: "h3", kind: "request", method: "echo", input: "reader stayed live" });
	assert.equal((await peer.take(f => f.id === "h3")).result, "reader stayed live");
});

test("an oversized event consumes the byte budget exclusively; completion does not need credit", { timeout: 10_000 }, async t => {
	const peer = await start(t);
	peer.send({ id: "h1", kind: "request", method: "burst", input: { count: 2, bytes: PAYLOAD_BYTES + 100 } });
	const first = await peer.take(f => f.kind === "event"); assert.equal(first.sequence, 1);
	await delay(30); assert(!peer.frames.some(f => f.kind === "event"));
	peer.send({ id: "h1", kind: "credit", sequence: 1 });
	assert.equal((await peer.take(f => f.kind === "event")).sequence, 2);
	assert.equal((await peer.take(f => f.kind === "reply")).lastEvent, 2);
});

test("wrong-generation and duplicate-credit traffic disconnects instead of minting capacity", { timeout: 10_000 }, async t => {
	for (const violation of ["generation", "credit"]) {
		const peer = await start(t);
		peer.send({ id: "h1", kind: "request", method: "burst", input: { count: 1 } });
		await peer.take(f => f.kind === "event"); await peer.take(f => f.kind === "reply");
		if (violation === "generation") peer.send({ id: "h2", kind: "request", method: "echo", input: null, generation: "wrong" });
		else { peer.send({ id: "h1", kind: "credit", sequence: 1 }); peer.send({ id: "h1", kind: "credit", sequence: 1 }); }
		await peer.exited;
	}
});
