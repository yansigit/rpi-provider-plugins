import { createHash } from "node:crypto";

type JournalKind = "cloud" | "usage";
export type JournalRequest = (request: Record<string, unknown>) => Promise<unknown>;
function object(value: unknown): Record<string, unknown> {
	if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("Invalid native journal response");
	return value as Record<string, unknown>;
}
function size(value: unknown): number {
	if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0) throw new Error("Invalid native journal size");
	return value;
}
function frames(bytes: Buffer): { frames: unknown[]; incompleteTail: boolean } {
	const text = bytes.toString("utf8"), end = text.lastIndexOf("\n");
	return { frames: end < 0 ? [] : text.slice(0, end).split("\n").map(line => JSON.parse(line) as unknown),
		incompleteTail: !!text && !text.endsWith("\n") };
}

/** Awaited host IO; validation remains in the retained adapter. */
export class NativeJournals {
	private request: JournalRequest;
	constructor(request: JournalRequest) { this.request = request; }
	private async read(kind: JournalKind): Promise<{ bytes: Buffer; digest: string }> {
		const opened = object(await this.request({ action: "open", kind }));
		if (typeof opened.reader !== "string" || !opened.reader) throw new Error("Invalid native journal reader");
		const reader = opened.reader;
		try {
			const expectedSize = size(opened.size), parts: Buffer[] = [];
			let received = 0;
			const hash = createHash("sha256");
			for (;;) {
				const chunk = object(await this.request({ action: "next", reader }));
				if (typeof chunk.bytes !== "string") throw new Error("Invalid native journal chunk");
				const bytes = Buffer.from(chunk.bytes, "base64");
				if (bytes.toString("base64") !== chunk.bytes || bytes.length > 1024 * 1024 || received + bytes.length > expectedSize) {
					throw new Error("Invalid native journal chunk size or encoding");
				}
				received += bytes.length; parts.push(bytes); hash.update(bytes);
				if (chunk.digest !== null) {
					if (received !== expectedSize || chunk.digest !== hash.digest("hex")) throw new Error("Native journal snapshot verification failed");
					return { bytes: Buffer.concat(parts, received), digest: chunk.digest as string };
				}
				if (!bytes.length || received === expectedSize) throw new Error("Native journal snapshot did not finish");
			}
		} finally { await this.request({ action: "close", reader }); }
	}
	async readCloud(): Promise<string> { return (await this.read("cloud")).bytes.toString("utf8"); }
	async readUsage(): Promise<{ frames: unknown[]; incompleteTail: boolean }> { return frames((await this.read("usage")).bytes); }
	private async append(kind: JournalKind, bytes: Buffer, expectedDigest?: string): Promise<boolean> {
		const receipt = object(await this.request({ action: "append", kind, bytes: bytes.toString("base64"),
			...(expectedDigest === undefined ? {} : { expectedDigest }) }));
		if (receipt.committed === false && kind === "usage") return false;
		if (receipt.committed !== true || receipt.durability !== "durable" || size(receipt.size) < bytes.length) {
			throw new Error("Invalid native journal commit; inspect the journal before retrying a mutation");
		}
		return true;
	}
	async appendCloud(data: unknown): Promise<void> {
		await this.append("cloud", Buffer.from(`${JSON.stringify(data)}\n`));
	}
	async appendUsage(data: unknown, validate: (frames: unknown[]) => void): Promise<void> {
		const bytes = Buffer.from(`${JSON.stringify(data)}\n`), stable: unknown = JSON.parse(bytes.toString("utf8"));
		for (;;) {
			const snapshot = await this.read("usage"), prefix = frames(snapshot.bytes);
			if (prefix.incompleteTail) throw new Error("incomplete journal frame");
			validate([...prefix.frames, stable]);
			if (await this.append("usage", bytes, snapshot.digest)) return;
		}
	}
}
