import type { SimpleStreamOptions } from "@earendil-works/pi-ai";

/** Translate only the native wire's enums and absent optionals; retain provider-specific options. */
export function nativeStreamOptions(input: Record<string, unknown>, signal: AbortSignal): SimpleStreamOptions {
	const options = { ...input };
	for (const key of ["apiKey", "timeoutMs", "maxRetries", "maxRetryDelayMs", "headers", "metadata", "cacheRetention",
		"sessionId", "reasoning", "maxTokens", "temperature", "thinkingBudgets"]) {
		if (options[key] === null) delete options[key];
	}
	if (options.reasoning !== undefined) {
		if (typeof options.reasoning !== "string") throw new Error("Invalid native Cursor reasoning level");
		const reasoning = options.reasoning.toLowerCase();
		if (!["off", "minimal", "low", "medium", "high", "xhigh", "max"].includes(reasoning)) throw new Error("Invalid native Cursor reasoning level");
		if (reasoning === "off") delete options.reasoning;
		else options.reasoning = reasoning;
	}
	if (options.cacheRetention !== undefined) {
		if (typeof options.cacheRetention !== "string") throw new Error("Invalid native Cursor cache retention");
		const retention = options.cacheRetention.toLowerCase();
		if (!["none", "short", "long"].includes(retention)) throw new Error("Invalid native Cursor cache retention");
		options.cacheRetention = retention;
	}
	const headers = options.headers ?? {};
	if (!headers || typeof headers !== "object" || Array.isArray(headers) ||
		Object.values(headers).some(value => value !== null && typeof value !== "string")) throw new Error("Invalid native Cursor request headers");
	return { ...options, headers: { ...headers }, signal } as SimpleStreamOptions;
}
