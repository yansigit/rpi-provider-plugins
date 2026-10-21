import type { AssistantMessage, AssistantMessageEvent, ToolCall } from "@earendil-works/pi-ai";
import { consumeCursorNativeToolDisplay, getCursorNativeToolDisplayState, isCursorReplayToolCallId } from "../cursor-native-tool-display-state.js";
import { NativeMessageStream, withNativeStreamFactory } from "./stream.js";

type Recording = { version: 1; isError: boolean; result: { content: unknown[]; details?: unknown; terminate?: boolean } };
function failure(message: string): Recording {
	return { version: 1, isError: true, result: { content: [{ type: "text", text: message }], details: {} } };
}

/** Each physical send owns its records; the inner stream and lazy outer share only final snapshots. */
export function withRecordedNativeStream<T>(owner: object, emit: (event: AssistantMessageEvent) => void, operation: () => T): T {
	const state = getCursorNativeToolDisplayState(owner);
	const final = new WeakMap<AssistantMessage, AssistantMessage>();
	function tool(call: ToolCall, terminal: boolean): ToolCall {
		if (!isCursorReplayToolCallId(call.id)) return call;
		// A partial may be persisted after a crash before its result was recorded. It must never execute.
		let recorded: Recording | { version: 1 } = { version: 1 };
		if (terminal) {
			try {
				const item = consumeCursorNativeToolDisplay(call.id, state, call.name);
				if (!item) recorded = failure(`No recorded Cursor ${call.name} result was available. This replay-only call does not execute work.`);
				else if (item.isError) recorded = failure(item.result.content
					.map(entry => entry.type === "text" ? entry.text : undefined).filter(Boolean).join("\n") || "Cursor tool replay failed");
				else recorded = { version: 1, isError: false, result: { ...item.result, terminate: item.terminate ?? true } };
			} catch (error) {
				recorded = failure(error instanceof Error ? error.message : String(error));
			}
		}
		return { ...call, recorded } as ToolCall;
	}
	function message(source: AssistantMessage, terminal: boolean): AssistantMessage {
		if (terminal && final.has(source)) return final.get(source)!;
		const copy = { ...source, content: source.content.map(block => block.type === "toolCall" ? tool(block, terminal) : block) };
		if (terminal) { final.set(source, copy); final.set(copy, copy); }
		return copy;
	}
	class RecordedStream extends NativeMessageStream {
		override push(event: AssistantMessageEvent): void {
			if (this.done) return;
			if (event.type === "done") super.push({ ...event, message: message(event.message, true) });
			else if (event.type === "error") super.push({ ...event, error: message(event.error, true) });
			else if (event.type === "toolcall_end") super.push({ ...event, toolCall: tool(event.toolCall, false), partial: message(event.partial, false) });
			else super.push({ ...event, partial: message(event.partial, false) });
		}
		override end(result?: AssistantMessage): void { super.end(result && message(result, true)); }
	}
	return withNativeStreamFactory(() => new RecordedStream(emit), operation);
}
