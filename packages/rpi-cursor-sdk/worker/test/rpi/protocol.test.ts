import { describe, expect, it } from "vitest";
import { encodeFrame, FrameReader, FrameWriter, MAX_FRAME_BYTES, parseFrame, validateBootstrap } from "../../src/rpi/protocol.js";
import type { Socket } from "node:net";

describe("rpi worker framing", () => {
	it("reads split prefixes, UTF-8 bodies and adjacent frames without losing boundaries", () => {
		const data = [{ text: "a🦀z" }, null, [1, 2, 3]];
		const bytes = Buffer.concat(data.map(encodeFrame));
		for (let width = 1; width <= bytes.length; width++) {
			const reader = new FrameReader(); const actual: unknown[] = [];
			for (let i = 0; i < bytes.length; i += width) reader.push(bytes.subarray(i, i + width), value => actual.push(value));
			reader.end(); expect(actual).toEqual(data);
		}
	});
	it("rejects oversized, empty, truncated, non-UTF-8 and invalid JSON frames", () => {
		for (const size of [0, MAX_FRAME_BYTES + 1]) {
			const bytes = Buffer.alloc(4); bytes.writeUInt32BE(size);
			expect(() => new FrameReader().push(bytes, () => {})).toThrow("frame length");
		}
		for (const bytes of [Buffer.from([0, 0]), encodeFrame({ value: 1 }).subarray(0, -1)]) {
			const reader = new FrameReader(); reader.push(bytes, () => {}); expect(() => reader.end()).toThrow("Truncated");
		}
		for (const body of [Buffer.from([34, 0xff, 34]), Buffer.from("invalid")]) {
			const prefix = Buffer.alloc(4); prefix.writeUInt32BE(body.length);
			expect(() => new FrameReader().push(Buffer.concat([prefix, body]), () => {})).toThrow();
		}
	});
	it("refuses generation changes, ambiguous fields and invalid identifiers", () => {
		const frame = { version: 1, generation: "owner", id: "h1", kind: "request", method: "native", input: null };
		expect(parseFrame(frame, "owner")).toEqual(frame);
		for (const patch of [{ generation: "other" }, { id: "h0" }, { id: "w-1" }, { version: 2 }, { token: "secret" }, { kind: "reply", lastEvent: -1 }]) {
			expect(() => parseFrame({ ...frame, ...patch }, "owner")).toThrow();
		}
		expect(() => validateBootstrap({ version: 1, generation: "a".repeat(32), port: 1, token: "b".repeat(64) })).not.toThrow();
		expect(() => validateBootstrap({ version: 1, generation: "a".repeat(32), port: 1, token: "b".repeat(64), host: "remote" })).toThrow();
	});
	it("reserves control progress behind an in-flight frame without interleaving bytes", () => {
		const sent: string[] = []; let finish: (error?: Error) => void = () => {};
		const socket = { write(bytes: Buffer, callback: typeof finish) { sent.push(bytes.toString()); finish = callback; } } as unknown as Socket;
		const writer = new FrameWriter(socket, error => { throw error; });
		writer.write(Buffer.from("event-one"), true);
		writer.write(Buffer.from("event-two"), true);
		writer.write(Buffer.from("completion"));
		writer.write(Buffer.from("cancel"));
		expect(sent).toEqual(["event-one"]);
		finish(); finish(); finish(); finish();
		expect(sent).toEqual(["event-one", "completion", "cancel", "event-two"]);
		writer.close(); expect(() => writer.write(Buffer.from("late"))).toThrow("closed");
	});
});
