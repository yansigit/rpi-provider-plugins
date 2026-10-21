import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { start } from "../worker/test/rpi/peer-harness.mjs";

test("packaged dispatcher runs retained hooks, auth, commands and both streams through acknowledged host ports", { timeout: 30_000 }, async t => {
	const cwd = await mkdtemp(join(tmpdir(), "rpi-cursor-runtime-"));
	t.after(() => rm(cwd, { recursive: true, force: true }));
	const peer = await start(t, { entry: new URL("../runtime/main.mjs", import.meta.url), cwd });
	let next = 0;
	const begin = (method, input) => {
		const id = `h${++next}`; peer.send({ id, kind: "request", method, input });
		return { id, result: async () => {
			const frame = await peer.take(frame => frame.id === id && ["reply", "error"].includes(frame.kind));
			if (frame.kind === "error") throw new Error(frame.error);
			return frame.result;
		} };
	};
	const open = begin("runtime.open", { cwd, agentDir: join(cwd, "rpi-agent"), sessionDir: join(cwd, "sessions") });
	const ready = await peer.take(frame => frame.id === open.id);
	assert.equal(ready.kind, "event", JSON.stringify(ready));
	assert.equal(ready.event.type, "ready");
	peer.send({ id: open.id, kind: "credit", sequence: ready.sequence });
	const manifest = ready.event.manifest;
	assert.equal(manifest.commands.length, 10);
	assert(manifest.flags.some(flag => flag.name === "cursor-no-fast"));
	assert(manifest.events.includes("session_compact_failed"));
	assert(manifest.tools.some(tool => tool.name === "cursor_activate_skill"));
	const model = manifest.provider.models[0], { baseUrl: _base, headers: _headers, ...publicModel } = model;
	const session = { metadata: { id: "fixture-session", createdAt: 1000 }, revision: 0, name: null, labels: {}, entries: [], branches: [{ lane: "main", revision: 0, leafId: null }] };
	const origins = new Set(), trace = [], failures = [];
	let originNumber = 0, revision = 0, operation = null, appendGate = Promise.resolve();
	const view = () => ({ snapshot: structuredClone(session), leafId: session.branches[0].leafId, branchRevision: 0,
		projection: { model: null, thinkingLevel: "off", entries: session.entries.map(entry => ({ sourceEntry: entry, messages: [] })) } });
	async function host(method, input) {
		if (method === "scoped") {
			assert(origins.has(input.scope), "scoped call must retain its origin until release acknowledgement");
			return host(input.method, input.input);
		}
		trace.push({ method, input: structuredClone(input) });
		assert.equal(method, "native");
		switch (input.port) {
			case "context":
				if (input.action === "flags") return { "cursor-no-fast": true };
				assert.equal(input.action, "snapshot");
				return { cwd, mode: "print", hasUI: false, projectTrusted: false, projectTrustResolved: false, isIdle: operation === null, hasPendingMessages: false,
					systemPrompt: "native prompt", sessionId: session.metadata.id, model: publicModel, tools: [], identity: operation };
			case "tools":
				if (input.action === "setActive") return null;
				assert.equal(input.action, "inspect"); return [];
			case "auth": assert.equal(input.action, "resolve"); return null;
			case "ui": return { ok: true };
			case "providers": {
				if (input.action === "publish") return { revision: ++revision };
				assert.equal(input.action, "refresh");
				assert.equal(input.allowNetwork, false);
				assert.deepEqual(await begin("provider.catalog", { publicationId: `publication-${revision + 1}`, allowNetwork: false, force: false }).result(), { published: true });
				return { revision };
			}
			case "sessions": {
				if (input.action === "capture") { const origin = `origin-${++originNumber}`; origins.add(origin); return { origin, snapshot: structuredClone(session) }; }
				assert(origins.has(input.origin));
				switch (input.action) {
					case "location": return { path: null };
					case "view": return view();
					case "snapshot": return structuredClone(session);
					case "sync": return { durability: "ephemeral" };
					case "release": origins.delete(input.origin); return null;
					case "append": case "journalAppend": {
						await appendGate;
						const entry = { type: "custom", id: `committed-${++session.revision}`, seq: session.revision,
							parentId: session.branches[0].leafId, timestamp: 1000 + session.revision, customType: input.customType,
							...(Object.hasOwn(input, "data") ? { data: input.data } : {}) };
						session.entries.push(entry); session.branches[0].leafId = entry.id;
						return { entry, revision: session.revision, durability: "ephemeral", advancedLane: true };
					}
				}
			}
		}
		throw new Error(`Unexpected native host request: ${method}/${input.port}/${input.action}`);
	}
	async function pump() {
		for (;;) {
			const request = await peer.take(frame => frame.kind === "request");
			void host(request.method, request.input).then(result => peer.send({ id: request.id, kind: "reply", result: result ?? null, lastEvent: 0 }), error => {
				failures.push(error.message); peer.send({ id: request.id, kind: "error", error: error.message, lastEvent: 0 });
			});
		}
	}
	void pump();
	await begin("runtime.configure", {}).result();
	await begin("extension.event", { event: { type: "session_start", reason: "startup" } }).result();
	assert(revision > 0, "catalog refresh must reenter the worker while session_start awaits its host call");
	for (let index = 0; index < 70; index++) {
		operation = { operationId: `info-${index}`, purpose: "session_info_changed" };
		await begin("extension.event", { event: { type: "session_info_changed", name: `name-${index}` } }).result();
	}
	operation = null;
	const login = await begin("provider.auth", { action: "login", input: { methodId: "api_key" } }).result();
	assert.equal(login.kind, "interaction");
	let release; appendGate = new Promise(resolve => { release = resolve; });
	const command = begin("extension.command", { name: "cursor-mode", args: "plan" });
	let completed = false; const commandResult = command.result().then(value => { completed = true; return value; });
	while (!trace.some(item => item.input.port === "sessions" && item.input.action === "append" && item.input.customType.includes("mode"))) await new Promise(resolve => setTimeout(resolve, 5));
	assert.equal(completed, false); release(); await commandResult;
	assert(session.entries.some(entry => entry.customType.includes("mode")));
	for (const entry of ["stream", "streamSimple"]) {
		operation = { operationId: `operation-${entry}`, purpose: "prompt" };
		const stream = begin("provider.stream", { entry, model, context: { messages: [{ role: "user", content: "offline", timestamp: 1 }] }, options: { env: {} }, authMode: "replace" });
		const events = [];
		for (;;) {
			const frame = await peer.take(frame => frame.id === stream.id);
			if (frame.kind === "event") { events.push(frame.event); peer.send({ id: stream.id, kind: "credit", sequence: frame.sequence }); }
			else { assert.equal(frame.kind, "reply", JSON.stringify(frame)); assert.match(frame.result.errorMessage, /API key/i); break; }
		}
		assert.equal(events.at(-1).type, "error");
		await begin("operation.end", { operationId: operation.operationId }).result();
	}
	operation = null;
	await begin("extension.event", { event: { type: "session_shutdown", reason: "quit" } }).result();
	peer.send({ id: open.id, kind: "cancel" });
	await open.result();
	assert.equal(origins.size, 0);
	assert.deepEqual(failures, []);
});
