// Node ESM resolve hook: the pinned upstream sources and the retained worker port
// use TypeScript-style `./sibling.js` specifiers that point at `./sibling.ts`
// files on disk. `--experimental-transform-types` strips types but does not rewrite
// those specifiers, so retry a failed `.js` resolution as `.ts` before giving up.
// This hook only changes module resolution; it never rewrites source text.
export async function resolve(specifier, context, next) {
	try {
		return await next(specifier, context);
	} catch (error) {
		if (specifier.endsWith(".js")) {
			try {
				return await next(`${specifier.slice(0, -3)}.ts`, context);
			} catch {
				// Fall through to the original resolution failure.
			}
		}
		throw error;
	}
}
