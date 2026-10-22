#!/usr/bin/env node
/**
 * Purpose: Verify the distributable Cursor runtime asset manifest.
 * Responsibilities: Re-stage the packaged runtime from the compiled worker, canonicalize
 * its SHA-256 manifest, compare it with the committed fixture, and reject archives that
 * could not satisfy the host's sidecar rules or that contain proprietary SDK bytes.
 * Usage: `node tests/r01-package-reference.mjs [--write]`.
 * Invariants: Uses only the local pinned worker build; never installs or downloads packages.
 */
import { execFile } from "node:child_process";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import process from "node:process";

const run = promisify(execFile);
const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const fixture = join(root, "tests", "r01-package-assets.json");
const sdkHashes = join(root, "distribution", "cursor-sdk-1.0.37.sha256.json");
const MAX_ENTRIES = 128;
const LIBRARY_EXTENSIONS = new Set([".so", ".dylib", ".dll", ".pyd"]);

function fail(message) {
	process.stderr.write(`r01-package-reference: ${message}\n`);
	process.exit(1);
}

function canonical(manifest) {
	return `${JSON.stringify(manifest, null, 2)}\n`;
}

function validate(manifest, forbidden) {
	if (manifest.schema_version !== 1) fail("manifest schema_version must be 1");
	if (manifest.package !== "rpi-cursor-sdk") fail("manifest package must be rpi-cursor-sdk");
	if (manifest.source !== ".package") fail("manifest source must be .package");
	const assets = manifest.assets;
	if (!Array.isArray(assets) || assets.length === 0) fail("manifest must declare assets");
	if (assets.length + 1 > MAX_ENTRIES) fail(`manifest declares ${assets.length} assets; the host accepts at most ${MAX_ENTRIES - 1}`);
	const seen = new Set();
	const folded = new Set();
	for (const item of assets) {
		if (typeof item.path !== "string" || !/^[A-Za-z0-9_.-]+(?:\/[A-Za-z0-9_.-]+)*$/.test(item.path)) fail(`invalid asset path: ${item.path}`);
		if (item.path.length > 512) fail(`asset path is too long: ${item.path}`);
		if (folded.has(item.path.toLowerCase())) fail(`duplicate or case-aliased asset: ${item.path}`);
		folded.add(item.path.toLowerCase());
		seen.add(item.path);
		if (!item.path.startsWith("runtime/")) fail(`asset must live under runtime/: ${item.path}`);
		if (item.path.split("/").includes("node_modules")) fail(`asset vendors node modules: ${item.path}`);
		if (!/^[0-9a-f]{64}$/.test(item.sha256)) fail(`invalid sha256 for ${item.path}`);
		if (forbidden.has(item.sha256)) fail(`asset ${item.path} is a byte-identical copy of a proprietary Cursor SDK file`);
		const components = item.path.split("/");
		if (!components[0] || components.some(component => component.startsWith(".") || component === "")) fail(`invalid asset path: ${item.path}`);
		const extension = item.path.slice(item.path.lastIndexOf(".")).toLowerCase();
		if (LIBRARY_EXTENSIONS.has(extension)) fail(`asset has a native library extension: ${item.path}`);
	}
	const required = ["runtime/main.mjs", "runtime/package.json", "runtime/package-lock.json", "runtime/LICENSE"];
	for (const path of required) if (!seen.has(path)) fail(`manifest is missing ${path}`);
	if (![...seen].some(path => path.startsWith("runtime/dist/"))) fail("manifest is missing runtime/dist");
	if (![...seen].some(path => path.startsWith("runtime/shared/"))) fail("manifest is missing runtime/shared");
}

const write = process.argv.includes("--write");
const directory = await mkdtemp(join(tmpdir(), "rpi-cursor-package-"));
try {
	await run(process.execPath, [join(root, "scripts", "stage-package-runtime.mjs"), "--out", directory], { cwd: root });
	const manifest = JSON.parse(await readFile(join(directory, "runtime-assets.json"), "utf8"));
	const sdk = JSON.parse(await readFile(sdkHashes, "utf8"));
	if (sdk.packages.find(item => item.name === "@cursor/sdk")?.version !== "1.0.37") fail("SDK hash manifest is not @cursor/sdk@1.0.37");
	const forbidden = new Set(sdk.packages.flatMap(item => item.files));
	const serialized = canonical(manifest);
	validate(manifest, forbidden);
	if (write) {
		await writeFile(fixture, serialized);
		process.stdout.write(`wrote ${fixture}\n`);
	} else {
		let committed;
		try {
			committed = await readFile(fixture, "utf8");
		} catch {
			fail(`missing fixture ${fixture}; run with --write`);
		}
		if (committed !== serialized) fail(`fixture differs from the staged runtime; rerun with --write after reviewing the change`);
	}
	const summary = {
		status: "pass",
		package: manifest.package,
		assets: manifest.assets.length,
		entries: manifest.assets.length + 1,
		entryLimit: MAX_ENTRIES,
		forbiddenSdkFiles: forbidden.size,
		paths: manifest.assets.map(item => item.path),
	};
	process.stdout.write(`${JSON.stringify(summary, null, 2)}\n`);
} catch (error) {
	fail(error instanceof Error ? error.message : String(error));
} finally {
	await rm(directory, { recursive: true, force: true });
}
