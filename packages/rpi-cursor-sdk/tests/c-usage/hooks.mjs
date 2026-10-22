// Node resolve hook for the C-USAGE reference runs. It redirects the bare
// `@cursor/sdk` specifier to the local fake only when a scripted usage answer is
// present, so ordinary imports stay untouched.
import { pathToFileURL } from "node:url";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const FAKE = pathToFileURL(join(HERE, "sdk.mjs")).href;

export async function resolve(specifier, context, nextResolve) {
	if (specifier === "@cursor/sdk" && process.env.RPI_CURSOR_USAGE_SDK_RESPONSE !== undefined) {
		return { url: FAKE, shortCircuit: true };
	}
	return nextResolve(specifier, context);
}
