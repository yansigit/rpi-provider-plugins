import { AsyncLocalStorage } from "node:async_hooks";
import { Worker } from "node:worker_threads";
import { encodeFrame, MAX_CALLS, VERSION, type Frame, validateBootstrap, type Bootstrap } from "./protocol.js";
import { StreamBudget } from "./stream-budget.js";

export interface WorkerCall {
	readonly id: string;
	readonly signal: AbortSignal;
	emit(event: unknown): void;
	/** Optional waiter cancellation; admitted persistence uses null and awaits the host receipt. */
	host(method: string, input: unknown, signal?: AbortSignal | null): Promise<unknown>;
}
export type WorkerHandler = (method: string, input: unknown, call: WorkerCall) => Promise<unknown>;
type Deferred = { resolve: (value: unknown) => void; reject: (error: Error) => void };
type Inbound =
	| { kind: "ready" | "closed" }
	| { kind: "request"; frame: Extract<Frame, { kind: "request" }>; shared: SharedArrayBuffer }
	| { kind: "reply"; frame: Extract<Frame, { kind: "reply" | "error" }> }
	| { kind: "cancel"; id: string };

/** The adapter owns this thread; the dedicated transport remains live during synchronous SDK callbacks. */
export class WorkerPeer {
	private worker: Worker;
	private calls = new Map<string, { controller: AbortController; budget: StreamBudget }>();
	private pending = new Map<string, Deferred>();
	private nextId = 0;
	private closed = false;
	private readyDeferred!: { resolve: () => void; reject: (error: Error) => void };
	private closedResolve!: () => void;
	readonly ready = new Promise<void>((resolve, reject) => { this.readyDeferred = { resolve, reject }; });
	readonly disconnected = new Promise<void>(resolve => { this.closedResolve = resolve; });
	readonly current = new AsyncLocalStorage<WorkerCall>();
	constructor(private config: Bootstrap, private handler: WorkerHandler, private errorText: (error: unknown) => string = () => "Cursor worker request failed") {
		validateBootstrap(config);
		this.worker = new Worker(new URL("./transport-thread.js", import.meta.url), { workerData: config });
		this.worker.on("message", (message: Inbound) => {
			switch (message.kind) {
				case "ready": this.readyDeferred.resolve(); break;
				case "closed": this.disconnect(); break;
				case "cancel": this.calls.get(message.id)?.controller.abort(); break;
				case "reply": {
					const pending = this.pending.get(message.frame.id);
					if (!pending) { this.disconnect(); return; }
					this.pending.delete(message.frame.id);
					if (message.frame.kind === "reply") pending.resolve(message.frame.result);
					else pending.reject(new Error(message.frame.error));
					break;
				}
				case "request": void this.receive(message.frame, message.shared); break;
			}
		});
		this.worker.on("error", () => this.disconnect());
		this.worker.on("exit", () => this.disconnect());
	}
	private send(frame: Frame): void {
		if (this.closed) throw new Error("Cursor worker disconnected");
		// Validate/size-check before posting: structured-clone queues are otherwise unbounded.
		const encoded = encodeFrame(frame);
		this.worker.postMessage({ kind: "frame", frame, encoded }, [encoded.buffer]);
	}
	private async receive(frame: Extract<Frame, { kind: "request" }>, shared: SharedArrayBuffer): Promise<void> {
		const controller = new AbortController();
		const budget = new StreamBudget(shared);
		this.calls.set(frame.id, { controller, budget });
		let sequence = 0;
		let complete = false;
		const envelope = { version: VERSION, generation: this.config.generation, id: frame.id } as const;
		const call: WorkerCall = {
			id: frame.id, signal: controller.signal,
			emit: event => {
				if (complete || this.closed) throw new Error("Cursor worker stream is closed");
				const outgoing: Frame = { ...envelope, kind: "event", sequence: sequence + 1, event };
				const encoded = encodeFrame(outgoing);
				const weight = budget.acquire(encoded.length - 4);
				sequence++;
				// Only the encoded frame is transferred; a placeholder avoids cloning the payload twice.
				this.worker.postMessage({ kind: "frame", frame: { ...outgoing, event: null }, encoded, weight }, [encoded.buffer]);
			},
			host: (method, input, signal = null) => {
				if (complete || this.closed) return Promise.reject(new Error("Cursor worker call is closed"));
				if (signal?.aborted) return Promise.reject(signal.reason);
				if (this.pending.size >= MAX_CALLS) return Promise.reject(new Error("Cursor worker host call capacity exhausted"));
				if (this.nextId === Number.MAX_SAFE_INTEGER) return Promise.reject(new Error("Cursor worker request IDs exhausted"));
				const id = `w${++this.nextId}`;
				return new Promise((resolve, reject) => {
					const abort = () => {
						try { if (signal !== controller.signal) this.send({ ...envelope, id, kind: "cancel" }); }
						catch { this.disconnect(); }
						reject(signal?.reason);
					};
					const cleanup = () => signal?.removeEventListener("abort", abort);
					this.pending.set(id, {
						resolve: value => { cleanup(); resolve(value); },
						reject: error => { cleanup(); reject(error); },
					});
					try { this.send({ ...envelope, id, kind: "request", method, input, parent: frame.id }); }
					catch (error) { this.pending.delete(id); cleanup(); reject(error); return; }
					signal?.addEventListener("abort", abort, { once: true });
					if (signal?.aborted) abort();
				});
			},
		};
		try {
			const result = await this.current.run(call, () => this.handler(frame.method, frame.input, call));
			this.send({ ...envelope, kind: "reply", result: result ?? null, lastEvent: sequence });
		} catch (error) {
			if (!this.closed) {
				try { this.send({ ...envelope, kind: "error", error: this.errorText(error), lastEvent: sequence }); }
				catch { this.disconnect(); }
			}
		} finally { complete = true; this.calls.delete(frame.id); }
	}
	private disconnect(): void {
		if (this.closed) return;
		this.closed = true;
		const error = new Error("Cursor worker disconnected");
		this.readyDeferred.reject(error);
		for (const { controller, budget } of this.calls.values()) { budget.cancel(); controller.abort(error); }
		for (const pending of this.pending.values()) pending.reject(error);
		this.pending.clear(); this.closedResolve();
	}
	async close(): Promise<void> {
		this.disconnect();
		await this.worker.terminate();
	}
}

/** Bootstrap data travels through private stdin, never argv, environment, or logs. */
export async function readBootstrap(): Promise<Bootstrap> {
	if (Number(process.versions.node.split(".")[0]) < 24) throw new Error("rpi-cursor-sdk requires Node 24 or newer");
	const chunks: Buffer[] = [];
	let size = 0;
	for await (const chunk of process.stdin) {
		const bytes = Buffer.from(chunk as Uint8Array);
		size += bytes.length;
		if (size > 4096) throw new Error("Worker bootstrap exceeds size limit");
		chunks.push(bytes);
	}
	let value: unknown;
	try { value = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(Buffer.concat(chunks))); }
	catch { throw new Error("Invalid worker bootstrap"); }
	return validateBootstrap(value);
}
