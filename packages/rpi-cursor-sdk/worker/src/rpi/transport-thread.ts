import { connect } from "node:net";
import { parentPort, workerData } from "node:worker_threads";
import { encodeFrame, FrameReader, FrameWriter, MAX_CALLS, parseFrame, VERSION, type Frame, validateBootstrap } from "./protocol.js";
import { StreamBudget } from "./stream-budget.js";


interface Incoming { budget: StreamBudget; credits: Map<number, number>; sequence: number; completed: boolean }
type Outgoing = { kind: "frame"; frame: Frame; encoded?: Uint8Array; weight?: number };

function run(): void {
	if (!parentPort) return;
	const parent = parentPort;
	const config = validateBootstrap(workerData);
	const incoming = new Map<string, Incoming>();
	const outgoing = new Map<string, string>();
	let lastHostId = 0n;
	let lastWorkerId = 0n;
	let authenticated = false;
	let stopped = false;
	const socket = connect({ host: "127.0.0.1", port: config.port });
	socket.setNoDelay(true);
	const stop = () => {
		if (stopped) return;
		stopped = true;
		for (const call of incoming.values()) call.budget.cancel();
		writer.close(); socket.destroy();
		parent.postMessage({ kind: "closed" }); parent.close();
	};
	const writer = new FrameWriter(socket, stop);
	const reader = new FrameReader();
	const handshakeTimeout = setTimeout(stop, 10_000);
	socket.on("connect", () => writer.write(encodeFrame({ version: VERSION, generation: config.generation, token: config.token })));
	socket.on("error", stop);
	socket.on("close", () => { clearTimeout(handshakeTimeout); stop(); });
	socket.on("end", () => { try { reader.end(); } catch { /* Disconnect also covers incomplete framing. */ } finally { stop(); } });
	socket.on("data", chunk => {
		try {
			reader.push(chunk, value => {
				if (!authenticated) {
					const ready = value as Record<string, unknown>;
					if (!ready || ready.version !== VERSION || ready.generation !== config.generation || ready.ready !== true || Object.keys(ready).length !== 3) throw new Error("Invalid worker handshake");
					authenticated = true; clearTimeout(handshakeTimeout); parent.postMessage({ kind: "ready" }); return;
				}
				const frame = parseFrame(value, config.generation);
				if (frame.kind === "request") {
					if (!frame.id.startsWith("h") || frame.parent !== undefined || incoming.size >= MAX_CALLS || BigInt(frame.id.slice(1)) <= lastHostId) throw new Error("Invalid worker call admission");
					lastHostId = BigInt(frame.id.slice(1));
					const budget = new StreamBudget();
					incoming.set(frame.id, { budget, sequence: 0, credits: new Map(), completed: false });
					parent.postMessage({ kind: "request", frame, shared: budget.shared });
				} else if (frame.kind === "credit") {
					const call = incoming.get(frame.id);
					const weight = call?.credits.get(frame.sequence);
					if (!call || !weight) throw new Error("Unknown worker event credit");
					call.credits.delete(frame.sequence); call.budget.release(weight);
					if (call.completed && !call.credits.size) incoming.delete(frame.id);
				} else if (frame.kind === "cancel") {
					if (!frame.id.startsWith("h") || BigInt(frame.id.slice(1)) > lastHostId) throw new Error("Unknown worker cancellation");
					incoming.get(frame.id)?.budget.cancel();
					for (const [id, owner] of outgoing) {
						if (owner === frame.id) writer.write(encodeFrame({ version: VERSION, generation: config.generation, id, kind: "cancel" }));
					}
					parent.postMessage({ kind: "cancel", id: frame.id });
				} else if (frame.kind === "reply" || frame.kind === "error") {
					if (frame.lastEvent !== 0 || !outgoing.delete(frame.id)) throw new Error("Unknown worker host reply");
					parent.postMessage({ kind: "reply", frame });
				} else throw new Error("Unexpected host stream event");
			});
		} catch { stop(); }
	});
	parent.on("message", (message: Outgoing) => {
		if (stopped) return;
		try {
			if (!authenticated) throw new Error("Worker handshake incomplete");
			const frame = parseFrame(message.frame, config.generation);
			if (frame.kind === "request") {
				if (!frame.id.startsWith("w") || BigInt(frame.id.slice(1)) <= lastWorkerId || outgoing.size >= MAX_CALLS || !frame.parent || !incoming.has(frame.parent)) throw new Error("Invalid worker host call");
				lastWorkerId = BigInt(frame.id.slice(1));
				outgoing.set(frame.id, frame.parent);
			} else if (frame.kind === "event") {
				const call = incoming.get(frame.id);
				if (!call || call.completed || frame.sequence !== call.sequence + 1 || !message.weight || !message.encoded) throw new Error("Invalid worker stream emission");
				call.sequence = frame.sequence; call.credits.set(frame.sequence, message.weight);
			} else if (frame.kind === "reply" || frame.kind === "error") {
				const call = incoming.get(frame.id);
				if (!call || call.completed || frame.lastEvent !== call.sequence) throw new Error("Invalid worker completion");
				call.completed = true;
				if (!call.credits.size) incoming.delete(frame.id);
			} else if (frame.kind === "cancel") {
				if (!frame.id.startsWith("w") || BigInt(frame.id.slice(1)) > lastWorkerId) throw new Error("Invalid outgoing worker cancellation");
				// The IO thread may have received the reply before the adapter observed its abort.
				if (!outgoing.has(frame.id)) return;
			} else throw new Error("Invalid outgoing worker frame");
			writer.write(message.encoded ? Buffer.from(message.encoded.buffer, message.encoded.byteOffset, message.encoded.byteLength) : encodeFrame(frame), frame.kind === "event");
		} catch { stop(); }
	});
}

run();
