#!/usr/bin/env node
// Stage the rpi side of the combined run into one directory.
//
// The Cursor cdylib resolves `runtime/main.mjs` relative to its own loaded
// path, so the library must be a real file (a symlink makes dladdr report the
// target's directory) and the packaged runtime must sit beside it. This copies
// the libs and links the runtime directory.
//
//   node tools/combined/stage-rpi.mjs --out <dir> \
//     --antigravity <librpi_antigravity_auth.so> \
//     --cursor <librpi_cursor_sdk.so> \
//     [--multiprovider <librpi_multiprovider.so>] \
//     --runtime <packages/rpi-cursor-sdk/runtime>
import { chmod, copyFile, mkdir, rm, symlink } from "node:fs/promises";
import { existsSync } from "node:fs";
import path from "node:path";

function parse(argv) {
  const options = { out: null, antigravity: null, cursor: null, multiprovider: null, runtime: null };
  for (let index = 0; index < argv.length; index++) {
    const arg = argv[index];
    const value = () => {
      index += 1;
      if (index >= argv.length) throw new Error(`${arg} needs a value`);
      return argv[index];
    };
    if (arg === "--out") options.out = value();
    else if (arg === "--antigravity") options.antigravity = value();
    else if (arg === "--cursor") options.cursor = value();
    else if (arg === "--multiprovider") options.multiprovider = value();
    else if (arg === "--runtime") options.runtime = value();
    else throw new Error(`unknown argument ${arg}`);
  }
  if (!options.out || !options.antigravity || !options.cursor || !options.runtime) {
    throw new Error("--out, --antigravity, --cursor and --runtime are required");
  }
  return options;
}

const options = parse(process.argv.slice(2));
for (const [name, value] of Object.entries(options)) {
  if (name !== "out" && value !== null && !existsSync(value)) throw new Error(`${name} does not exist: ${value}`);
}
await rm(options.out, { recursive: true, force: true });
await mkdir(options.out, { recursive: true, mode: 0o700 });
await chmod(options.out, 0o700);
const libs = [options.antigravity, options.cursor, ...(options.multiprovider ? [options.multiprovider] : [])];
for (const lib of libs) await copyFile(lib, path.join(options.out, path.basename(lib)));
await symlink(path.resolve(options.runtime), path.join(options.out, "runtime"), "dir");
process.stdout.write(`${options.out}\n`);
