import assert from "node:assert/strict";
import { once } from "node:events";
import { spawn } from "node:child_process";
import { randomBytes } from "node:crypto";
import { createServer } from "node:net";
import { fileURLToPath } from "node:url";
import { encodeFrame, FrameReader } from "../../dist/rpi/protocol.js";

export async function start(t, { entry = new URL("./fixture-worker.mjs", import.meta.url), cwd } = {}) {
	const server = createServer(); server.listen(0, "127.0.0.1"); await once(server, "listening");
	const config = { version: 1, port: server.address().port, generation: randomBytes(16).toString("hex"), token: randomBytes(32).toString("hex") };
	const connection = once(server, "connection");
	const child = spawn(process.execPath, [fileURLToPath(entry)], { cwd, stdio: ["pipe", "pipe", "pipe"] });
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

