// Fake `@cursor/sdk` for the C-USAGE reference runs.
//
// It exposes only the surface the retained usage modules touch: the static
// `Agent.getUsage(agentId, options)` that `/cursor-usage refresh` calls through
// `loadCursorSdk()`. The scripted answer comes from `RPI_CURSOR_USAGE_SDK_RESPONSE`
// (a JSON value, or `{"__error":"..."}` / `{"__hang":true}`). No Cursor network.
const DELAY_MS = Number(process.env.RPI_CURSOR_USAGE_SDK_DELAY_MS ?? 0);

function parseResponse() {
	const response = process.env.RPI_CURSOR_USAGE_SDK_RESPONSE;
	if (response === undefined) return undefined;
	return JSON.parse(response);
}

export const Agent = {
	async getUsage(agentId, _options) {
		const response = parseResponse();
		if (DELAY_MS > 0) await new Promise((resolve) => setTimeout(resolve, DELAY_MS));
		if (response && response.__error) throw new Error(String(response.__error));
		if (response && response.__hang) return new Promise(() => {});
		return response;
	},
};

export const Cursor = {
	async configure() {},
};

export class CursorSdkError extends Error {}
export class CursorAgentError extends CursorSdkError {}
export class AuthenticationError extends CursorSdkError {}
export class RateLimitError extends CursorSdkError {}
export class NetworkError extends CursorSdkError {}

export function wrapSdkError(error) {
	return error instanceof Error ? error : new Error(String(error));
}

export function convertError(error) {
	return wrapSdkError(error);
}
