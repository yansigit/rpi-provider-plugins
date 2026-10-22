// Minimal, self-contained extension harness for the C-USAGE reference runs.
//
// It drives the UNCHANGED pinned upstream usage modules
// (`provider-references/integrated/node_modules/pi-cursor-sdk/dist`) with the
// real Pi 1.3.0 `SessionManager`. Only the parts those modules actually touch are
// supplied; the session id, entry ids, clock and randomness are injected so a
// scenario replays byte-for-byte.
import { existsSync, mkdirSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

export const REFERENCE_ROOT = process.env.RPI_CURSOR_REFERENCE
	? process.env.RPI_CURSOR_REFERENCE
	: fileURLToPath(new URL("../../../../../provider-references/integrated", import.meta.url));
const PI_ENTRY = join(REFERENCE_ROOT, "node_modules/@earendil-works/pi-coding-agent/dist/index.js");
const CURSOR_DIST = join(REFERENCE_ROOT, "node_modules/pi-cursor-sdk/dist");

export async function loadUpstream() {
	const pi = await import(PI_ENTRY);
	const ledger = await import(join(CURSOR_DIST, "cursor-usage-ledger.js"));
	const command = await import(join(CURSOR_DIST, "cursor-usage-command.js"));
	return { SessionManager: pi.SessionManager, ledger, command };
}

export function makeAssistantMessage(text = "Done", timestamp = 2) {
	return {
		role: "assistant",
		content: [{ type: "text", text }],
		api: "cursor-sdk",
		provider: "cursor",
		model: "test-model",
		usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } },
		stopReason: "stop",
		timestamp,
	};
}

export function makeModel(id = "test-model") {
	return {
		id, name: "Test Model", api: "cursor-sdk", provider: "cursor", baseUrl: "", reasoning: false,
		input: ["text", "image"], cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
		contextWindow: 128000, maxTokens: 16384,
	};
}

/** Rewrite a session file's header id so its journal path is stable, then open it. */
export function openFixed(SessionManager, file, id) {
	const lines = readFileSync(file, "utf8").split("\n").filter(Boolean);
	const header = JSON.parse(lines[0]);
	header.id = id;
	writeFileSync(file, [JSON.stringify(header), ...lines.slice(1)].join("\n") + "\n");
	return SessionManager.open(file);
}

export function forkFixed(SessionManager, source, cwd, dir, id) {
	const forked = SessionManager.forkFrom(source, cwd, dir);
	const file = forked.getSessionFile();
	if (!file) throw new Error("fork produced no session file");
	return openFixed(SessionManager, file, id);
}

export function createHarness({ SessionManager, ledger, command, root, sessionId }) {
	mkdirSync(root, { recursive: true });
	const sessionDir = join(root, "sessions");
	mkdirSync(sessionDir, { recursive: true });
	const file = join(sessionDir, "fixed-session.jsonl");
	writeFileSync(file, `${JSON.stringify({ type: "session", version: 3, id: sessionId, timestamp: "2026-10-22T00:00:00.000Z", cwd: root })}\n`);
	let manager = SessionManager.open(file);
	manager.appendMessage({ role: "user", content: "not exported", timestamp: 1 });
	manager.appendMessage(makeAssistantMessage());

	const handlers = new Map();
	const commands = new Map();
	const appended = [];
	const pi = {
		on: (name, handler) => handlers.set(name, handler),
		appendEntry: (kind, data) => { manager.appendCustomEntry(kind, data); appended.push({ kind, data }); },
		registerCommand: (name, options) => commands.set(name, options),
	};
	ledger.registerCursorUsageLedger(pi);
	command.registerCursorUsageCommand(pi);

	const notify = [];
	const ctx = () => ({
		cwd: root,
		hasUI: true,
		ui: { notify: (message, type) => notify.push([message, type ?? null]) },
		sessionManager: manager,
		modelRegistry: { getApiKeyForProvider: () => "fixture-api-key" },
		model: makeModel("composer-2.5"),
	});
	const start = (agent, options = {}) =>
		ledger.captureCursorUsageRecorder(pi, ctx()).start({
			agent, runtime: "local", model: makeModel(), modelSelection: { id: "test-model" },
			purpose: "normal", resumed: false, newlyCreated: true, ...options,
		});
	const journalFiles = () => readdirSync(sessionDir).filter((name) => name.endsWith(".journal")).map((name) => join(sessionDir, name));
	return {
		root, sessionDir, pi, ctx, start, notify, commands, appended,
		get manager() { return manager; },
		replace(next) { manager = next; },
		override(next) { manager = next; },
		invokeEvent(name, event) { return handlers.get(name)?.(event, ctx()); },
		journalFiles,
		onlyJournal() {
			const files = journalFiles();
			if (files.length !== 1) throw new Error(`expected one journal, found ${files.length}`);
			return files[0];
		},
	};
}

export function exists(path) {
	return existsSync(path);
}

export function parentOf(path) {
	return dirname(path);
}
