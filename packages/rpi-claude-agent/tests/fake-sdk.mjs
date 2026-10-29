// Fake @anthropic-ai/claude-agent-sdk, loaded with RPI_CLAUDE_AGENT_SDK=<this file>.
// It reacts to the user text and logs each query and user message as JSON lines
// to RPI_CLAUDE_FAKE_LOG. Anthropic's service is never contacted.
import { appendFileSync } from "node:fs";

let queries = 0;
const log = entry => process.env.RPI_CLAUDE_FAKE_LOG && appendFileSync(process.env.RPI_CLAUDE_FAKE_LOG, JSON.stringify(entry) + "\n");

export function tool(name, description, inputSchema, handler) { return { name, description, inputSchema, handler }; }
export function createSdkMcpServer({ name, tools }) { return { type: "sdk", name, tools }; }

export function query({ prompt, options }) {
	const n = ++queries;
	const { env, mcpServers, canUseTool, ...rest } = options;
	log({ event: "query", n, options: { ...rest, tools: options.tools, mcpTools: mcpServers.rpi.tools.map(t => t.name) },
		tokenOk: !!env.CLAUDE_CODE_OAUTH_TOKEN && env.CLAUDE_CODE_OAUTH_TOKEN === process.env.RPI_CLAUDE_FAKE_EXPECT_TOKEN,
		apiKeyInEnv: "ANTHROPIC_API_KEY" in env, configDir: env.CLAUDE_CONFIG_DIR });
	let interrupted, closed = false;
	const interruption = new Promise(resolve => { interrupted = resolve; });
	const base = { parent_tool_use_id: null, uuid: "u", session_id: "s" };
	const ev = event => ({ type: "stream_event", event, ...base });
	const textBlock = (index, text) => [
		ev({ type: "content_block_start", index, content_block: { type: "text", text: "" } }),
		...text.match(/.{1,6}/g).map(chunk => ev({ type: "content_block_delta", index, delta: { type: "text_delta", text: chunk } })),
		ev({ type: "content_block_stop", index }),
	];
	const response = (blocks, stop) => [
		ev({ type: "message_start", message: { usage: { input_tokens: 10, output_tokens: 1, cache_read_input_tokens: 2 } } }),
		...blocks,
		ev({ type: "message_delta", delta: { stop_reason: stop }, usage: { output_tokens: 5 } }),
		ev({ type: "message_stop" }),
	];
	const success = { type: "result", subtype: "success", is_error: false, result: "", ...base };
	async function* run() {
		for await (const user of prompt) {
			const said = typeof user.message.content === "string" ? user.message.content : user.message.content.map(b => b.text ?? "").join("");
			log({ event: "user", n, text: said });
			if (said.includes("use a tool")) {
				yield* response([
					...textBlock(0, "Reading."),
					ev({ type: "content_block_start", index: 1, content_block: { type: "tool_use", id: "toolu_1", name: "mcp__rpi__read", input: {} } }),
					ev({ type: "content_block_delta", index: 1, delta: { type: "input_json_delta", partial_json: "{\"path\":" } }),
					ev({ type: "content_block_delta", index: 1, delta: { type: "input_json_delta", partial_json: "\"note.txt\"}" } }),
					ev({ type: "content_block_stop", index: 1 }),
				], "tool_use");
				const allowed = await canUseTool("mcp__rpi__read", { path: "note.txt" }, {});
				const denied = await canUseTool("Bash", { command: "ls" }, {});
				log({ event: "permission", n, allowed: allowed.behavior, denied: denied.behavior });
				const handler = mcpServers.rpi.tools.find(t => t.name === "read").handler;
				const result = await handler({ path: "note.txt" }, { _meta: { "claudecode/toolUseId": "toolu_1" } });
				log({ event: "tool_result", n, result });
				yield* response(textBlock(0, `Tool said: ${result.content[0].text}`), "end_turn");
				yield success;
			} else if (said.includes("hang")) {
				yield* response(textBlock(0, "Working"), "end_turn").slice(0, 3);
				await interruption;
				log({ event: "interrupted", n });
				yield { type: "result", subtype: "error_during_execution", is_error: true, errors: ["interrupted"], ...base };
			} else if (said.includes("limit")) {
				yield { type: "rate_limit_event", rate_limit_info: { status: "rejected", resetsAt: 1767225600, rateLimitType: "five_hour" }, ...base };
				yield { type: "assistant", error: "rate_limit", message: { content: [{ type: "text", text: "You've hit your limit" }] }, ...base };
				yield { type: "result", subtype: "success", is_error: true, result: "You've hit your limit", ...base };
			} else {
				yield* response(textBlock(0, `Echo: ${said.slice(-40)}`), "end_turn");
				yield success;
			}
		}
	}
	const iterator = run();
	return Object.assign(iterator, {
		interrupt: async () => { interrupted(); },
		close: () => { if (!closed) { closed = true; log({ event: "close", n }); } },
	});
}
