// Shared helpers for the C06 cloud differential harness.
//
// Both runners (run-reference.mjs for unchanged Pi 1.3.0, run-rpi.mjs for the
// retained rpi worker) emit the same `observed` shape so the Rust test can
// compare complete JSON values:
//
//   exitCode        process/worker exit status
//   sdkCalls        per-API counts from the fake SDK process counters
//   cloudCalls      every cloud-relevant fake-SDK `sdk_call` record, projected
//                   to drop only the conversation prompt text and image bytes
//                   (prompt assembly is covered by the C01 prompt lane)
//   lifecycle       the ordered `cursor-cloud-lifecycle` session entries
//   journal         the ordered durable journal entries (whole objects; only
//                   per-run session identifiers are replaced by placeholders)
//   lifecycleCounts action -> count
//
// Nothing for real Cursor network access is generated here.
import { createHash } from "node:crypto";
import { existsSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";

export const SCHEMA = "rpi-cursor-c-cloud-evidence/1";
export const LIFECYCLE_TYPE = "cursor-cloud-lifecycle";
export const FROZEN_CLOCK = "2026-01-01T00:00:00.000Z";
export const JOURNAL_PATTERN = /^\.cursor-cloud-lifecycle-[0-9a-f]{32}\.journal$/;

export const sha256 = (text) => createHash("sha256").update(text).digest("hex");
export const sha256File = (path) => sha256(readFileSync(path));

export function sortObject(object) {
	return Object.fromEntries(Object.entries(object).sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0)));
}

export function readJsonLines(text) {
	return text.split("\n").filter((line) => line.length > 0);
}

/** Fake-SDK records written by the unchanged fake `@cursor/sdk`. */
export function fakeRecords(text) {
	return readJsonLines(text).map((line) => JSON.parse(line));
}

function isCloudCall(record) {
	if (record.kind !== "sdk_call") return false;
	if (record.api.startsWith("Agent.")) return true;
	if (record.api === "agent.send") return record.runtime === "cloud";
	return typeof record.agentId === "string" && record.agentId.startsWith("bc-");
}

/**
 * Cloud SDK calls with complete options. `agent.send` drops the assembled
 * conversation text and image payloads, which are runtime-specific prompt
 * assembly (C01), not cloud selection/lifecycle. Every other field is kept.
 */
export function cloudCalls(records) {
	return records.filter(isCloudCall).map((record) => {
		if (record.api !== "agent.send") return record;
		const { message: _message, images: _images, ...rest } = record;
		return rest;
	});
}

export function sdkCallCounts(records) {
	const counts = {};
	for (const record of records) {
		if (record.kind === "process_counters") {
			for (const [api, n] of Object.entries(record.counters.sdkCalls ?? {})) counts[api] = (counts[api] ?? 0) + n;
		}
	}
	return sortObject(counts);
}

/** The ordered cloud lifecycle payloads from appended session entries. */
export function lifecycleEntries(entries) {
	return entries
		.filter((entry) => entry && entry.type === "custom" && entry.customType === LIFECYCLE_TYPE)
		.map((entry) => entry.data);
}

export function lifecycleCounts(entries) {
	const counts = {};
	for (const entry of entries) counts[entry.action] = (counts[entry.action] ?? 0) + 1;
	return sortObject(counts);
}

/** Whole durable journal objects; only per-run session identity is placeholdered. */
export function normalizeJournal(raw) {
	const sessionIds = new Set();
	const sessionFiles = new Set();
	const anchors = new Set();
	const entries = readJsonLines(raw).map((line) => JSON.parse(line));
	for (const entry of entries) {
		if (typeof entry.sessionId === "string") sessionIds.add(entry.sessionId);
		if (typeof entry.sessionFile === "string") sessionFiles.add(entry.sessionFile);
		if (typeof entry.anchorEntryId === "string") anchors.add(entry.anchorEntryId);
	}
	return entries.map((entry) => ({
		...entry,
		...(typeof entry.sessionId === "string" ? { sessionId: "<session>" } : {}),
		...(typeof entry.sessionFile === "string" ? { sessionFile: "<file>" } : {}),
		...(typeof entry.anchorEntryId === "string" ? { anchorEntryId: "<anchor>" } : {}),
	}));
}

/** Reads every durable cloud lifecycle journal in a session directory. */
export function readJournals(directory) {
	if (!existsSync(directory)) return [];
	const files = readdirSync(directory).filter((name) => JOURNAL_PATTERN.test(name)).sort();
	return files.flatMap((name) => normalizeJournal(readFileSync(join(directory, name), "utf8")));
}

/**
 * Extracts cloud lifecycle entries from Pi's `entry_appended` JSON events. The
 * entries carry runtime-generated ids/timestamps; drop only those envelope
 * fields, keeping the complete `data` payload.
 */
export function lifecycleFromPiEvents(stdout) {
	const observed = [];
	for (const line of readJsonLines(stdout)) {
		const event = JSON.parse(line);
		if (event.type !== "entry_appended") continue;
		const entry = event.entry;
		if (!entry || entry.type !== "custom" || entry.customType !== LIFECYCLE_TYPE) continue;
		observed.push(entry.data);
	}
	return observed;
}

export function observedFromRecords(records, lifecycle, journal, sendError) {
	return {
		sdkCalls: sdkCallCounts(records),
		cloudCalls: cloudCalls(records),
		lifecycle,
		lifecycleCounts: lifecycleCounts(lifecycle),
		journal,
		sendError,
	};
}

/**
 * The cloud send refusal, if any, as Pi reports it: the last assistant message
 * with `stopReason: "error"`. A successful turn yields undefined.
 */
export function sendErrorFromPiEvents(stdout) {
	let error;
	for (const line of readJsonLines(stdout)) {
		const event = JSON.parse(line);
		if (event.type !== "turn_end") continue;
		if (event.message?.stopReason === "error" && typeof event.message.errorMessage === "string") error = event.message.errorMessage;
	}
	return error;
}

export function writeEvidence(path, evidence, write) {
	const text = `${JSON.stringify(evidence, null, 2)}\n`;
	if (write) {
		writeFileSync(path, text);
		process.stdout.write(`wrote ${path} (${evidence.status})\n`);
		return;
	}
	if (!existsSync(path)) throw new Error(`${path} missing; run with --write`);
	const committed = JSON.parse(readFileSync(path, "utf8"));
	const diffs = [];
	if (committed.status !== evidence.status) diffs.push("status");
	for (const result of evidence.scenarios) {
		const old = committed.scenarios?.find((candidate) => candidate.name === result.name);
		if (!old) diffs.push(`scenario ${result.name} missing from evidence`);
		else if (JSON.stringify(old) !== JSON.stringify(result)) diffs.push(`scenario ${result.name} differs from evidence`);
	}
	if (JSON.stringify(committed.files) !== JSON.stringify(evidence.files)) diffs.push("file provenance");
	if (diffs.length > 0) {
		process.stdout.write(`${path} is stale: ${diffs.join(", ")}\n`);
		process.exitCode = 1;
	}
}

export function argValue(args, name) {
	const index = args.indexOf(name);
	return index < 0 ? undefined : args[index + 1];
}
