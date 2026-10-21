import { afterEach, expect, it, vi } from "vitest";
import type { AssistantMessage, AssistantMessageEvent } from "@earendil-works/pi-ai";
import { __testUtils, getCursorNativeToolDisplayState, recordCursorNativeToolDisplay, registerCursorNativeToolDisplayState, type CursorNativeToolDisplayItem } from "../../src/cursor-native-tool-display-state.js";
import { createNativeCursorToolDefinition, wrapNativeCursorTool } from "../../src/cursor-native-tool-display-tools.js";
import { withRecordedNativeStream } from "../../src/rpi/replay.js";
import { createCursorMessageStream, NativeMessageStream } from "../../src/rpi/stream.js";
import { createExtensionTestContext, createPiHarness, makeAssistantMessage } from "../helpers/pi-harness.js";

afterEach(() => { __testUtils.reset(); vi.restoreAllMocks(); });
function fixture() {
	const pi = createPiHarness(); registerCursorNativeToolDisplayState(pi);
	__testUtils.registerNativeToolNameForTests("bash", pi);
	const state = getCursorNativeToolDisplayState(pi);
	const item: CursorNativeToolDisplayItem = { id: "cursor-replay-1-1-tool-0", toolName: "bash", args: { command: "never run" },
		result: { content: [{ type: "text", text: "already executed by Cursor" }], details: { exitCode: 0 } }, isError: false };
	const partial = makeAssistantMessage(); partial.content = [{ type: "toolCall", id: item.id, name: "bash", arguments: { command: "never run" } }];
	return { pi, state, item, partial };
}
function recording(message: AssistantMessage): unknown {
	return (message.content[0] as unknown as { recorded: unknown }).recorded;
}

it.each(["default", "continue", "error", "empty-error", "missing"])("matches the retained Pi replay outcome for %s without executing a fallback", async kind => {
	const f = fixture(), emitted: AssistantMessageEvent[] = [];
	if (kind === "continue") f.item.terminate = false;
	if (kind === "error" || kind === "empty-error") { f.item.isError = true; f.item.result.content = kind === "error" ? [{ type: "text", text: "first" }, { type: "text", text: "" }, { type: "text", text: "second" }] : []; }
	const stream = withRecordedNativeStream(f.pi, event => emitted.push(structuredClone(event)), () => createCursorMessageStream()) as NativeMessageStream;
	stream.push({ type: "toolcall_end", contentIndex: 0, toolCall: f.partial.content[0] as never, partial: f.partial });
	if (kind !== "missing") recordCursorNativeToolDisplay(f.item, f.state);
	stream.push({ type: "done", reason: "toolUse", message: f.partial }); stream.end(f.partial);
	const result = await stream.finished;
	expect(recording(result)).toEqual(recording((emitted[1] as { message: AssistantMessage }).message));
	expect(recording((emitted[0] as { partial: AssistantMessage }).partial)).toEqual({ version: 1 });
	expect((emitted[0] as unknown as { toolCall: { recorded: unknown } }).toolCall.recorded).toEqual({ version: 1 });
	expect(recording(f.partial)).toBeUndefined();

	const fallback = vi.fn(async () => { throw new Error("replay executed work"); });
	const original = wrapNativeCursorTool(createNativeCursorToolDefinition("bash", process.cwd()),
		() => ({ ...createNativeCursorToolDefinition("bash", process.cwd()), execute: fallback }), f.state);
	if (kind !== "missing") recordCursorNativeToolDisplay(f.item, f.state);
	let expected;
	try { expected = { version: 1, isError: false, result: await original.execute(f.item.id, f.item.args, undefined, undefined, createExtensionTestContext()) }; }
	catch (error) { expected = { version: 1, isError: true, result: { content: [{ type: "text", text: (error as Error).message }], details: {} } }; }
	expect(recording(result)).toEqual(expected); expect(fallback).not.toHaveBeenCalled();
});

it("does not consume a foreign owner's result or turn an executable bridge call into a recording", async () => {
	const f = fixture(), foreign = createPiHarness(); registerCursorNativeToolDisplayState(foreign);
	__testUtils.registerNativeToolNameForTests("bash", foreign);
	recordCursorNativeToolDisplay(f.item, getCursorNativeToolDisplayState(foreign));
	f.partial.content.push({ type: "toolCall", id: "cursor-pi-bridge-1", name: "custom", arguments: { exact: true } });
	const stream = withRecordedNativeStream(f.pi, () => {}, () => createCursorMessageStream()) as NativeMessageStream;
	stream.push({ type: "done", reason: "toolUse", message: f.partial }); stream.end();
	const result = await stream.finished;
	expect(recording(result)).toMatchObject({ isError: true, result: { content: [{ text: "No owned Cursor bash result was available. This replay-only call does not execute work." }] } });
	expect(result.content[1]).toEqual(f.partial.content[1]); expect(__testUtils.nativeToolResultCount()).toBe(1);
});

it("captures the final recording even after stream cancellation and preserves it through the lazy outer result", async () => {
	const f = fixture(), sink = vi.fn(() => { throw new DOMException("cancelled", "AbortError"); });
	const [inner, outer] = withRecordedNativeStream(f.pi, sink, () => [createCursorMessageStream(), createCursorMessageStream()]) as NativeMessageStream[];
	expect(() => inner.push({ type: "start", partial: f.partial })).toThrow("cancelled");
	recordCursorNativeToolDisplay(f.item, f.state);
	f.partial.stopReason = "aborted";
	inner.push({ type: "error", reason: "aborted", error: f.partial }); inner.end(f.partial);
	outer.end(await inner.finished);
	expect(recording(await outer.finished)).toMatchObject({ isError: false, result: { terminate: true } });
	expect(sink).toHaveBeenCalledTimes(1); expect(__testUtils.nativeToolResultCount()).toBe(0);
});
