import { WorkerPeer, readBootstrap } from "./dist/rpi/peer.js";
import { initializeNativePaths } from "./dist/rpi/paths.js";
import { scrubSensitiveText } from "./dist/cursor-sensitive-text.js";

let runtime;
let opening = false;
const peer = new WorkerPeer(await readBootstrap(), async (method, input, call) => {
	if (method !== "runtime.open") {
		if (!runtime) throw new Error("Native Cursor runtime has not opened");
		return runtime.dispatch(method, input, call);
	}
	if (opening) throw new Error("Native Cursor runtime already opened");
	opening = true;
	if (!input || typeof input !== "object" || Array.isArray(input)) throw new Error("Invalid native Cursor bootstrap");
	const paths = input;
	if (typeof paths.agentDir !== "string" || typeof paths.cwd !== "string" || typeof paths.sessionDir !== "string") throw new Error("Invalid native Cursor locations");
	initializeNativePaths(paths.agentDir);
	const { NativeCursorRuntime } = await import("./dist/rpi/runtime.js");
	runtime = new NativeCursorRuntime(call, { cwd: paths.cwd, sessionDir: paths.sessionDir });
	try {
		const manifest = await runtime.load();
		call.emit({ type: "ready", manifest });
		if (!call.signal.aborted) await new Promise(resolve => call.signal.addEventListener("abort", () => resolve(), { once: true }));
	} finally { await runtime.close(); }
	return null;
}, error => scrubSensitiveText(error instanceof Error ? error.message : String(error)));
await peer.ready;
await peer.disconnected;
try { await runtime?.close(); }
catch (error) { process.stderr.write(`${scrubSensitiveText(error instanceof Error ? error.message : String(error))}\n`); process.exitCode = 1; }
finally { await peer.close(); }
