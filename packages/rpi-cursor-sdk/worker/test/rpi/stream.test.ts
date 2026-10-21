import { afterEach, expect, it, vi } from "vitest";
import type { AssistantMessageEvent } from "@earendil-works/pi-ai";
import { AssistantMessageEventStream } from "@earendil-works/pi-ai/utils/event-stream";
import { NativeMessageStream, createCursorMessageStream, withNativeStreamSink } from "../../src/rpi/stream.js";
import { CursorPartialContentEmitter } from "../../src/cursor-partial-content-emitter.js";
import { makeAssistantMessage } from "../helpers/pi-harness.js";

afterEach(() => vi.restoreAllMocks());

it("delivers ordered events once through the sink and waits for producer cleanup", async () => {
	const emitted: AssistantMessageEvent[] = [], stream = new NativeMessageStream(event => { emitted.push(structuredClone(event)); });
	const partial = makeAssistantMessage("partial"); partial.timestamp = Date.now();
	stream.push({ type: "start", partial });
	stream.push({ type: "text_start", contentIndex: 0, partial });
	stream.push({ type: "text_delta", contentIndex: 0, delta: "partial", partial });
	stream.push({ type: "done", reason: "stop", message: partial });
	expect(await stream.result()).toBe(partial);
	let finished = false;
	const pending = stream.finished.then(() => { finished = true; });
	await Promise.resolve(); expect(finished).toBe(false);
	stream.push({ type: "start", partial });
	expect(emitted.map(event => event.type)).toEqual(["start", "text_start", "text_delta", "done"]);
	expect(partial.durationMs).toBeGreaterThanOrEqual(0);
	stream.end(); await pending;
	expect(finished).toBe(true);
});

it("preserves Pi's timing rules without its unbounded FIFO", async () => {
	vi.spyOn(Date, "now").mockReturnValue(1000);
	vi.spyOn(performance, "now").mockReturnValue(500);
	const original = new AssistantMessageEventStream(), native = new NativeMessageStream(() => {});
	const a = makeAssistantMessage("complete"), b = makeAssistantMessage("complete");
	a.timestamp = b.timestamp = 1000;
	vi.spyOn(performance, "now").mockReturnValue(515.6);
	original.push({ type: "done", reason: "stop", message: a });
	native.push({ type: "done", reason: "stop", message: b }); native.end();
	expect(b.durationMs).toBe(a.durationMs); expect(b.durationMs).toBe(16);
	await expect(native[Symbol.asyncIterator]().next()).rejects.toThrow("owning IPC stream");
});

it("retains the final partial and allows cleanup after the credit gate is cancelled", async () => {
	const cancelled = new DOMException("cancelled", "AbortError"), sink = vi.fn(() => { throw cancelled; });
	const stream = new NativeMessageStream(sink), partial = makeAssistantMessage("preserved");
	expect(() => stream.push({ type: "text_delta", contentIndex: 0, delta: "preserved", partial })).toThrow(cancelled);
	partial.stopReason = "aborted";
	expect(() => stream.push({ type: "error", reason: "aborted", error: partial })).not.toThrow();
	stream.end();
	expect(await stream.finished).toBe(partial);
	expect(sink).toHaveBeenCalledTimes(1);
});

it("rejects a missing terminal and isolates native factories from the retained Pi path", async () => {
	expect(createCursorMessageStream()).not.toBeInstanceOf(NativeMessageStream);
	const outer = vi.fn(), inner = vi.fn();
	await withNativeStreamSink(outer, async () => {
		await Promise.resolve();
		const native = createCursorMessageStream(); expect(native).toBeInstanceOf(NativeMessageStream);
		if (!(native instanceof NativeMessageStream)) throw new Error("missing native stream");
		const checked = expect(native.finished).rejects.toThrow("without a terminal result");
		native.end(); await checked;
		withNativeStreamSink(inner, () => createCursorMessageStream().push({ type: "start", partial: makeAssistantMessage() }));
		createCursorMessageStream().push({ type: "start", partial: makeAssistantMessage() });
	});
	expect(outer).toHaveBeenCalledTimes(1); expect(inner).toHaveBeenCalledTimes(1);
	expect(createCursorMessageStream()).not.toBeInstanceOf(NativeMessageStream);
});

// Agreed native IPC exception: snapshot each event on emission, not after future object mutations.
it("captures partials at emission while retaining Pi event order, deltas and final output", async () => {
	vi.spyOn(Date, "now").mockReturnValue(1000);
	vi.spyOn(performance, "now").mockReturnValue(500);
	const reference = new AssistantMessageEventStream(), original: AssistantMessageEvent[] = [];
	const read = (async () => { for await (const event of reference) original.push(structuredClone(event)); })();
	const captured: AssistantMessageEvent[] = [], native = new NativeMessageStream(event => { captured.push(structuredClone(event)); });
	for (const stream of [reference, native]) {
		const partial = makeAssistantMessage(); partial.content = []; partial.timestamp = 1000;
		const emitter = new CursorPartialContentEmitter(stream, partial);
		emitter.appendTextDelta("hello"); emitter.closeAll();
		stream.push({ type: "done", reason: "stop", message: partial }); stream.end();
	}
	await read; await native.finished;
	expect(original[0]).toMatchObject({ type: "text_start", partial: { content: [{ type: "text", text: "hello" }] } });
	expect(captured[0]).toMatchObject({ type: "text_start", partial: { content: [{ type: "text", text: "" }] } });
	expect(captured.slice(1, -1).map(event => event.type)).toEqual(original.slice(1, -1).map(event => event.type));
	expect(captured[1]).toMatchObject({ type: "text_delta", delta: "hello", partial: { content: [{ type: "text", text: "hello" }] } });
	expect(captured.at(-1)).toEqual(original.at(-1));
});

it("does not reopen a failed delivery gate when a sink throws a falsy value", async () => {
	const sink = vi.fn(() => { throw undefined; });
	const stream = new NativeMessageStream(sink), partial = makeAssistantMessage();
	for (let i = 0; i < 2; i++) {
		let threw = false;
		try { stream.push({ type: "start", partial }); } catch { threw = true; }
		expect(threw).toBe(true);
	}
	stream.push({ type: "error", reason: "error", error: partial }); stream.end();
	await stream.finished;
	expect(sink).toHaveBeenCalledTimes(1);
});
