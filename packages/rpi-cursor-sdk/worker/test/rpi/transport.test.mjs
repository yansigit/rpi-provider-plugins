import assert from "node:assert/strict";
import { once } from "node:events";
import { spawn } from "node:child_process";
import { randomBytes } from "node:crypto";
import { createServer } from "node:net";
import { setTimeout as delay } from "node:timers/promises";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { encodeFrame, FrameReader, EVENT_CAPACITY, PAYLOAD_BYTES } from "../../dist/rpi/protocol.js";

test("invalid bootstrap does not echo private input in process errors", { timeout: 10_000 }, async () => {
	const child = spawn(process.execPath, [fileURLToPath(new URL("./fixture-worker.mjs", import.meta.url))], { stdio: ["pipe", "pipe", "pipe"] });
	let output = ""; child.stdout.on("data", chunk => { output += chunk; }); child.stderr.on("data", chunk => { output += chunk; });
	const exited = once(child, "close");
	child.stdin.end('{"token":"private-fixture-connection-secret",invalid}');
	const [code] = await exited;
	assert.notEqual(code, 0); assert.match(output, /Invalid worker bootstrap/);
	assert.doesNotMatch(output, /private-fixture-connection-secret/);
});

async function start(t) {
	const server = createServer(); server.listen(0, "127.0.0.1"); await once(server, "listening");
	const config = { version: 1, port: server.address().port, generation: randomBytes(16).toString("hex"), token: randomBytes(32).toString("hex") };
	const connection = once(server, "connection");
	const child = spawn(process.execPath, [fileURLToPath(new URL("./fixture-worker.mjs", import.meta.url))], { stdio: ["pipe", "pipe", "pipe"] });
	let output = ""; child.stdout.on("data", chunk => { output += chunk; }); child.stderr.on("data", chunk => { output += chunk; });
	const exited = once(child, "exit");
	child.stdin.end(JSON.stringify(config));
	const [socket] = await connection;
	t.after(async () => { socket.destroy(); server.close(); if (child.exitCode === null) child.kill(); await exited; });
	const frames = []; const waiters = []; const reader = new FrameReader();
	let hello;
	let ready; const authenticated = new Promise(resolve => { ready = resolve; });
	socket.on("data", chunk => reader.push(chunk, frame => {
		if (!hello) {
			assert.deepEqual(frame, { version: 1, generation: config.generation, token: config.token }); hello = true;
			socket.write(encodeFrame({ version: 1, generation: config.generation, ready: true })); ready();
		} else { frames.push(frame); for (const wake of waiters.splice(0)) wake(); }
	}));
	const send = frame => socket.write(encodeFrame({ version: 1, generation: config.generation, ...frame }));
	const take = async predicate => {
		for (;;) {
			const index = frames.findIndex(predicate);
			if (index >= 0) return frames.splice(index, 1)[0];
			await new Promise(resolve => waiters.push(resolve));
		}
	};
	await authenticated;
	return { send, take, frames, socket, exited, output: () => output, config };
}

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
