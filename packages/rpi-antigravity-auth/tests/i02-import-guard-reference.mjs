// I02 reference: imported vault state cannot activate Antigravity custody.
//
// Custody classifications come from the unchanged pinned Pi slot implementation
// (`packages/core/src/common-auth-embedded/pi-slot/slot.js`). The import-origin
// policy is composed here and mirrored by `src/vault/import_guard.rs`.
//
//   node tests/i02-import-guard-reference.mjs <provider-references> [--write]
//
// Scenarios run in a disposable directory under $HOME and write only Pi slot
// files there. Two runs produce byte-identical output.

import { createHash } from "node:crypto";
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
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

const antigravityRoot = join(reference, "antigravity");
const EXPECTED = {
	"antigravity/packages/core/src/common-auth-embedded/pi-slot/slot.js":
		"13a7d5941b79c6d444cf9dc6f23263cf9237146c7dd5de7d162dbc21e015532c",
	"antigravity/packages/pi/src/index.ts": "bd04a01ef5c99edd2ac64de41e136bc797991ea38bb3312b0c5a5791a2340e12",
	"antigravity/packages/pi/src/vault-setup.ts":
		"64bf98fdb397707be4d1bdd34ef56b89dbd1ebd67a07afd8de7c01e6e418c07e",
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
if (!readFileSync(join(antigravityRoot, "packages/pi/src/index.ts"), "utf8").includes("'antigravity-vault-custody'"))
	throw new Error("pinned placeholder marker moved");

const { openPiSlot } = await import(
	pathToFileURL(join(antigravityRoot, "packages/core/src/common-auth-embedded/pi-slot/slot.js"))
);

const NOTICE =
	"Vault state that was not enrolled for this agent cannot serve Antigravity. Request vault access again; no imported custody, backup, token or local login is activated.";
const RESTORE =
	"The credential slot and vault mode disagree. Restore the saved login or switch the account source before continuing.";
const MARKER = "antigravity-vault-custody";
const PROVIDER = "google";
const STASH = "google-login-stash.json";
const MODE_VERSION = 1;
const STASH_VERSION = 1;
const SHA = "0".repeat(64);
const LOGIN = JSON.stringify({ google: { type: "api_key", key: "real-login-key" } });
const PLACEHOLDER = JSON.stringify({ google: { type: "api_key", key: MARKER } });

const root = mkdtempSync(join(homedir(), ".rpi-i02-"));
try {
	// ---- upstream custody classifications --------------------------------
	const layouts = ["empty", "login", "placeholder", "lookalike", "entered", "foreign-stash"];
	const custodyCases = [];
	for (const layout of layouts) {
		const directory = join(root, layout);
		mkdirSync(directory, { recursive: true, mode: 0o700 });
		const authPath = join(directory, "auth.json");
		const stashPath = join(directory, STASH);
		if (layout === "login" || layout === "entered") writeFileSync(authPath, LOGIN);
		if (layout === "placeholder") writeFileSync(authPath, PLACEHOLDER);
		if (layout === "lookalike")
			writeFileSync(
				authPath,
				JSON.stringify({ google: { type: "api_key", key: `${MARKER}-extra` } }),
			);
		if (layout === "foreign-stash") {
			const source = join(root, "foreign-source");
			mkdirSync(source, { recursive: true, mode: 0o700 });
			writeFileSync(join(source, "auth.json"), LOGIN);
			await openPiSlot({
				authPath: join(source, "auth.json"),
				stashPath: join(source, STASH),
				provider: PROVIDER,
				placeholderKey: MARKER,
			}).enterVault();
			copyFileSync(join(source, "auth.json"), authPath);
			copyFileSync(join(source, STASH), stashPath);
		}
		const slot = openPiSlot({
			authPath,
			stashPath,
			provider: PROVIDER,
			placeholderKey: MARKER,
			lockTimeoutMs: 5000,
		});
		if (layout === "entered") await slot.enterVault();
		let upstream;
		try {
			upstream = await slot.inspect();
		} catch (error) {
			// An invalid stash cannot be classified by the slot; the guard reports
			// mismatched custody for it.
			upstream = { slot: "foreign", stash: "invalid", error: error.code };
		}
		custodyCases.push({ id: layout, upstream });
	}

	// ---- import-origin policy --------------------------------------------
	const isObject = (value) => typeof value === "object" && value !== null && !Array.isArray(value);
	const own = (value, key) => isObject(value) && Object.prototype.hasOwnProperty.call(value, key);
	const validToken = (value) => typeof value === "string" && /^[0-9a-f]{64}$/.test(value);
	const nonEmpty = (value) => typeof value === "string" && value.length > 0;
	const number = (value) => typeof value === "number" && Number.isFinite(value) && value >= 0;

	function safeSourcePath(path) {
		return (
			typeof path === "string" &&
			path.startsWith("/") &&
			path.endsWith(".jsonl") &&
			!path.includes("\\") &&
			!path.includes("\0") &&
			!path.split("/").includes("..")
		);
	}

	function markerCode(marker) {
		if (!isObject(marker)) return "invalid-import-marker";
		if (marker.source !== "pi") return "forged-custody-origin";
		if (typeof marker.sourcePath !== "string") return "invalid-import-marker";
		if (!safeSourcePath(marker.sourcePath)) return "unsafe-source-path";
		if (typeof marker.sha256 !== "string" || !/^[0-9a-fA-F]{64}$/.test(marker.sha256))
			return "invalid-import-marker";
		const version = marker.sessionVersion;
		if (!Number.isSafeInteger(version) || version < 0) return "invalid-import-marker";
		if (version < 1 || version > 3) return "unsupported-imported-version";
		return "imported-custody";
	}

	function importMarker(value) {
		if (!isObject(value)) return null;
		if (value.origin === "imported" || own(value, "importedFrom")) return "imported-custody";
		if (!own(value, "imported")) return null;
		return markerCode(value.imported);
	}

	function artifactCode(name) {
		const lower = name.toLowerCase();
		if (lower.includes("pi-import")) return "imported-location";
		if (lower.endsWith(".bak") || lower.endsWith(".backup") || lower.endsWith(".tmp") || lower.includes(".retired"))
			return "active-backup";
		return null;
	}

	function modeRecord(value) {
		if (!isObject(value)) return { code: "unknown-vault-mode" };
		if (own(value, "imported") || own(value, "importedFrom") || value.origin === "imported")
			return { code: "imported-custody" };
		for (const key of Object.keys(value))
			if (!["version", "mode", "routeId"].includes(key)) return { code: "unknown-vault-mode" };
		if (!Number.isSafeInteger(value.version) || value.version < 0)
			return { code: "unknown-vault-mode" };
		if (value.version !== MODE_VERSION) return { code: "obsolete-vault-mode" };
		if (value.mode !== "local" && value.mode !== "custody") return { code: "unknown-vault-mode" };
		let routeId = null;
		if (own(value, "routeId")) {
			if (typeof value.routeId !== "string" || value.routeId.trim() === "")
				return { code: "unknown-vault-mode" };
			routeId = value.routeId;
		}
		if (value.mode === "local" && routeId !== null) return { code: "unknown-vault-mode" };
		return { mode: value.mode, routeId };
	}

	function tokenGeneration(value) {
		if (!isObject(value) || !validToken(value.token)) return { code: "invalid-enrollment" };
		if (!(typeof value.token_generation === "number" && Number.isFinite(value.token_generation) && value.token_generation >= 1))
			return { code: "obsolete-enrollment" };
		return { generation: value.token_generation };
	}

	function stateAdmission(value) {
		if (!isObject(value) || value.version !== 1 || !nonEmpty(value.proposedName))
			return "invalid-enrollment";
		// Upstream decode_state validates the known fields and ignores extras.
		switch (value.phase) {
			case "pending":
				return validToken(value.requestSecret) &&
					(value.requestId === undefined || nonEmpty(value.requestId)) &&
					number(value.createdAt) &&
					number(value.updatedAt)
					? null
					: "invalid-enrollment";
			case "approved":
				return (value.approvedName === undefined || nonEmpty(value.approvedName)) &&
					typeof value.tokenGeneration === "number" &&
					value.tokenGeneration >= 1 &&
					number(value.updatedAt)
					? null
					: "invalid-enrollment";
			case "denied":
			case "blocked":
				return (value.errorCode === undefined || typeof value.errorCode === "string") &&
					number(value.updatedAt)
					? null
					: "invalid-enrollment";
			default:
				return "invalid-enrollment";
		}
	}

	function refuse(code, fresh) {
		return {
			allowed: false,
			mode: null,
			routeId: null,
			code,
			freshEnrollment: fresh,
			notice: fresh ? NOTICE : RESTORE,
		};
	}

	function admit(files, custody) {
		for (const name of files.artifacts ?? []) {
			const code = artifactCode(name);
			if (code) return refuse(code, true);
		}
		for (const value of [files.mode, files.enrollment_state, files.enrollment_token, files.stash]) {
			if (value === undefined || value === null) continue;
			const code = importMarker(value);
			if (code) return refuse(code, true);
		}
		if (custody.slot === "foreign" || custody.stash === "invalid") return refuse("mismatched-custody", false);
		const record = files.mode === undefined || files.mode === null ? { mode: "local", routeId: null } : modeRecord(files.mode);
		if (record.code) return refuse(record.code, true);
		if (files.stash !== undefined && files.stash !== null) {
			if (files.stash.version !== STASH_VERSION) return refuse("obsolete-custody-record", true);
			if (own(files.stash, "provider") && files.stash.provider !== PROVIDER)
				return refuse("mismatched-custody", false);
		}
		if (record.mode === "local") {
			if (custody.slot === "placeholder" || custody.stash === "valid") return refuse("active-custody", false);
			return { allowed: true, mode: "local", routeId: null, code: null, freshEnrollment: false, notice: null };
		}
		if (custody.slot === "original" || custody.slot === "empty") return refuse("unsynchronized-custody", false);
		if (custody.stash === "missing") return refuse("missing-custody", false);
		if (files.enrollment_token === undefined || files.enrollment_token === null)
			return refuse("missing-enrollment", true);
		const token = tokenGeneration(files.enrollment_token);
		if (token.code) return refuse(token.code, true);
		if (files.enrollment_state !== undefined && files.enrollment_state !== null) {
			const code = stateAdmission(files.enrollment_state);
			if (code) return refuse(code, true);
		}
		return {
			allowed: true,
			mode: "custody",
			routeId: record.routeId,
			code: null,
			freshEnrollment: false,
			notice: null,
		};
	}

	// ---- marker scenarios ------------------------------------------------
	const token = (extra = {}) => ({ token: "a".repeat(64), token_generation: 2, ...extra });
	const approved = (extra = {}) => ({
		version: 1,
		phase: "approved",
		proposedName: "antigravity-auth-pi",
		approvedName: "antigravity-auth-pi",
		tokenGeneration: 2,
		updatedAt: 1_729_000_000_000,
		...extra,
	});
	const imported = (extra = {}) => ({
		source: "pi",
		sourcePath: "/home/u/.pi/agent/sessions/a.jsonl",
		sha256: SHA,
		sessionVersion: 3,
		...extra,
	});
	const placeholder = { slot: "placeholder", stash: "valid" };

	const markerCases = [];
	const add = (id, files, custody) =>
		markerCases.push({ id, files, custody, result: admit(files, custody) });

	add("local-default", {}, { slot: "empty", stash: "missing" });
	add("local-login", { mode: { version: 1, mode: "local" } }, { slot: "original", stash: "missing" });
	add("local-placeholder-left", { mode: { version: 1, mode: "local" } }, placeholder);
	add("local-imported-mode", { mode: { version: 1, mode: "local", imported: imported() } }, { slot: "original", stash: "missing" });
	add("custody-valid", {
		mode: { version: 1, mode: "custody", routeId: "route-1" },
		enrollment_token: token(),
		enrollment_state: approved(),
		stash: { version: 1, provider: PROVIDER },
	}, placeholder);
	add("custody-without-route", {
		mode: { version: 1, mode: "custody" },
		enrollment_token: token(),
	}, placeholder);
	add("custody-no-token", {
		mode: { version: 1, mode: "custody" },
		stash: { version: 1, provider: PROVIDER },
	}, placeholder);
	add("custody-invalid-token", {
		mode: { version: 1, mode: "custody" },
		enrollment_token: { token: "not-hex", token_generation: 2 },
	}, placeholder);
	add("custody-obsolete-generation", {
		mode: { version: 1, mode: "custody" },
		enrollment_token: { token: "a".repeat(64), token_generation: 0 },
	}, placeholder);
	add("custody-invalid-state", {
		mode: { version: 1, mode: "custody" },
		enrollment_token: token(),
		enrollment_state: { version: 1, phase: "approved", proposedName: "x" },
	}, placeholder);
	add("custody-imported-state", {
		mode: { version: 1, mode: "custody" },
		enrollment_token: token(),
		enrollment_state: approved({ imported: imported() }),
	}, placeholder);
	add("custody-imported-token", {
		mode: { version: 1, mode: "custody" },
		enrollment_token: token({ imported: imported() }),
	}, placeholder);
	add("custody-forged-marker", {
		mode: { version: 1, mode: "custody" },
		enrollment_state: approved({ imported: imported({ source: "rpi" }) }),
	}, placeholder);
	add("custody-traversal-marker", {
		mode: { version: 1, mode: "custody" },
		enrollment_state: approved({ imported: imported({ sourcePath: "/home/u/.pi/../../etc/a.jsonl" }) }),
	}, placeholder);
	add("custody-legacy-origin", {
		mode: { version: 1, mode: "custody" },
		enrollment_state: approved({ origin: "imported" }),
	}, placeholder);
	add("custody-backup-artifact", {
		mode: { version: 1, mode: "custody" },
		enrollment_token: token(),
		artifacts: ["pi-enrollment-state.json.bak"],
	}, placeholder);
	add("custody-import-artifact", {
		mode: { version: 1, mode: "custody" },
		enrollment_token: token(),
		artifacts: ["sessions/pi-import/a.jsonl"],
	}, placeholder);
	add("custody-obsolete-mode", { mode: { version: 0, mode: "custody" } }, placeholder);
	add("custody-unknown-mode-key", { mode: { version: 1, mode: "custody", modeSource: "pi" } }, placeholder);
	add("custody-obsolete-stash", {
		mode: { version: 1, mode: "custody" },
		stash: { version: 0, provider: PROVIDER },
	}, placeholder);
	add("custody-foreign-provider", {
		mode: { version: 1, mode: "custody" },
		stash: { version: 1, provider: "openai" },
	}, placeholder);
	add("custody-foreign-slot", {
		mode: { version: 1, mode: "custody" },
		enrollment_token: token(),
	}, { slot: "original", stash: "missing" });
	add("custody-invalid-stash", {
		mode: { version: 1, mode: "custody" },
		enrollment_token: token(),
	}, { slot: "foreign", stash: "invalid" });

	const output = {
		status: "I02 package half: imported or foreign vault state carries no custody authority",
		reference: "a97f9055017f3c1546afbfe61a19d50b428be3d8",
		pi: "1.3.0",
		node: "24.22.0",
		upstreamHashes: hashes,
		counts: { custodyCases: custodyCases.length, markerCases: markerCases.length },
		custodyCases,
		markerCases,
	};
	const text = `${JSON.stringify(output, null, 2)}\n`;
	const fixturePath = fileURLToPath(new URL("./i02-import-guard.json", import.meta.url));
	if (write) {
		writeFileSync(fixturePath, text);
		console.log(`wrote ${fixturePath} (${custodyCases.length} custody, ${markerCases.length} marker cases)`);
	} else {
		const committed = readFileSync(fixturePath, "utf8");
		if (committed !== text) {
			console.error("i02-import-guard.json differs from the reference output; run with --write and review");
			process.exitCode = 1;
		} else {
			console.log(
				`i02-import-guard.json matches the reference (${custodyCases.length} custody, ${markerCases.length} marker cases)`,
			);
		}
	}
} finally {
	rmSync(root, { recursive: true, force: true });
}
