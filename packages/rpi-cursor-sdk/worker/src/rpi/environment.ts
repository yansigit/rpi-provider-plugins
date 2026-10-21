import { AsyncLocalStorage } from "node:async_hooks";

type RequestEnvironment = { mode: "ambient" | "replace"; env: Readonly<Record<string, string>> };
const requests = new AsyncLocalStorage<RequestEnvironment>();

/** Account replacement must not reintroduce the worker's inherited default credential. */
export function cursorCredentialEnvironment(name: string): string | undefined {
	const request = requests.getStore();
	if (request && Object.hasOwn(request.env, name)) return request.env[name];
	return request?.mode === "replace" ? undefined : process.env[name];
}
export function withNativeAuthEnvironment<T>(mode: "ambient" | "replace", env: Readonly<Record<string, string>>, operation: () => T): T {
	if ((mode !== "ambient" && mode !== "replace") || !env || typeof env !== "object" || Array.isArray(env) ||
		Object.values(env).some(value => typeof value !== "string")) throw new Error("Invalid native Cursor request environment");
	return requests.run({ mode, env: Object.freeze({ ...env }) }, operation);
}
