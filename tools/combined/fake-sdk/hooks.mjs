// Resolve hook for register.mjs. Redirects the two @cursor/sdk specifiers the
// pinned adapter and the rpi worker import to the fake, and leaves every other
// specifier untouched. It is inert unless RPI_CURSOR_FAKE_SCENARIO is set.
const SDK_URL = new URL("./sdk.mjs", import.meta.url).href;
const SQLITE_URL = new URL("./sqlite.mjs", import.meta.url).href;

const REDIRECTS = new Map([
	["@cursor/sdk", SDK_URL],
	["@cursor/sdk/sqlite", SQLITE_URL],
]);

export async function resolve(specifier, context, nextResolve) {
	if (process.env.RPI_CURSOR_FAKE_SCENARIO && REDIRECTS.has(specifier)) {
		return { url: REDIRECTS.get(specifier), format: "module", shortCircuit: true };
	}
	return nextResolve(specifier, context);
}
