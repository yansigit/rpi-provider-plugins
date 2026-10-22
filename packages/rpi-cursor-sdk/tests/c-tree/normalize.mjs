// Shared normalization for the C04 session-entry fixtures.
//
// The reference (unchanged Pi 1.3.0 + pinned adapter) and the candidate (retained
// rpi worker with awaited session ports) are driven with the same scripted
// sessions, but their environment-derived values necessarily differ:
//
//   - Pi creates a random session id and a timestamp-named session file; rpi's
//     emulated host uses fixed fixture identifiers.
//   - Pi's persistent scope key is the session-file path; the native manager's is
//     `rpi:<sha256([path,id])>`.
//   - Opaque hashes (branchPathHash, contextFingerprint, the settings/store hash
//     embedded in poolKey and storeIdentity.stateRoot) cover random entry ids,
//     timestamps and the process system prompt. Pi also represents that system
//     prompt as the first `system` session message, while the native port passes
//     it as `context.systemPrompt`; the fingerprint is therefore normalized whole
//     and its semantic outcome is compared through sendState/compactionGeneration.
//
// Everything else (custom type, order, version, runtime, agent ids, cleanup
// action/phase, candidate/protected/deleted/failed agent ids, sendState shape and
// compactionGeneration) is compared exactly. This list is mirrored in both
// evidence files under `normalization`.
export const LINEAGE = "cursor-sdk-agent-lineage";
export const RESUME = "cursor-sdk-agent-resume";
export const CLEANUP = "cursor-sdk-agent-cleanup";
export const C04_TYPES = new Set([LINEAGE, RESUME, CLEANUP]);

const SESSION_FILE = /(?:[^\s"']*[/\\])?[^\s"'/\\]*\.jsonl/g;
const ISO = /\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z/g;
const UUID = /[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/g;
const AGENT = /agent-[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/g;

// Keep deterministic `agent-<uuid>` ids exact when they are the whole value.
function isAgentId(value) {
	return typeof value === "string" && /^agent-[A-Za-z0-9_-]+$/.test(value);
}

export function normalizeText(value, root) {
	let text = value;
	if (root) text = text.split(root).join("<root>");
	// Preserve deterministic fake agent ids before the generic UUID rule.
	const agents = [];
	text = text.replace(AGENT, (match) => {
		const token = `<agent:${agents.length}>`;
		agents.push(match);
		return token;
	});
	text = text
		.replace(SESSION_FILE, "<session-file>")
		.replace(ISO, "<iso>")
		.replace(UUID, "<uuid>")
		.replace(/\b[0-9a-f]{16}\b/g, "<hash>")
		.replace(/\b[0-9a-f]{32}\b/g, "<hash>");
	text = text.replace(/<agent:(\d+)>/g, (_, index) => agents[Number(index)]);
	return text;
}

function normalizePoolKey(value) {
	if (typeof value !== "string") return value;
	// poolKey joins scope, cwd, model, settings, store hash and bridge spelling
	// with NUL. Normalize each component that carries environment identity.
	return value
		.split("\u0000")
		.map((part, index) => {
			if (index === 0) return "<session-file>";
			if (index === 1) return "<cwd>";
			return normalizeText(part);
		})
		.join("\u0000");
}

// Pi carries its system prompt as the first `system` session message, so the
// fingerprint's message count includes it; the native port carries the same prompt
// in `context.systemPrompt`, outside `context.messages`. The opaque hash and that
// structural offset are therefore not comparable. The semantic outcome of the
// heuristic (`bootstrapped`, `incrementalSendCount`) is still compared exactly.
function normalizeFingerprint(value) {
	if (value === undefined) return value;
	return "<fingerprint>";
}

function normalizeError(value, root) {
	return typeof value === "string" ? normalizeText(value, root) : value;
}

/** Normalize one C04 custom entry for whole-value comparison. */
export function normalizeC04Entry(entry, root) {
	const data = entry.data && typeof entry.data === "object" && !Array.isArray(entry.data)
		? structuredClone(entry.data)
		: entry.data;
	const out = { customType: entry.customType, data };
	if (entry.customType === LINEAGE) {
		data.sessionId = "<session-id>";
		data.sessionFile = "<session-file>";
		data.scopeKey = "<scope-key>";
		data.cwd = "<cwd>";
		data.timestamp = "<iso>";
	} else if (entry.customType === RESUME) {
		data.sessionId = "<session-id>";
		data.sessionFile = "<session-file>";
		data.scopeKey = "<scope-key>";
		data.cwd = "<cwd>";
		data.poolKey = normalizePoolKey(data.poolKey);
		data.branchPathHash = "<hash>";
		data.createdAt = "<iso>";
		if (data.sendState && typeof data.sendState === "object") {
			data.sendState.contextFingerprint = normalizeFingerprint(data.sendState.contextFingerprint);
		}
		if (data.storeIdentity && typeof data.storeIdentity === "object") {
			data.storeIdentity.stateRoot = "<store-root>";
		}
	} else if (entry.customType === CLEANUP) {
		data.timestamp = "<iso>";
		if (Array.isArray(data.failedAgentIds)) {
			data.failedAgentIds = data.failedAgentIds.map((failure) => ({
				...failure,
				error: normalizeError(failure.error, root),
			}));
		}
	}
	return out;
}

export function normalizeC04Entries(entries, root) {
	return entries
		.filter((entry) => C04_TYPES.has(entry.customType))
		.map((entry) => normalizeC04Entry(entry, root));
}
