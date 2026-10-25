// The committed c_tools evidence must never record an absolute home path: gate
// runs set HOME to a temporary directory, so redaction cannot rely on homedir().
import assert from "node:assert/strict";
import { test } from "node:test";
import { homedir } from "node:os";
import { assertPortable, checkoutPrefixes, HOME_MARKER, homePrefixes, portablePath } from "./home-paths.mjs";

const checkout = "/home/somebody/Projects/rpi-package--lane";

test("an isolated home still redacts the checkout's real home", () => {
	const prefixes = checkoutPrefixes(checkout);
	assert.deepEqual(prefixes, ["/home/somebody"]);
	// homedir() is /tmp/... during a gate run, which matches nothing here.
	const fake = `${checkout}/packages/rpi-cursor-sdk/tests/fake-sdk`;
	assert.equal(portablePath(fake, prefixes), `${HOME_MARKER}/Projects/rpi-package--lane/packages/rpi-cursor-sdk/tests/fake-sdk`);
});

test("a checkout outside a projects directory redacts itself", () => {
	const prefixes = checkoutPrefixes("/opt/checkouts/rpi-package--lane");
	assert.deepEqual(prefixes, ["/opt/checkouts/rpi-package--lane"]);
	assert.equal(portablePath("/opt/checkouts/rpi-package--lane/packages/x", prefixes), `${HOME_MARKER}/packages/x`);
});

test("recorded provenance with a raw home path is refused", () => {
	const prefixes = checkoutPrefixes(checkout);
	assert.throws(
		() => assertPortable({ fakeSdkDir: `${checkout}/packages/x` }, prefixes),
		/packages\/x; use <home> instead|still contains the absolute path/,
	);
	assertPortable({ fakeSdkDir: `${HOME_MARKER}/Projects/rpi-package--lane/packages/x` }, prefixes);
});

test("the longest prefix wins and the current home is always a candidate", () => {
	const prefixes = homePrefixes(["/home/somebody", "/home/somebody/deeper"]);
	assert.deepEqual(
		prefixes.filter((prefix) => prefix.startsWith("/home/somebody")),
		["/home/somebody/deeper", "/home/somebody"],
	);
	assert.ok(prefixes.includes(homedir()));
	assert.equal(portablePath("/home/somebody/deeper/file", prefixes), `${HOME_MARKER}/file`);
	assert.equal(portablePath("/home/somebody/other", prefixes), `${HOME_MARKER}/other`);
});
