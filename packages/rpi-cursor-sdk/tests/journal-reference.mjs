// Execute the unchanged pinned Node file writers; all paths are disposable fixture paths.
import { readFileSync, writeFileSync, existsSync, mkdirSync } from "node:fs";
import { pathToFileURL } from "node:url";
import { join } from "node:path";
const [reference, root] = process.argv.slice(2);
const requests = JSON.parse(readFileSync(0, "utf8"));
const durable = await import(pathToFileURL(join(reference, "dist/cursor-durable-fs.js")));
const cloud = await import(pathToFileURL(join(reference, "dist/cursor-cloud-lifecycle.js")));
const OriginalDate = Date;
globalThis.Date = class extends OriginalDate {
	constructor(...args) { super(...(args.length ? args : [1_729_000_000_123])); }
	static now() { return 1_729_000_000_123; }
};
const results = [];
for (const [index, input] of requests.entries()) {
	const directory = join(root, String(index)); mkdirSync(directory);
	let path = join(directory, "usage.journal"), outcomes = [];
	if (input.kind === "cloud") {
		cloud.__testUtils.reset();
		const sessionFile = join(directory, "session.jsonl"), sessionId = "fixture";
		writeFileSync(sessionFile, "fixture\n");
		path = cloud.__testUtils.durableLedgerPath(sessionFile, sessionId);
		const entries = [], handlers = new Map();
		const pi = { on: (name, handler) => handlers.set(name, handler), appendEntry: (customType, data) => {
			entries.push({ id: `entry-${entries.length + 1}`, type: "custom", customType, data });
		} };
		cloud.registerCursorCloudLifecycleLedger(pi);
		handlers.get("session_start")({}, { sessionManager: { getSessionFile: () => sessionFile, getSessionId: () => sessionId, getBranch: () => entries } });
		if (input.prefix !== null) writeFileSync(path, Buffer.from(input.prefix, "base64"));
		for (const report of input.records) outcomes.push(cloud.recordCursorCloudLifecycleRun(report));
	} else {
		if (input.prefix !== null) writeFileSync(path, Buffer.from(input.prefix, "base64"));
		for (const data of input.records) {
			try { durable.appendDurableFrame(path, data); outcomes.push(true); }
			catch (error) { outcomes.push(error.message); }
		}
	}
	results.push({ outcomes, bytes: (existsSync(path) ? readFileSync(path) : Buffer.alloc(0)).toString("base64") });
}
process.stdout.write(JSON.stringify(results));
