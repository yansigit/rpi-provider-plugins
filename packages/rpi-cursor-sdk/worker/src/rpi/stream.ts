import { AsyncLocalStorage } from "node:async_hooks";
import { createAssistantMessageEventStream, type AssistantMessage, type AssistantMessageEvent, type AssistantMessageEventStream } from "@earendil-works/pi-ai";
// Pi’s packed extension loader exposes the public factory, not the implementation subpath.
const StreamBase = createAssistantMessageEventStream().constructor as new () => AssistantMessageEventStream;

type Sink = (event: AssistantMessageEvent) => void;
const sinks = new AsyncLocalStorage<Sink>();

/** Emits directly through the worker's synchronous credit gate; no second event queue. */
export class NativeMessageStream extends StreamBase {
	private sink: Sink;
	private terminal?: AssistantMessage;
	private deliveryError: unknown;
	private deliveryFailed = false;
	private ended = false;
	private resolveFinished!: (message: AssistantMessage) => void;
	private rejectFinished!: (error: Error) => void;
	/** Resolves after producer cleanup/end, not merely when a terminal event is pushed. */
	readonly finished = new Promise<AssistantMessage>((resolve, reject) => {
		this.resolveFinished = resolve; this.rejectFinished = reject;
	});
	constructor(sink: Sink) { super(); this.sink = sink; }
	override push(event: AssistantMessageEvent): void {
		if (this.done) return;
		const terminal = event.type === "done" ? event.message : event.type === "error" ? event.error : undefined;
		if (terminal) {
			this.terminal = terminal;
			// Preserve Pi's duration and result semantics without enqueuing the event.
			super.end(terminal);
		}
		if (this.deliveryFailed) {
			if (!terminal) throw this.deliveryError;
			return;
		}
		try { this.sink(event); }
		catch (error) {
			this.deliveryError = error; this.deliveryFailed = true;
			// Cleanup must still reach end after cancellation closes the stream credit gate.
			if (!terminal) throw error;
		}
	}
	override end(result?: AssistantMessage): void {
		if (this.ended) return;
		this.ended = true;
		if (result !== undefined) this.terminal = result;
		super.end(result);
		if (this.terminal) this.resolveFinished(this.terminal);
		else this.rejectFinished(new Error("Cursor stream ended without a terminal result"));
	}
	override async *[Symbol.asyncIterator](): AsyncGenerator<AssistantMessageEvent> {
		throw new Error("Native Cursor events are delivered through the owning IPC stream");
	}
}

export function createCursorMessageStream(): AssistantMessageEventStream {
	const sink = sinks.getStore();
	return sink ? new NativeMessageStream(sink) : createAssistantMessageEventStream();
}
export function withNativeStreamSink<T>(sink: Sink, operation: () => T): T { return sinks.run(sink, operation); }
