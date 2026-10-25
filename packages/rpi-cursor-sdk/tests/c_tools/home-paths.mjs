// Redaction of absolute checkout paths for the c_tools evidence file.
//
// Gate runs point HOME at a throwaway directory under /tmp, so splitting on
// os.homedir() alone leaves this checkout's real absolute path in committed
// evidence. Every home-like prefix is redacted instead, and the recorded
// provenance is asserted to be free of them before it is written.
import { homedir } from "node:os";
import { basename, dirname } from "node:path";

export const HOME_MARKER = "<home>";

/**
 * Home-like prefixes that must never be recorded verbatim.
 * @param {string[]} extra additional absolute paths (e.g. a lane's home)
 */
export function homePrefixes(extra = []) {
	const candidates = [homedir(), process.env.HOME, process.env.RPI_TEST_HOME, ...extra]
		.filter((value) => typeof value === "string")
		.map((value) => value.replace(/\/+$/, ""))
		.filter((value) => value.length > 1);
	return [...new Set(candidates)].sort((left, right) => right.length - left.length);
}

/**
 * The path with every home-like prefix replaced by `<home>`.
 * @param {string} path
 * @param {string[]} extra
 */
export function portablePath(path, extra = []) {
	let out = path;
	for (const prefix of homePrefixes(extra)) out = out.split(prefix).join(HOME_MARKER);
	return out;
}

/**
 * Throw when a recorded value still carries a raw home prefix.
 * @param {unknown} recorded
 * @param {string[]} extra
 */
export function assertPortable(recorded, extra = []) {
	const text = JSON.stringify(recorded);
	for (const prefix of homePrefixes(extra)) {
		if (text.includes(prefix)) {
			throw new Error(`recorded provenance still contains the absolute path ${prefix}; use <home> instead`);
		}
	}
}

/**
 * Extra prefixes for a checkout: lane worktrees sit directly under the projects
 * directory, whose parent is the home-like path a non-isolated run would use.
 * A checkout outside such a directory redacts itself instead.
 * @param {string} checkout
 */
export function checkoutPrefixes(checkout) {
	const projects = dirname(checkout);
	return basename(projects) === "Projects" ? [dirname(projects)] : [checkout];
}

/**
 * A recorded path that does not depend on which checkout produced it: paths
 * inside the checkout are recorded relative to it (`<checkout>/...`), so a lane
 * worktree and the main checkout regenerate byte-identical provenance; anything
 * else falls back to home redaction.
 * @param {string} path
 * @param {string} checkout
 * @param {string[]} extra
 */
export function checkoutRelativePath(path, checkout, extra = []) {
	const root = checkout.replace(/\/+$/, "");
	if (path === root) return CHECKOUT_MARKER;
	if (path.startsWith(`${root}/`)) return `${CHECKOUT_MARKER}${path.slice(root.length)}`;
	return portablePath(path, extra);
}

export const CHECKOUT_MARKER = "<checkout>";
