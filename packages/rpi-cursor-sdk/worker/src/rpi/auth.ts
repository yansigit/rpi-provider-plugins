import type { ApiKeyCredential, AuthPrompt, AuthResult, Provider } from "@earendil-works/pi-ai";

type ObjectValue = Record<string, unknown>;
function object(value: unknown): ObjectValue {
	if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("Invalid Cursor authentication request");
	return value as ObjectValue;
}
function environment(value: unknown): Record<string, string> {
	if (value === undefined) return {};
	const result = object(value);
	if (Object.values(result).some(value => typeof value !== "string")) throw new Error("Invalid Cursor authentication environment");
	return result as Record<string, string>;
}
function credential(value: unknown): ApiKeyCredential | undefined {
	if (value == null) return undefined;
	const input = object(value);
	if (input.type !== "api_key" || (input.key !== undefined && typeof input.key !== "string") ||
		Object.keys(input).some(key => !["type", "key", "env"].includes(key))) throw new Error("Invalid Cursor API-key credential");
	return { type: "api_key", ...(input.key === undefined ? {} : { key: input.key as string }),
		...(input.env === undefined ? {} : { env: environment(input.env) }) };
}
class LoginPrompt extends Error {
	readonly prompt: AuthPrompt;
	constructor(prompt: AuthPrompt) { super("Cursor API-key input required"); this.prompt = prompt; }
}

/** The pinned API-key login is one side-effect-free prompt; the host owns the continuation ticket. */
export async function cursorNativeAuth(provider: Provider, action: string, raw: unknown, signal: AbortSignal): Promise<unknown> {
	signal.throwIfAborted();
	const input = object(raw), apiKey = provider.auth.apiKey;
	if (!apiKey) throw new Error("Cursor API-key authentication is unavailable");
	const response = (kind: string, payload: unknown = null) => ({ version: 1, kind, payload });
	if (action === "resolveApiKey") {
		const env = environment(input.env);
		const resolved = await apiKey.resolve({ credential: credential(input.credential), signal,
			ctx: { env: async name => Object.hasOwn(env, name) ? env[name] : undefined,
				fileExists: async () => { throw new Error("Cursor authentication unexpectedly requested file access"); } },
		});
		return resolved ? response("auth", { ...resolved.auth, env: resolved.env ?? {}, source: resolved.source }) : response("unavailable");
	}
	if (action === "revoke") return response("success");
	if (action !== "login" && action !== "continue") throw new Error("Unsupported Cursor authentication action");
	if (!apiKey.login) throw new Error("Cursor API-key login is unavailable");
	if (action === "login" && input.methodId !== "api_key") throw new Error("Unsupported Cursor authentication method");
	if (action === "continue" && (object(input.continuation).kind !== "cursor-api-key" || typeof input.value !== "string")) {
		throw new Error("Invalid Cursor login continuation");
	}
	let prompted = false;
	try {
		const value = await apiKey.login({ signal,
			prompt: async prompt => {
				if (prompted || prompt.type !== "secret") throw new Error("Cursor API-key login contract changed");
				prompted = true;
				if (action === "continue") return input.value as string;
				throw new LoginPrompt(prompt);
			},
			notify: () => { throw new Error("Cursor API-key login contract changed"); },
		});
		return response("credential", value);
	} catch (error) {
		if (!(error instanceof LoginPrompt)) throw error;
		return response("interaction", { kind: "input", title: error.prompt.message, secret: true, allowEmpty: true,
			continuation: { kind: "cursor-api-key" } });
	}
}

/** Validate the private host RequestAuth response before exposing the Pi auth shape. */
export function nativeRequestAuth(response: unknown): AuthResult | undefined {
	if (response === null) return undefined;
	if (!response || typeof response !== "object" || Array.isArray(response)) throw new Error("Invalid native authentication response");
	const value = response as Record<string, unknown>;
	for (const key of ["apiKey", "baseUrl", "source"]) if (value[key] != null && typeof value[key] !== "string") throw new Error("Invalid native authentication text");
	const strings = (input: unknown): Record<string, string> | undefined => {
		if (input == null) return undefined;
		if (typeof input !== "object" || Array.isArray(input) || Object.values(input).some(value => typeof value !== "string")) throw new Error("Invalid native authentication map");
		return input as Record<string, string>;
	};
	return { auth: { ...(typeof value.apiKey === "string" ? { apiKey: value.apiKey } : {}),
		...(typeof value.baseUrl === "string" ? { baseUrl: value.baseUrl } : {}), headers: strings(value.headers) },
		env: strings(value.env), ...(typeof value.source === "string" ? { source: value.source } : {}) };
}
