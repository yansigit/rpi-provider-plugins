import { cp, lstat, rm } from "node:fs/promises";
import { fileURLToPath } from "node:url";
const root = new URL("../", import.meta.url);
const runtime = new URL("runtime/", root);
if (!(await lstat(runtime)).isDirectory()) throw new Error("Native runtime directory must not be a link");
for (const name of ["dist", "shared"]) {
	const destination = new URL(name, runtime);
	await rm(destination, { recursive: true, force: true });
	await cp(new URL(`worker/${name}`, root), destination, { recursive: true });
}
process.stdout.write(`Staged native worker assets at ${fileURLToPath(runtime)}\n`);
