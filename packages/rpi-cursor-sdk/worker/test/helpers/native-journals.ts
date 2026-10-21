import { createHash } from "node:crypto";
import { vi } from "vitest";
import type { JournalRequest } from "../../src/rpi/journal.js";

export function nativeJournalFixture() {
	const contents = new Map<string, Buffer>(), readers = new Map<string, { bytes: Buffer; offset: number }>();
	let next = 0;
	const request = vi.fn<JournalRequest>(async input => {
		switch (input.action) {
			case "open": {
				const bytes = contents.get(String(input.kind)) ?? Buffer.alloc(0), reader = `reader-${++next}`;
				readers.set(reader, { bytes, offset: 0 });
				return { reader, size: bytes.length };
			}
			case "next": {
				const reader = readers.get(String(input.reader));
				if (!reader) throw new Error("unknown reader");
				const bytes = reader.bytes.subarray(reader.offset, reader.offset + 1024 * 1024);
				reader.offset += bytes.length;
				return { bytes: bytes.toString("base64"), digest: reader.offset === reader.bytes.length
					? createHash("sha256").update(reader.bytes).digest("hex") : null };
			}
			case "close": readers.delete(String(input.reader)); return null;
			case "append": {
				const kind = String(input.kind), current = contents.get(kind) ?? Buffer.alloc(0);
				if (kind === "usage" && createHash("sha256").update(current).digest("hex") !== input.expectedDigest) return { committed: false };
				const frame = Buffer.from(String(input.bytes), "base64");
				const boundary = kind === "cloud" && current.length > 0 && current.at(-1) !== 10 ? Buffer.from("\n") : Buffer.alloc(0);
				const bytes = Buffer.concat([current, boundary, frame]); contents.set(kind, bytes);
				return { committed: true, size: bytes.length, durability: "durable" };
			}
			default: throw new Error("unexpected journal request");
		}
	});
	return { request, contents, readers };
}
