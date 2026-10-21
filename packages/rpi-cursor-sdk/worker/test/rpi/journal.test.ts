import { expect, it, vi } from "vitest";
import { NativeJournals } from "../../src/rpi/journal.js";
import { nativeJournalFixture } from "../helpers/native-journals.js";

it("reads verified chunks, preserves bytes, and revalidates after a competing append", async () => {
	const h = nativeJournalFixture(), port = new NativeJournals(h.request);
	h.contents.set("cloud", Buffer.from("é".repeat(1024 * 1024)));
	expect(await port.readCloud()).toBe("é".repeat(1024 * 1024));
	expect(h.readers.size).toBe(0);
	const original = h.request.getMockImplementation()!;
	let compete = true;
	h.request.mockImplementation(async input => {
		if (input.action === "append" && compete) {
			compete = false; h.contents.set("usage", Buffer.from('{"id":"competitor"}\n'));
		}
		return original(input);
	});
	const validate = vi.fn();
	await port.appendUsage({ id: "caller" }, validate);
	expect(validate.mock.calls).toEqual([[[{ id: "caller" }]], [[{ id: "competitor" }, { id: "caller" }]]]);
	expect((await port.readUsage()).frames).toEqual([{ id: "competitor" }, { id: "caller" }]);
});

it("awaits the acknowledgement and rejects truncated usage while cloud retains it", async () => {
	const h = nativeJournalFixture(), port = new NativeJournals(h.request);
	h.contents.set("usage", Buffer.from('{"prior":true}\n{"torn":'));
	expect(await port.readUsage()).toEqual({ frames: [{ prior: true }], incompleteTail: true });
	await expect(port.appendUsage({ next: true }, () => {})).rejects.toThrow("incomplete journal frame");
	expect(h.request.mock.calls.some(([input]) => input.action === "append")).toBe(false);
	h.contents.set("cloud", Buffer.from('{"torn":'));
	const original = h.request.getMockImplementation()!;
	let acknowledge!: () => void, finished = false;
	h.request.mockImplementation(async input => {
		if (input.action === "append") await new Promise<void>(resolve => { acknowledge = resolve; });
		return original(input);
	});
	const pending = port.appendCloud({ next: true }).then(() => { finished = true; });
	await Promise.resolve(); expect(finished).toBe(false);
	acknowledge(); await pending;
	expect(await port.readCloud()).toBe('{"torn":\n{"next":true}\n');
});

it("releases read handles after bad chunks and rejects non-durable acknowledgements", async () => {
	for (const chunk of [{ bytes: "invalid!", digest: null }, { bytes: "", digest: "incorrect" }, { bytes: "", digest: null }]) {
		const h = nativeJournalFixture(), port = new NativeJournals(h.request), original = h.request.getMockImplementation()!;
		h.request.mockImplementation(input => input.action === "next" ? Promise.resolve(chunk) : original(input));
		await expect(port.readUsage()).rejects.toThrow();
		expect(h.readers.size).toBe(0);
	}
	const h = nativeJournalFixture(), port = new NativeJournals(h.request);
	h.request.mockResolvedValueOnce({ committed: true, size: 3, durability: "ephemeral" });
	await expect(port.appendCloud({})).rejects.toThrow("Invalid native journal commit");
});
