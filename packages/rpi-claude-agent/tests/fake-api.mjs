// Fake Anthropic Messages API for running the real Agent SDK and its bundled
// Claude Code offline (inside a network namespace with only loopback).
//   node fake-api.mjs <port> <log>
// First reply: a tool_use of rpi's read tool. Once the tool_result is in the
// history: a text reply saying whether the file body arrived.
import { createServer } from "node:http";
import { appendFileSync } from "node:fs";

const [port, logFile] = process.argv.slice(2);
const log = line => appendFileSync(logFile, line + "\n");
createServer((request, response) => {
	let body = "";
	request.on("data", chunk => { body += chunk; });
	request.on("end", () => {
		log(JSON.stringify({ method: request.method, url: request.url, bearer: /^Bearer /.test(request.headers.authorization ?? ""), apiKeyHeader: "x-api-key" in request.headers }));
		if (!request.url.startsWith("/v1/messages") || request.url.includes("count_tokens")) {
			response.writeHead(404, { "content-type": "application/json" });
			return response.end('{"type":"error","error":{"type":"not_found_error","message":"offline fake"}}');
		}
		const json = JSON.parse(body || "{}");
		const history = JSON.stringify(json.messages);
		log(JSON.stringify({ model: json.model, tools: (json.tools ?? []).map(tool => tool.name) }));
		if (!json.stream) {
			response.writeHead(200, { "content-type": "application/json" });
			return response.end(JSON.stringify({ id: "msg_0", type: "message", role: "assistant", model: json.model, content: [{ type: "text", text: "ok" }], stop_reason: "end_turn", usage: { input_tokens: 1, output_tokens: 1 } }));
		}
		response.writeHead(200, { "content-type": "text/event-stream" });
		const event = (type, data) => response.write(`event: ${type}\ndata: ${JSON.stringify({ type, ...data })}\n\n`);
		event("message_start", { message: { id: "msg_1", type: "message", role: "assistant", model: json.model, content: [], stop_reason: null, usage: { input_tokens: 7, output_tokens: 1 } } });
		if (!history.includes("tool_result")) {
			event("content_block_start", { index: 0, content_block: { type: "tool_use", id: "toolu_offline_1", name: "mcp__rpi__read", input: {} } });
			event("content_block_delta", { index: 0, delta: { type: "input_json_delta", partial_json: '{"path":"note.txt"}' } });
			event("content_block_stop", { index: 0 });
			event("message_delta", { delta: { stop_reason: "tool_use" }, usage: { output_tokens: 3 } });
		} else {
			const seen = history.includes("file body from rpi") ? "SEEN" : "MISSING";
			event("content_block_start", { index: 0, content_block: { type: "text", text: "" } });
			event("content_block_delta", { index: 0, delta: { type: "text_delta", text: `Offline tool result ${seen}` } });
			event("content_block_stop", { index: 0 });
			event("message_delta", { delta: { stop_reason: "end_turn" }, usage: { output_tokens: 4 } });
		}
		event("message_stop", {});
		response.end();
	});
}).listen(Number(port), "127.0.0.1");
