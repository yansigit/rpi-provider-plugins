import { createCursorMessageStream, NativeMessageStream } from "./rpi/stream.js";
import {
	type Api,
	type AssistantMessage,
	type AssistantMessageEventStream,
	type Context,
	type Model,
	type SimpleStreamOptions,
} from "@earendil-works/pi-ai";
import { streamCursor, type CursorProviderOwnership } from "./cursor-provider.js";
import { sanitizeCursorProviderError } from "./cursor-provider-errors.js";

function makeProviderRuntimeErrorMessage(model: Model<Api>, error: unknown, apiKey?: string): AssistantMessage {
	return {
		role: "assistant",
		content: [],
		api: model.api,
		provider: model.provider,
		model: model.id,
		usage: {
			input: 0,
			output: 0,
			cacheRead: 0,
			cacheWrite: 0,
			totalTokens: 0,
			cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
		},
		stopReason: "error",
		timestamp: Date.now(),
		errorMessage: `Cursor provider runtime failed: ${sanitizeCursorProviderError(error, apiKey)}`,
	};
}

export function createCursorLazyStream(capture: (model: Model<Api>, context: Context, options?: SimpleStreamOptions) => CursorProviderOwnership) {
	return (model: Model<Api>, context: Context, options?: SimpleStreamOptions): AssistantMessageEventStream => {
		const outer = createCursorMessageStream();
		const fail = (error: unknown) => {
			const message = makeProviderRuntimeErrorMessage(model, error, options?.apiKey);
			outer.push({ type: "error", reason: "error", error: message });
			outer.end(message);
		};
		let ownership: CursorProviderOwnership;
		try {
			ownership = capture(model, context, options);
		} catch (error) {
			fail(error);
			return outer;
		}
		queueMicrotask(async () => {
			try {
				const inner = streamCursor(model, context, options, ownership);
				if (outer instanceof NativeMessageStream && inner instanceof NativeMessageStream) {
					outer.end(await inner.finished);
				} else {
					for await (const event of inner) outer.push(event);
				}
			} catch (error) {
				fail(error);
			}
		});
		return outer;
	};
}
