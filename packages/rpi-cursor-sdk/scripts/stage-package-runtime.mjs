#!/usr/bin/env node
/**
 * Purpose: Stage the distributable Cursor worker runtime and its SHA-256 asset manifest.
 * Responsibilities: Bundle the compiled worker modules into a fixed, small set of files,
 * copy the tracked launcher/lock/license/shared files, and record every archived file.
 * Usage: `node scripts/stage-package-runtime.mjs [--out DIR] [--check FILE]`.
 * Invariants: No `node_modules` bytes are copied. Bare package imports stay external, so
 * the proprietary `@cursor/sdk` is installed from npm after installation, never vendored.
 * The output is deterministic for a given worker build and esbuild version.
 */
import { createHash } from "node:crypto";
import { createRequire } from "node:module";
import { cp, lstat, mkdir, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { dirname, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import process from "node:process";

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const workerRoot = join(packageRoot, "worker");
const runtimeRoot = join(packageRoot, "runtime");
const defaultOut = join(packageRoot, ".package");

// The launcher imports these modules by path. Each becomes one self-contained ESM bundle.
const ENTRIES = {
	"dist/rpi/peer": "dist/rpi/peer.js",
	"dist/rpi/paths": "dist/rpi/paths.js",
	"dist/rpi/runtime": "dist/rpi/runtime.js",
	"dist/rpi/transport-thread": "dist/rpi/transport-thread.js",
	"dist/cursor-sensitive-text": "dist/cursor-sensitive-text.js",
};
const COPIED_FILES = ["main.mjs", "package.json", "package-lock.json", "LICENSE"];
const COPIED_DIRECTORIES = ["shared"];

function fail(message) {
	process.stderr.write(`stage-package-runtime: ${message}\n`);
	process.exitCode = 1;
}

function argumentsFrom(argv) {
	const options = { out: defaultOut, check: undefined };
	for (let index = 0; index < argv.length; index += 1) {
		const value = argv[index];
		if (value === "--out" || value === "--check") {
			const next = argv[index + 1];
			if (next === undefined) throw new Error(`${value} needs a path`);
			if (value === "--out") options.out = resolve(next);
			else options.check = resolve(next);
			index += 1;
		} else if (value === "-h" || value === "--help") {
			process.stdout.write("Usage: node scripts/stage-package-runtime.mjs [--out DIR] [--check FILE]\n");
			process.exit(0);
		} else throw new Error(`unknown argument: ${value}`);
	}
	return options;
}

function sha256(bytes) {
	return createHash("sha256").update(bytes).digest("hex");
}

async function regularFile(path) {
	const info = await lstat(path);
	if (!info.isFile()) throw new Error(`expected a regular file: ${path}`);
	return readFile(path);
}

async function walk(root) {
	const files = [];
	for (const entry of await readdir(root, { withFileTypes: true })) {
		const path = join(root, entry.name);
		if (entry.isDirectory()) files.push(...(await walk(path)));
		else if (entry.isFile()) files.push(path);
		else throw new Error(`packaged runtime assets must be regular files: ${path}`);
	}
	return files;
}

async function copyTracked(outRuntime) {
	for (const name of COPIED_FILES) {
		const bytes = await regularFile(join(runtimeRoot, name));
		await writeFile(join(outRuntime, name), bytes);
	}
	for (const name of COPIED_DIRECTORIES) {
		const source = join(runtimeRoot, name);
		if (!(await lstat(source)).isDirectory()) throw new Error(`expected a directory: ${source}`);
		await cp(source, join(outRuntime, name), { recursive: true, dereference: false, errorOnExist: true });
	}
}

async function bundle(outRuntime) {
	const workerEntry = join(workerRoot, ENTRIES[Object.keys(ENTRIES)[0]]);
	try {
		await lstat(workerEntry);
	} catch {
		throw new Error(`missing compiled worker modules at ${dirname(workerEntry)}; run "npm ci && npm run build" in worker/ first`);
	}
	const require = createRequire(join(workerRoot, "package.json"));
	let esbuild;
	try {
		esbuild = require("esbuild");
	} catch {
		throw new Error('esbuild is unavailable; run "npm ci --ignore-scripts" in worker/ first');
	}
	const entryPoints = {};
	for (const [name, source] of Object.entries(ENTRIES)) entryPoints[name] = join(workerRoot, source);
	await esbuild.build({
		absWorkingDir: packageRoot,
		entryPoints,
		outdir: outRuntime,
		bundle: true,
		// Bare specifiers (including @cursor/sdk) remain runtime dependencies installed by npm.
		packages: "external",
		format: "esm",
		platform: "node",
		target: "node24",
		splitting: false,
		sourcemap: false,
		minify: false,
		legalComments: "none",
		logLevel: "warning",
	});
	return require("esbuild/package.json").version;
}

async function manifestFor(out) {
	const runtime = join(out, "runtime");
	const files = (await walk(runtime)).sort();
	const assets = [];
	for (const path of files) {
		const name = relative(out, path).split(sep).join("/");
		assets.push({ path: name, sha256: sha256(await regularFile(path)) });
	}
	return {
		schema_version: 1,
		package: "rpi-cursor-sdk",
		source: ".package",
		assets,
	};
}

try {
	const options = argumentsFrom(process.argv.slice(2));
	await rm(options.out, { recursive: true, force: true });
	const outRuntime = join(options.out, "runtime");
	await mkdir(outRuntime, { recursive: true });
	await copyTracked(outRuntime);
	const esbuildVersion = await bundle(outRuntime);
	const manifest = await manifestFor(options.out);
	const serialized = `${JSON.stringify(manifest, null, 2)}\n`;
	await writeFile(join(options.out, "runtime-assets.json"), serialized);
	process.stdout.write(`staged ${manifest.assets.length} assets under ${options.out} (esbuild ${esbuildVersion})\n`);
	if (options.check !== undefined) {
		const expected = await readFile(options.check, "utf8");
		if (expected !== serialized) {
			fail(`${relative(process.cwd(), options.check)} does not match the staged runtime; regenerate it with --write`);
		}
	}
} catch (error) {
	fail(error instanceof Error ? error.message : String(error));
}
