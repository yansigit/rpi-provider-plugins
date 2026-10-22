// Scripted OpenAI-compatible chat completions server (127.0.0.1 only).
//
// Pi 1.3.0 and rpi are pointed at this server through models.json with api
// "openai-completions". Each request consumes the next scripted reply; the
// reply is streamed as fixed-size text deltas so both sides see the same wire
// bytes. Requests are recorded for the run log. The runner gives both sides the
// same port (one side at a time), because the endpoint is echoed by Pi's
// get_state and must not differ between the two transcripts.
import http from "node:http";

// Characters per streamed content chunk. A reply sent as one chunk gives both
// sides the same delta structure; smaller chunks expose delta granularity.
const DEFAULT_CHUNK_CHARS = 8;

function sse(object) {
  return `data: ${JSON.stringify(object)}\n\n`;
}

export async function startFakeOpenAI({ replies, host = "127.0.0.1", port = 0, chunkChars = DEFAULT_CHUNK_CHARS, chunkDelayMs = 0 }) {
  if (!Array.isArray(replies) || replies.length === 0) {
    throw new Error("fake provider needs at least one scripted reply");
  }
  const requests = [];
  let next = 0;
  const server = http.createServer((req, res) => {
    let body = "";
    req.setEncoding("utf8");
    req.on("data", (chunk) => {
      body += chunk;
    });
    req.on("end", () => {
      let parsed = null;
      try {
        parsed = JSON.parse(body);
      } catch {
        parsed = { unparsed: body };
      }
      requests.push({ method: req.method, url: req.url, body: parsed });
      if (req.method !== "POST" || !req.url.endsWith("/chat/completions")) {
        res.writeHead(404, { "content-type": "application/json" });
        res.end(JSON.stringify({ error: { message: "not found" } }));
        return;
      }
      if (next >= replies.length) {
        res.writeHead(500, { "content-type": "application/json" });
        res.end(JSON.stringify({ error: { message: "no scripted reply left" } }));
        return;
      }
      const reply = replies[next];
      const id = `chatcmpl-parity-${next + 1}`;
      next += 1;
      const base = { id, object: "chat.completion.chunk", created: 1700000000, model: parsed?.model ?? "parity-model" };
      const chunks = [sse({ ...base, choices: [{ index: 0, delta: { role: "assistant", content: "" }, finish_reason: null }] })];
      for (let offset = 0; offset < reply.length; offset += chunkChars) {
        const piece = reply.slice(offset, offset + chunkChars);
        chunks.push(sse({ ...base, choices: [{ index: 0, delta: { content: piece }, finish_reason: null }] }));
      }
      chunks.push(
        sse({
          ...base,
          choices: [{ index: 0, delta: {}, finish_reason: "stop" }],
          usage: {
            prompt_tokens: 10,
            completion_tokens: Math.ceil(reply.length / 4),
            total_tokens: 10 + Math.ceil(reply.length / 4),
          },
        }),
      );
      chunks.push("data: [DONE]\n\n");
      res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache" });
      // Optional pause between chunks, to tell a streaming reader from one that
      // buffers the whole body.
      (async () => {
        for (const chunk of chunks) {
          res.write(chunk);
          if (chunkDelayMs > 0) await new Promise((resolve) => setTimeout(resolve, chunkDelayMs));
        }
        res.end();
      })();
    });
  });
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(port, host, resolve);
  });
  const bound = server.address().port;
  return {
    baseUrl: `http://${host}:${bound}`,
    requests,
    unusedReplies: () => replies.length - next,
    close: () => new Promise((resolve) => server.close(() => resolve())),
  };
}
