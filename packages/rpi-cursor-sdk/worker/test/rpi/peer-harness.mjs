import assert from "node:assert/strict";
import { once } from "node:events";
import { spawn } from "node:child_process";
import { randomBytes } from "node:crypto";
import { createServer } from "node:net";
import { fileURLToPath } from "node:url";
import { encodeFrame, FrameReader } from "../../dist/rpi/protocol.js";

// A worker that dies during bootstrap (for example a missing runtime
// node_modules) never connects; without these bounds the harness waited forever
// instead of reporting the child's own diagnostics. Every wait below is a race
// against the child's exit and a deadline, and nothing here creates a promise
// that can reject without a caller awaiting it.
const HANDSHAKE_MS = 10_000;
const FRAME_MS = 10_000;
const STDERR_BYTES = 4096;

function delay(ms) { return new Promise(resolve => { const timer = setTimeout(resolve, ms); timer.unref?.(); }); }

export async function start(t, { entry = new URL("./fixture-worker.mjs", import.meta.url), cwd } = {}) {
	const server = createServer(); server.listen(0, "127.0.0.1"); await once(server, "listening");
	const config = { version: 1, port: server.address().port, generation: randomBytes(16).toString("hex"), token: randomBytes(32).toString("hex") };
	const connection = once(server, "connection");
	const child = spawn(process.execPath, [fileURLToPath(entry)], { cwd, stdio: ["pipe", "pipe", "pipe"] });
	let stdout = ""; let stderr = "";
	child.stdout.on("data", chunk => { stdout += chunk; });
	child.stderr.on("data", chunk => { stderr += chunk; });
	const output = () => `${stdout}${stderr}`;
	const tail = () => stderr.slice(-STDERR_BYTES) || "<empty>";
	const dead = () => child.exitCode !== null || child.signalCode !== null;
	const state = () => dead() ? `worker exited code=${child.exitCode} signal=${child.signalCode}` : "worker running";
	const failure = (phase) => new Error(`${phase} (${state()})\nstderr: ${tail()}`);
	const exited = once(child, "exit");
	// Resolves with the exit status instead of rejecting, so it can be raced freely.
	const exitInfo = exited.then(([code, signal]) => ({ code, signal }));
	let socket;
	t.after(async () => {
		socket?.destroy(); server.close();
		if (!dead()) child.kill();
		await Promise.race([exited, delay(2_000)]);
	});
	child.stdin.end(JSON.stringify(config));
	const connected = await Promise.race([
		connection.then(value => ({ socket: value[0] })),
		exitInfo.then(exit => ({ exit })),
		delay(HANDSHAKE_MS).then(() => ({ timeout: true })),
	]);
	if (!connected.socket) throw failure(connected.exit ? "worker exited before it connected" : `worker did not connect within ${HANDSHAKE_MS}ms`);
	socket = connected.socket;
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
	const take = async (predicate, timeoutMs = FRAME_MS) => {
		const deadline = Date.now() + timeoutMs;
		for (;;) {
			const index = frames.findIndex(predicate);
			if (index >= 0) return frames.splice(index, 1)[0];
			if (dead()) throw failure(`no frame matched within ${timeoutMs}ms`);
			const remaining = deadline - Date.now();
			if (remaining <= 0) {
				throw new Error(`no frame matched within ${timeoutMs}ms (${state()})\nstderr: ${tail()}\nframes: ${JSON.stringify(frames).slice(0, 2000)}`);
			}
			await Promise.race([new Promise(resolve => waiters.push(resolve)), exitInfo, delay(remaining)]);
		}
	};
	const handshake = await Promise.race([
		authenticated.then(() => ({ ready: true })),
		exitInfo.then(exit => ({ exit })),
		delay(HANDSHAKE_MS).then(() => ({ timeout: true })),
	]);
	if (handshake.ready !== true) throw failure(handshake.exit ? "worker exited before the handshake completed" : `worker handshake timed out after ${HANDSHAKE_MS}ms`);
	return { send, take, frames, socket, child, exited, output, config };
}
