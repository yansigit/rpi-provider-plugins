import type { Socket } from "node:net";

export const VERSION = 1;
export const EVENT_CAPACITY = 64;
export const PAYLOAD_BYTES = 4 * 1024 * 1024;
export const MAX_FRAME_BYTES = 64 * 1024 * 1024;
export const MAX_CALLS = 16;

export interface Bootstrap { version: 1; generation: string; port: number; token: string }
export function validateBootstrap(value: unknown): Bootstrap {
	const data = value as Partial<Bootstrap> | undefined;
	if (!data || data.version !== VERSION || typeof data.generation !== "string" || !/^[a-f0-9]{32}$/.test(data.generation) ||
		typeof data.port !== "number" || !Number.isInteger(data.port) || data.port < 1 || data.port > 65535 ||
		typeof data.token !== "string" || !/^[a-f0-9]{64}$/.test(data.token) || Object.keys(data).some(key => !["version", "generation", "port", "token"].includes(key))) throw new Error("Invalid worker bootstrap");
	return data as Bootstrap;
}


export type Frame = {
	version: 1;
	generation: string;
	id: string;
} & (
	| { kind: "request"; method: string; input: unknown; parent?: string }
	| { kind: "reply"; result: unknown; lastEvent: number }
	| { kind: "error"; error: string; lastEvent: number }
	| { kind: "event"; sequence: number; event: unknown }
	| { kind: "credit"; sequence: number }
	| { kind: "cancel" }
);

function record(value: unknown): value is Record<string, unknown> {
	return !!value && typeof value === "object" && !Array.isArray(value);
}

export function parseFrame(value: unknown, generation: string): Frame {
	if (!record(value) || value.version !== VERSION || value.generation !== generation ||
		typeof value.id !== "string" || !/^[hw][1-9][0-9]{0,15}$/.test(value.id)) throw new Error("Invalid worker frame identity");
	let keys: string[];
	switch (value.kind) {
		case "request":
			if (typeof value.method !== "string" || !value.method || value.method.length > 128 || !("input" in value) ||
				(value.parent !== undefined && (typeof value.parent !== "string" || !/^h[1-9][0-9]{0,15}$/.test(value.parent)))) throw new Error("Invalid worker request");
			keys = ["method", "input", "parent"];
			break;
		case "reply":
		case "error":
			if (!Number.isSafeInteger(value.lastEvent) || (value.lastEvent as number) < 0 ||
				(value.kind === "reply" ? !("result" in value) : typeof value.error !== "string")) throw new Error("Invalid worker completion");
			keys = value.kind === "reply" ? ["result", "lastEvent"] : ["error", "lastEvent"];
			break;
		case "event":
		case "credit":
			if (!Number.isSafeInteger(value.sequence) || (value.sequence as number) < 1 ||
				(value.kind === "event" && !("event" in value))) throw new Error("Invalid worker event sequence");
			keys = value.kind === "event" ? ["sequence", "event"] : ["sequence"];
			break;
		case "cancel": keys = []; break;
		default: throw new Error("Invalid worker frame kind");
	}
	if (Object.keys(value).some(key => !["version", "generation", "id", "kind", ...keys].includes(key))) throw new Error("Unknown worker frame field");
	return value as Frame;
}

export function encodeFrame(value: unknown): Buffer<ArrayBuffer> {
	const json = JSON.stringify(value);
	if (json === undefined) throw new Error("Worker frame is not JSON");
	const size = Buffer.byteLength(json);
	if (size === 0 || size > MAX_FRAME_BYTES) throw new Error("Worker frame exceeds size limit");
	const frame = Buffer.allocUnsafeSlow(size + 4);
	frame.writeUInt32BE(size);
	frame.write(json, 4);
	return frame;
}

/** Incremental framing retains at most one bounded frame, without repeated concatenation. */
export class FrameReader {
	private prefix = Buffer.alloc(4);
	private prefixBytes = 0;
	private body?: Buffer;
	private bodyBytes = 0;
	push(chunk: Buffer, receive: (value: unknown) => void): void {
		let offset = 0;
		while (offset < chunk.length) {
			if (!this.body) {
				const size = Math.min(4 - this.prefixBytes, chunk.length - offset);
				chunk.copy(this.prefix, this.prefixBytes, offset, offset + size);
				this.prefixBytes += size; offset += size;
				if (this.prefixBytes !== 4) continue;
				const length = this.prefix.readUInt32BE();
				if (length === 0 || length > MAX_FRAME_BYTES) throw new Error("Invalid worker frame length");
				this.body = Buffer.allocUnsafe(length);
			}
			const size = Math.min(this.body.length - this.bodyBytes, chunk.length - offset);
			chunk.copy(this.body, this.bodyBytes, offset, offset + size);
			this.bodyBytes += size; offset += size;
			if (this.bodyBytes === this.body.length) {
				const value: unknown = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(this.body));
				this.body = undefined; this.bodyBytes = 0; this.prefixBytes = 0;
				receive(value);
			}
		}
	}
	end(): void {
		if (this.prefixBytes || this.body) throw new Error("Truncated worker frame");
	}
}

/** Replies/cancellation bypass stream capacity; a physical frame is never interleaved. */
export class FrameWriter {
	private control: Buffer[] = [];
	private events: Buffer[] = [];
	private writing = false;
	private stopped = false;
	private eventBytes = 0;
	private controlBytes = 0;
	constructor(private socket: Socket, private failed: (error: Error) => void) {}
	write(frame: Buffer, event = false): void {
		if (this.stopped) throw new Error("Worker connection is closed");
		// Stream producers have separate credits. Bound control traffic as well.
		if (!event && this.control.length >= MAX_CALLS * 4) throw new Error("Worker control queue is full");
		const bytes = event ? this.eventBytes : this.controlBytes;
		const limit = (MAX_FRAME_BYTES + 4) * MAX_CALLS * (event ? 1 : 2);
		if (bytes + frame.length > limit) throw new Error("Worker write queue exceeds size limit");
		(event ? this.events : this.control).push(frame);
		if (event) this.eventBytes += frame.length; else this.controlBytes += frame.length;
		this.flush();
	}
	close(): void { this.stopped = true; this.control = []; this.events = []; this.eventBytes = 0; this.controlBytes = 0; }
	private flush(): void {
		if (this.writing || this.stopped) return;
		const control = this.control.length > 0;
		const next = (control ? this.control : this.events).shift();
		if (!next) return;
		if (control) this.controlBytes -= next.length; else this.eventBytes -= next.length;
		this.writing = true;
		this.socket.write(next, error => {
			this.writing = false;
			if (error) this.failed(error);
			else this.flush();
		});
	}
}
