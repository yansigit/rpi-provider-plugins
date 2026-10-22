// I02 reference: imported history cannot activate Cursor management authority.
//
// Imports the unchanged pinned pi-cursor-sdk build and the unchanged Pi 1.3.0
// session manager. Identifier validity, resume/cleanup entry acceptance and
// cloud ledger projection come from those files; the import-origin policy is
// composed here and mirrored by `src/import_guard.rs`.
//
//   node tests/i02-import-guard-reference.mjs <provider-references> [--write]
//
// Without `--write` the committed fixture is regenerated and compared byte for
// byte (stdout JSON is stable, so two runs are identical). Exit 1 on a change.

import { createHash } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const [reference, flag] = process.argv.slice(2);
if (!reference) {
	console.error("usage: i02-import-guard-reference.mjs <provider-references> [--write]");
	process.exit(2);
}
const write = flag === "--write";
if (flag !== undefined && !write) {
	console.error(`unknown flag: ${flag}`);
	process.exit(2);
}

const cursorRoot = join(reference, "cursor");
const piRoot = join(reference, "integrated/node_modules/@earendil-works/pi-coding-agent");

const EXPECTED = {
	"cursor/dist/cursor-session-agent-resume.js": "5d8be20b675cc680704b3a7afbb8f002543d3d709bc0f3ac717b0d21bd0b8ef8",
	"cursor/dist/cursor-session-agent-cleanup.js": "5f70d4b33489ae2d32ea241cbab6434769b726b11bf3823b6ece41c0ce8bcff6",
	"cursor/dist/cursor-cloud-lifecycle.js": "62587e86a4c17a486447cf599712281832c53f082614ef28d485e855763c967a",
	"cursor/shared/cursor-cloud-lifecycle-constants.mjs": "b232945db6ac46efca48b1c197012316421d06d83287b4d2ffd5d3e8012382fa",
	"integrated/node_modules/@earendil-works/pi-coding-agent/dist/core/session-manager.js":
		"046b6a1109ac3f0ed893bb85bf0648709362fa926a5da75761216cf2fcf9d926",
};

const hashes = {};
for (const [relative, expected] of Object.entries(EXPECTED)) {
	const digest = createHash("sha256").update(readFileSync(join(reference, relative))).digest("hex");
	if (digest !== expected) {
		console.error(`${relative}: sha256 ${digest} != pinned ${expected}`);
		process.exit(1);
	}
	hashes[relative] = digest;
}

const resumeModule = await import(pathToFileURL(join(cursorRoot, "dist/cursor-session-agent-resume.js")));
const cleanupModule = await import(pathToFileURL(join(cursorRoot, "dist/cursor-session-agent-cleanup.js")));
const cloudModule = await import(pathToFileURL(join(cursorRoot, "dist/cursor-cloud-lifecycle.js")));
const constants = await import(pathToFileURL(join(cursorRoot, "shared/cursor-cloud-lifecycle-constants.mjs")));
const piSession = await import(pathToFileURL(join(piRoot, "dist/core/session-manager.js")));

const NOTICE =
	"Imported Pi history cannot resume a Cursor agent. This conversation continues with a fresh Cursor agent; earlier Cursor branch continuity, cleanup and cloud records are unavailable.";
const IMPORT_DIR = "pi-import";
const RESUME_ENTRY = "cursor-sdk-agent-resume";
const CLEANUP_ENTRY = "cursor-sdk-agent-cleanup";
const CLOUD_ENTRY = "cursor-cloud-lifecycle";
const LEGACY_RESUME_VERSION = 1;
const RESUME_VERSION = 2;
const RECORD_VERSION = 1;

// ---- upstream predicates (unchanged code, no reimplementation) -------------
function validSessionId(value) {
	try {
		piSession.assertValidSessionId(value);
		return true;
	} catch {
		return false;
	}
}
const validLocalAgentId = (value) => resumeModule.isCursorLocalAgentId(value);
const validCloudAgentId = (value) =>
	typeof value === "string" && constants.CLOUD_AGENT_ID_PATTERN.test(value);
const upstreamResume = (value) => resumeModule.parseCursorSessionAgentResumeEntryData(value);
const upstreamCleanup = (value) => cleanupModule.__testUtils.parseCleanupEntryData(value);

// ---- policy ---------------------------------------------------------------
const isObject = (value) => typeof value === "object" && value !== null && !Array.isArray(value);
const own = (value, key) => isObject(value) && Object.prototype.hasOwnProperty.call(value, key);

function safeSourcePath(path) {
	if (typeof path !== "string" || path.includes("\0")) return false;
	const normalized = path.replace(/\\/g, "/");
	const absolute = normalized.startsWith("/") || /^[A-Za-z]:\//.test(normalized);
	return (
		absolute &&
		normalized.endsWith(".jsonl") &&
		!normalized.split("/").includes("..")
	);
}

function markerCode(marker) {
	if (!isObject(marker)) return "imported-marker-invalid";
	if (marker.source !== "pi") return "imported-origin-forged";
	if (typeof marker.sourcePath !== "string") return "imported-marker-invalid";
	if (!safeSourcePath(marker.sourcePath)) return "unsafe-source-path";
	if (typeof marker.sha256 !== "string" || !/^[0-9a-fA-F]{64}$/.test(marker.sha256))
		return "imported-marker-invalid";
	const version = marker.sessionVersion;
	if (!Number.isSafeInteger(version) || version < 0) return "imported-marker-invalid";
	if (version < 1 || version > 3) return "unsupported-imported-version";
	return null;
}

function importedLocation(path) {
	return path.split("/").includes(IMPORT_DIR);
}

function entrySafe(entry) {
	// Origin markers are reported as `imported-entry-origin`, not as bad IDs.
	if (typeof entry.id !== "string" || !validSessionId(entry.id)) return false;
	if (own(entry, "parentId") && entry.parentId !== null) {
		if (typeof entry.parentId !== "string" || !validSessionId(entry.parentId)) return false;
	}
	return true;
}

function parseSession(lines) {
	if (lines.length === 0 || lines[0].trim() === "") return { code: "invalid-session", origin: "unknown" };
	let header;
	try {
		header = JSON.parse(lines[0]);
	} catch {
		return { code: "invalid-session", origin: "unknown" };
	}
	if (!isObject(header)) return { code: "invalid-session", origin: "unknown" };
	if (own(header, "imported")) {
		const code = markerCode(header.imported);
		return { code: code ?? "imported-origin", origin: "imported" };
	}
	if (header.type !== "session") return { code: "invalid-session", origin: "native" };
	if (!Number.isSafeInteger(header.version) || header.version < 0)
		return { code: "invalid-session", origin: "native" };
	if (header.version < 4) return { code: "obsolete-session-version", origin: "native" };
	if (header.version > 4) return { code: "unknown-session-version", origin: "native" };
	if (typeof header.id !== "string" || !validSessionId(header.id))
		return { code: "unsafe-identifier", origin: "native" };
	const entries = [];
	for (const line of lines.slice(1)) {
		if (line.trim() === "") continue;
		let entry;
		try {
			entry = JSON.parse(line);
		} catch {
			return { code: "invalid-session", origin: "unknown" };
		}
		if (!isObject(entry)) return { code: "invalid-session", origin: "native" };
		if (!entrySafe(entry)) return { code: "unsafe-identifier", origin: "native" };
		entries.push(entry);
	}
	return { header, entries, sessionId: header.id };
}

function custom(entries, kind) {
	return entries.filter((entry) => entry.type === "custom" && entry.customType === kind);
}

function refused(authority, code, origin, sessionId, pending = null) {
	const resume = authority === "resume";
	return {
		authority,
		allowed: false,
		code,
		origin,
		freshAgent: resume,
		notice: resume ? NOTICE : null,
		agentId: null,
		sessionId: sessionId ?? null,
		pending,
	};
}

function allowed(authority, sessionId, agentId = null) {
	return {
		authority,
		allowed: true,
		code: null,
		origin: "native",
		freshAgent: false,
		notice: null,
		agentId,
		sessionId,
		pending: null,
	};
}

function resumeGate(session, path) {
	let handle;
	for (const entry of custom(session.entries, RESUME_ENTRY)) {
		if (!own(entry, "data")) return "invalid-resume-entry";
		const data = entry.data;
		const parsed = upstreamResume(data);
		if (!parsed) return "invalid-resume-entry";
		if (parsed.version === LEGACY_RESUME_VERSION) return "obsolete-resume-entry";
		if (parsed.version !== RESUME_VERSION) return "unknown-resume-entry";
		if (own(data, "sessionId") && data.sessionId !== session.sessionId)
			return "mismatched-session-identity";
		if (own(data, "sessionFile")) {
			if (!safeSourcePath(data.sessionFile) || data.sessionFile !== path)
				return "mismatched-session-identity";
		}
		handle = parsed.agentId;
	}
	return handle ?? "missing-resume-entry";
}

function cloudPolicyAccept(value) {
	if (!isObject(value)) return false;
	if (!["record", "archive_intent", "delete_intent", "archive", "delete"].includes(value.action))
		return false;
	if (value.runtime !== "cloud") return false;
	if (!validCloudAgentId(value.agentId)) return false;
	if (typeof value.timestamp !== "string" || value.timestamp === "" || value.timestamp.length > 64)
		return false;
	if (own(value, "runId") && typeof value.runId !== "string") return false;
	// Branch display fields do not decide authority; upstream drops invalid ones.
	return true;
}

function compareTimestamps(left, right) {
	const z = (value) => /^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(\.\d+)?Z$/.test(value);
	const ordered = z(left) && z(right) ? (left < right ? -1 : left > right ? 1 : 0) : left.localeCompare(right);
	return ordered;
}

function cloudAgents(entries) {
	const agents = [];
	const mutations = [];
	for (const entry of entries) {
		if (entry.type !== "custom" || entry.customType !== CLOUD_ENTRY) continue;
		const data = entry.data;
		if (!cloudPolicyAccept(data)) continue;
		let index = agents.findIndex((agent) => agent.agentId === data.agentId);
		if (index === -1) {
			agents.push({ agentId: data.agentId, pending: null, deleted: false });
			mutations.push({});
			index = agents.length - 1;
		}
		if (data.action === "record") continue;
		const archive = data.action.startsWith("archive");
		const intent = data.action.endsWith("_intent");
		const slot = mutations[index];
		const key = archive ? "archive" : "delete";
		const current = slot[key];
		const comparison = current ? compareTimestamps(data.timestamp, current.timestamp) : 1;
		const replace =
			comparison > 0 || (comparison === 0 && !intent && current !== undefined && current.intent);
		if (replace) slot[key] = { intent, timestamp: data.timestamp };
	}
	for (let index = 0; index < agents.length; index += 1) {
		const slot = mutations[index];
		const pending = [];
		for (const key of ["archive", "delete"]) {
			if (slot[key]?.intent) pending.push([key, slot[key].timestamp]);
		}
		pending.sort((left, right) => compareTimestamps(right[1], left[1]));
		agents[index].pending = pending.length > 0 ? pending[0] : null;
		agents[index].deleted = slot.delete !== undefined && !slot.delete.intent;
	}
	return agents.filter((agent) => !agent.deleted);
}

function backupName(name) {
	const lower = name.toLowerCase();
	return (
		lower.endsWith(".bak") ||
		lower.endsWith(".backup") ||
		lower.endsWith(".tmp") ||
		lower.includes(".journal.bak")
	);
}

function artifactCheck(artifacts, sessionId, path, entries) {
	const synthetic = [...entries];
	for (const artifact of artifacts) {
		if (artifact.name.includes(IMPORT_DIR)) return { code: "imported-origin", pending: null };
		if (backupName(artifact.name)) return { code: "active-backup", pending: null };
		if (!artifact.name.includes(CLOUD_ENTRY)) continue;
		for (const line of artifact.text.split("\n")) {
			if (line.trim() === "") continue;
			let record;
			try {
				record = JSON.parse(line);
			} catch {
				return { code: "mismatched-custody", pending: null };
			}
			if (!isObject(record)) return { code: "mismatched-custody", pending: null };
			if (record.version !== RECORD_VERSION) return { code: "obsolete-custody-record", pending: null };
			if (record.sessionId !== sessionId || record.sessionFile !== path)
				return { code: "mismatched-custody", pending: null };
			if (
				own(record, "anchorEntryId") &&
				record.anchorEntryId !== null &&
				typeof record.anchorEntryId !== "string"
			)
				return { code: "mismatched-custody", pending: null };
			if (!cloudPolicyAccept(record)) return { code: "mismatched-custody", pending: null };
			synthetic.push({ type: "custom", customType: CLOUD_ENTRY, data: record });
		}
	}
	const pending = cloudAgents(synthetic).find((agent) => agent.pending !== null);
	return {
		code: null,
		pending: pending ? { agentId: pending.agentId, action: pending.pending[0] } : null,
	};
}

function evaluate(testCase) {
	const { authority, path, lines, artifacts } = testCase;
	if (importedLocation(path)) return refused(authority, "imported-location", "imported", null);
	const session = parseSession(lines);
	if (session.code) return refused(authority, session.code, session.origin, null);
	for (const entry of session.entries) {
		if (own(entry, "imported") || own(entry.data, "imported"))
			return refused(authority, "imported-entry-origin", "imported", session.sessionId);
	}
	if (authority === "resume") {
		const handle = resumeGate(session, path);
		return typeof handle === "string" && handle.startsWith("agent-")
			? allowed(authority, session.sessionId, handle)
			: refused(authority, handle, "native", session.sessionId);
	}
	if (authority === "cleanup") {
		const handle = resumeGate(session, path);
		if (!(typeof handle === "string" && handle.startsWith("agent-")))
			return refused(authority, handle, "native", session.sessionId);
		const pending = [];
		for (const entry of custom(session.entries, CLEANUP_ENTRY)) {
			const data = entry.data;
			const parsed = upstreamCleanup(data);
			if (!parsed) return refused(authority, "invalid-cleanup-entry", "native", session.sessionId);
			if (Array.isArray(data.candidateAgentIds)) {
				for (const candidate of data.candidateAgentIds) {
					if (!validLocalAgentId(candidate))
						return refused(authority, "unsafe-agent-id", "native", session.sessionId);
				}
			}
			for (const key of ["protectedAgentIds", "deletedAgentIds"]) {
				if (!own(data, key) || !Array.isArray(data[key])) continue;
				for (const value of data[key]) {
					if (!validLocalAgentId(value))
						return refused(authority, "unsafe-agent-id", "native", session.sessionId);
				}
			}
			if (own(data, "failedAgentIds") && Array.isArray(data.failedAgentIds)) {
				for (const failure of data.failedAgentIds) {
					if (!validLocalAgentId(failure?.agentId))
						return refused(authority, "unsafe-agent-id", "native", session.sessionId);
				}
			}
			if (parsed.action === "delete" && parsed.phase === "intent") {
				for (const candidate of parsed.candidateAgentIds ?? [])
					if (!pending.includes(candidate)) pending.push(candidate);
			} else if (parsed.action === "delete" && parsed.phase === "result") {
				for (const candidate of parsed.candidateAgentIds ?? []) {
					const at = pending.indexOf(candidate);
					if (at !== -1) pending.splice(at, 1);
				}
				if (Array.isArray(data.failedAgentIds)) {
					for (const failure of data.failedAgentIds) {
						const at = pending.indexOf(failure.agentId);
						if (at !== -1) pending.splice(at, 1);
					}
				}
			}
		}
		if (pending.length > 0) {
			pending.sort();
			return refused(authority, "active-custody", "native", session.sessionId, {
				agentId: pending[0],
				action: "delete",
			});
		}
		return allowed(authority, session.sessionId, handle);
	}
	for (const entry of custom(session.entries, CLOUD_ENTRY)) {
		const data = entry.data;
		if (!cloudPolicyAccept(data)) {
			if (isObject(data) && typeof data.agentId === "string" && !validCloudAgentId(data.agentId))
				return refused(authority, "unsafe-cloud-identifier", "native", session.sessionId);
			return refused(authority, "invalid-cloud-entry", "native", session.sessionId);
		}
	}
	const checked = artifactCheck(artifacts, session.sessionId, path, session.entries);
	if (checked.pending)
		return refused(authority, "active-custody", "native", session.sessionId, checked.pending);
	if (checked.code)
		return refused(
			authority,
			checked.code,
			checked.code === "imported-origin" ? "imported" : "native",
			session.sessionId,
		);
	return allowed(authority, session.sessionId);
}

// ---- scenarios ------------------------------------------------------------
const SESSION_ID = "0193aa1b-0000-7000-8000-000000000001";
const SESSION_PATH = `/home/u/.rpi/sessions/${SESSION_ID}.jsonl`;
const OTHER_ID = "0193aa1b-0000-7000-8000-000000000002";
const CLOUD_AGENT = "bc-00000000-0000-0000-0000-000000000001";
const STORE = { version: 1, stateRoot: "/home/u/.cursor/store" };
const SHA = "0".repeat(64);
const MARKER = (extra = {}) => ({
	source: "pi",
	sourcePath: "/home/u/.pi/agent/sessions/a.jsonl",
	sha256: SHA,
	sessionVersion: 3,
	...extra,
});

const header = (extra = {}) => JSON.stringify({ type: "session", version: 4, id: SESSION_ID, ...extra });
const piHeader = (extra = {}) => JSON.stringify({ type: "session", version: 3, id: SESSION_ID, ...extra });
const entryLine = (id, customType, data, extra = {}) =>
	JSON.stringify({ type: "custom", id, parentId: null, customType, data, ...extra });
const resumeData = (extra = {}) => ({
	version: 2,
	runtime: "local",
	agentId: "agent-abc",
	scopeKey: "scope",
	sessionFile: SESSION_PATH,
	sessionId: SESSION_ID,
	cwd: "/p",
	poolKey: "pool",
	branchPathHash: "hash",
	compactionGeneration: 0,
	sendState: { bootstrapped: false, contextFingerprint: "f", incrementalSendCount: 0 },
	createdAt: "2026-10-01T00:00:00.000Z",
	storeIdentity: STORE,
	...extra,
});
const resumeLine = (extra = {}) => entryLine("entry0001", RESUME_ENTRY, resumeData(extra));
const cleanupData = (extra = {}) => ({
	action: "delete",
	runtime: "local",
	timestamp: "2026-10-01T00:00:00.000Z",
	candidateAgentIds: ["agent-old"],
	...extra,
});
const cleanupLine = (extra = {}, id = "entry0002") =>
	entryLine(id, CLEANUP_ENTRY, cleanupData(extra));
const cloudData = (extra = {}) => ({
	action: "record",
	runtime: "cloud",
	agentId: CLOUD_AGENT,
	runId: "run-1",
	timestamp: "2026-10-01T00:00:00.000Z",
	...extra,
});
const cloudLine = (extra = {}, id = "entry0002") =>
	entryLine(id, CLOUD_ENTRY, cloudData(extra));
const journalLine = (extra = {}) =>
	JSON.stringify({
		version: 1,
		sessionId: SESSION_ID,
		sessionFile: SESSION_PATH,
		anchorEntryId: null,
		...cloudData(),
		...extra,
	});

const cases = [];
const add = (id, authority, lines, extra = {}) =>
	cases.push({ id, authority, path: extra.path ?? SESSION_PATH, lines, artifacts: extra.artifacts ?? [] });

add("resume-native", "resume", [header(), resumeLine()]);
add("resume-crlf", "resume", [`${header()}\r`, `${resumeLine()}\r`]);
add("resume-imported-header", "resume", [
	piHeader({ imported: MARKER() }),
	resumeLine(),
]);
add("resume-imported-location", "resume", [header(), resumeLine()], {
	path: `/home/u/.rpi/sessions/${IMPORT_DIR}/${SESSION_ID}.abc123456789.jsonl`,
});
add("resume-pi-version", "resume", [piHeader(), resumeLine()]);
add("resume-future-version", "resume", [
	JSON.stringify({ type: "session", version: 5, id: SESSION_ID }),
	resumeLine(),
]);
add("resume-header-id-traversal", "resume", [
	JSON.stringify({ type: "session", version: 4, id: "../escape" }),
	resumeLine(),
]);
add("resume-entry-id-traversal", "resume", [
	header(),
	resumeLine().replace('"id":"entry0001"', '"id":"../../entry0001"'),
]);
add("resume-entry-parent-traversal", "resume", [
	header(),
	resumeLine().replace('"parentId":null', '"parentId":"../parent"'),
]);
add("resume-entry-imported", "resume", [header(), resumeLine()].map((line, index) =>
	index === 1 ? line.replace('"data":{', '"imported":{"source":"pi"},"data":{') : line,
));
add("resume-entry-data-imported", "resume", [
	header(),
	entryLine("entry0001", RESUME_ENTRY, resumeData({ imported: MARKER() })),
]);
add("resume-marker-forged", "resume", [piHeader({ imported: MARKER({ source: "rpi" }) }), resumeLine()]);
add("resume-marker-traversal", "resume", [
	piHeader({ imported: MARKER({ sourcePath: "/home/u/.pi/../../etc/a.jsonl" }) }),
	resumeLine(),
]);
add("resume-marker-bad-digest", "resume", [piHeader({ imported: MARKER({ sha256: "abc" }) }), resumeLine()]);
add("resume-marker-future-version", "resume", [
	piHeader({ imported: MARKER({ sessionVersion: 4 }) }),
	resumeLine(),
]);
add("resume-legacy-entry", "resume", [
	header(),
	resumeLine({ version: LEGACY_RESUME_VERSION, storeIdentity: undefined }),
]);
add("resume-rejected-version", "resume", [header(), resumeLine({ version: 3 })]);
add("resume-agent-traversal", "resume", [header(), resumeLine({ agentId: "agent-../escape" })]);
add("resume-session-id-mismatch", "resume", [header(), resumeLine({ sessionId: OTHER_ID })]);
add("resume-session-file-mismatch", "resume", [
	header(),
	resumeLine({ sessionFile: `/home/u/.rpi/sessions/${OTHER_ID}.jsonl` }),
]);
add("resume-missing-entry", "resume", [header()]);
add("cleanup-native-empty-ledger", "cleanup", [header(), resumeLine()]);
add("cleanup-intent-pending", "cleanup", [
	header(),
	resumeLine(),
	cleanupLine({ phase: "intent" }),
]);
add("cleanup-intent-result", "cleanup", [
	header(),
	resumeLine(),
	cleanupLine({ phase: "intent" }),
	cleanupLine(
		{
			phase: "result",
			deletedAgentIds: ["agent-old"],
			timestamp: "2026-10-01T00:00:01.000Z",
		},
		"entry0003",
	),
]);
add("cleanup-candidate-traversal", "cleanup", [
	header(),
	resumeLine(),
	cleanupLine({ phase: "intent", candidateAgentIds: ["../../agent-old"] }),
]);
add("cleanup-imported-session", "cleanup", [piHeader({ imported: MARKER() }), resumeLine()]);
add("cloud-native-record", "cloud", [header(), cloudLine()]);
add("cloud-archive-intent-pending", "cloud", [
	header(),
	cloudLine(),
	cloudLine({ action: "archive_intent" }, "entry0003"),
]);
add("cloud-archive-intent-result", "cloud", [
	header(),
	cloudLine(),
	cloudLine({ action: "archive_intent" }, "entry0003"),
	cloudLine({ action: "archive", timestamp: "2026-10-01T00:00:01.000Z" }, "entry0004"),
]);
add("cloud-delete-result", "cloud", [
	header(),
	cloudLine(),
	cloudLine({ action: "delete_intent" }, "entry0003"),
	cloudLine({ action: "delete", timestamp: "2026-10-01T00:00:01.000Z" }, "entry0004"),
]);
add("cloud-invalid-agent-id", "cloud", [
	header(),
	cloudLine({ agentId: "bc-00000000-0000-0000-0000-00000000000z" }),
]);
add("cloud-missing-agent-id", "cloud", [
	header(),
	cloudLine({ agentId: undefined }),
]);
add("cloud-imported-session", "cloud", [piHeader({ imported: MARKER() }), cloudLine()]);
add("cloud-journal-pending", "cloud", [header(), cloudLine()], {
	artifacts: [
		{
			name: `.cursor-cloud-lifecycle-0123456789abcdef0123456789abcdef.journal`,
			text: `${journalLine({ action: "delete_intent" })}\n`,
		},
	],
});
add("cloud-journal-mismatched-session", "cloud", [header(), cloudLine()], {
	artifacts: [
		{
			name: `.cursor-cloud-lifecycle-0123456789abcdef0123456789abcdef.journal`,
			text: `${journalLine({ sessionId: OTHER_ID })}\n`,
		},
	],
});
add("cloud-journal-obsolete-record", "cloud", [header(), cloudLine()], {
	artifacts: [
		{
			name: `.cursor-cloud-lifecycle-0123456789abcdef0123456789abcdef.journal`,
			text: `${journalLine({ version: 0 })}\n`,
		},
	],
});
add("cloud-journal-backup", "cloud", [header(), cloudLine()], {
	artifacts: [
		{
			name: `.cursor-cloud-lifecycle-0123456789abcdef0123456789abcdef.journal.bak`,
			text: "",
		},
	],
});
add("cloud-import-artifact", "cloud", [header(), cloudLine()], {
	artifacts: [{ name: "session.pi-import.marker", text: "" }],
});

const fixtures = cases.map((testCase) => {
	// Lenient per-line parse: the upstream predicates are reported even when the
	// session as a whole is refused, so identifier rules stay independently
	// comparable.
	let header = null;
	try {
		const parsed = JSON.parse(testCase.lines[0].replace(/\r$/, ""));
		if (isObject(parsed)) header = parsed;
	} catch {}
	const entries = [];
	for (const line of testCase.lines.slice(1)) {
		if (line.trim() === "") continue;
		try {
			const entry = JSON.parse(line.replace(/\r$/, ""));
			if (isObject(entry)) entries.push(entry);
		} catch {}
	}
	const resume = [];
	const cleanup = [];
	for (const [index, entry] of entries.entries()) {
		if (entry.type !== "custom") continue;
		if (entry.customType === RESUME_ENTRY) {
			const parsed = upstreamResume(entry.data);
			resume.push({
				index,
				accepted: Boolean(parsed),
				version: parsed?.version ?? null,
				agentId: parsed?.agentId ?? null,
			});
		}
		if (entry.customType === CLEANUP_ENTRY) {
			const parsed = upstreamCleanup(entry.data);
			cleanup.push({
				index,
				accepted: Boolean(parsed),
				action: parsed?.action ?? null,
				phase: parsed?.phase ?? null,
			});
		}
	}
	const cloud = cloudModule.readCursorCloudLifecycleAgents(entries).map((agent) => ({
		agentId: agent.agentId,
		pendingAction: agent.pendingAction ?? null,
	}));
	return {
		id: testCase.id,
		authority: testCase.authority,
		path: testCase.path,
		lines: testCase.lines,
		artifacts: testCase.artifacts,
		upstream: {
			sessionId: header && typeof header.id === "string" ? header.id : null,
			sessionIdValid: header ? validSessionId(header.id) : false,
			piCurrentVersion: piSession.CURRENT_SESSION_VERSION,
			resume,
			cleanup,
			cloud,
		},
		result: evaluate(testCase),
	};
});

const authorities = { resume: 0, cleanup: 0, cloud: 0 };
for (const fixture of fixtures) authorities[fixture.authority] += 1;
const output = {
	status: "I02 package half: imported history carries no Cursor management authority",
	reference: "f337dc4bb7cea936c52e69a7fe08e7e376c8467b",
	pi: "1.3.0",
	node: "24.22.0",
	upstreamHashes: hashes,
	counts: {
		cases: fixtures.length,
		...authorities,
		refused: fixtures.filter((fixture) => !fixture.result.allowed).length,
		allowed: fixtures.filter((fixture) => fixture.result.allowed).length,
	},
	cases: fixtures,
};

const text = `${JSON.stringify(output, null, 2)}\n`;
const fixturePath = fileURLToPath(new URL("./i02-import-guard.json", import.meta.url));
if (write) {
	writeFileSync(fixturePath, text);
	console.log(`wrote ${fixturePath} (${fixtures.length} cases)`);
} else {
	const committed = readFileSync(fixturePath, "utf8");
	if (committed !== text) {
		console.error("i02-import-guard.json differs from the reference output; run with --write and review");
		process.exit(1);
	}
	console.log(`i02-import-guard.json matches the reference (${fixtures.length} cases)`);
}
