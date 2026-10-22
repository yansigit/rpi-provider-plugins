// Scripted Antigravity upstream for the combined parity run.
//
// The unchanged Pi extension and the rpi extension both speak HTTPS to
// `cloudcode-pa.googleapis.com` through their own transport. This process is a
// loopback HTTP CONNECT proxy that terminates TLS with the disposable
// `agy-server-cert.pem` fixture (SAN covers the real host name) and answers
// `v1internal:loadCodeAssist` and `v1internal:streamGenerateContent` from a
// scenario. Nothing leaves 127.0.0.1.
//
// Requests are recorded deterministically (no wall-clock fields) so both sides
// can be compared request-for-request.
import { createServer } from "node:net";
import { readFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { TLSSocket, createSecureContext } from "node:tls";
import path from "node:path";
import { fileURLToPath } from "node:url";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const CERT = path.join(HERE, "fixtures/tls/agy-server-cert.pem");
const KEY = path.join(HERE, "fixtures/tls/agy-server-key.pem");

const tokenTag = (token) => createHash("sha256").update(token).digest("hex").slice(0, 12);

// Minimal HTTP/1.1 request reader for a TLS socket. Both clients send one
// request per connection (`Connection: close` semantics); body framing is
// content-length or chunked.
function readRequests(socket, onRequest) {
  let buffer = Buffer.alloc(0);
  let handled = false;
  const processBuffer = () => {
    if (handled) return;
    const headEnd = buffer.indexOf("\r\n\r\n");
    if (headEnd < 0) return;
    const head = buffer.slice(0, headEnd).toString("latin1");
    const lines = head.split("\r\n");
    const [method, target, version] = (lines.shift() ?? "").split(" ");
    const headers = {};
    for (const line of lines) {
      const index = line.indexOf(":");
      if (index <= 0) continue;
      const key = line.slice(0, index).trim().toLowerCase();
      const value = line.slice(index + 1).trim();
      headers[key] = headers[key] === undefined ? value : `${headers[key]}, ${value}`;
    }
    let body = buffer.slice(headEnd + 4);
    const chunked = (headers["transfer-encoding"] ?? "").toLowerCase().includes("chunked");
    if (chunked) {
      const decoded = [];
      let rest = body;
      for (;;) {
        const lineEnd = rest.indexOf("\r\n");
        if (lineEnd < 0) return;
        const size = Number.parseInt(rest.slice(0, lineEnd).toString("latin1").split(";")[0], 16);
        if (Number.isNaN(size)) return;
        rest = rest.slice(lineEnd + 2);
        if (rest.length < size + 2) return;
        if (size === 0) break;
        decoded.push(rest.slice(0, size));
        rest = rest.slice(size + 2);
      }
      body = Buffer.concat(decoded);
    } else {
      const length = Number.parseInt(headers["content-length"] ?? "0", 10);
      if (body.length < length) return;
      body = body.slice(0, length);
    }
    handled = true;
    onRequest({ method, target, version, headers, body });
  };
  socket.on("data", (chunk) => {
    buffer = Buffer.concat([buffer, chunk]);
    if (!handled) processBuffer();
  });
  socket.once("end", () => {
    if (!handled) processBuffer();
  });
  processBuffer();
}

function sendResponse(socket, { status = 200, headers = {}, body = "" }) {
  const payload = Buffer.from(body);
  const lines = [`HTTP/1.1 ${status} OK`, `content-length: ${payload.length}`, "connection: close"];
  for (const [key, value] of Object.entries(headers)) lines.push(`${key}: ${value}`);
  socket.write(`${lines.join("\r\n")}\r\n\r\n`);
  socket.write(payload);
  socket.end();
}

function sendSse(socket, chunks) {
  socket.write("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n");
  for (const chunk of chunks) socket.write(`data: ${JSON.stringify(chunk)}\n\n`);
  socket.end();
}

function sseChunk(text, finishReason) {
  return {
    response: {
      candidates: [
        {
          content: { parts: [{ text }], role: "model" },
          ...(finishReason === undefined ? {} : { finishReason }),
        },
      ],
    },
  };
}

/**
 * Start the scripted upstream.
 *
 * scenario: {
 *   tokens: [{ token, project }],       // account credentials the run may use
 *   replies: ["text", ...],             // one per streamGenerateContent call
 *   usage: { promptTokenCount, candidatesTokenCount, totalTokenCount }
 * }
 */
export async function startAgyServer(scenario) {
  const secureContext = createSecureContext({
    cert: readFileSync(CERT),
    key: readFileSync(KEY),
  });
  const tokens = new Map((scenario.tokens ?? []).map((entry) => [entry.token, entry]));
  const replies = [...(scenario.replies ?? [])];
  const usage = scenario.usage ?? {
    promptTokenCount: 11,
    candidatesTokenCount: 7,
    totalTokenCount: 18,
  };
  const requests = [];
  let streamIndex = 0;

  const server = createServer((socket) => {
    let head = Buffer.alloc(0);
    const onData = (chunk) => {
      head = Buffer.concat([head, chunk]);
      const end = head.indexOf("\r\n\r\n");
      if (end < 0) return;
      socket.off("data", onData);
      const first = head.slice(0, end).toString("latin1").split("\r\n")[0] ?? "";
      const match = /^CONNECT\s+([^:\s]+):(\d+)\s+HTTP\/1\.[01]$/.exec(first);
      if (!match) {
        socket.end("HTTP/1.1 405 Method Not Allowed\r\ncontent-length: 0\r\n\r\n");
        return;
      }
      socket.write("HTTP/1.1 200 Connection Established\r\n\r\n");
      const tls = new TLSSocket(socket, { isServer: true, secureContext });
      tls.on("error", () => socket.destroy());
      readRequests(tls, (request) => {
        const token = (request.headers.authorization ?? "").replace(/^Bearer\s+/i, "");
        const account = tokens.get(token);
        const record = {
          method: request.method,
          target: request.target,
          host: match[1],
          authorization: token === "" ? "<none>" : `<token:${tokenTag(token)}>`,
          account: account?.label ?? (token === "" ? "<none>" : "<unknown>"),
          body: request.body.length === 0 ? null : safeJson(request.body),
        };
        requests.push(record);
        if (request.target.startsWith("/v1internal:loadCodeAssist")) {
          const project = account?.project ?? "fixture-project-unknown";
          sendResponse(tls, {
            headers: { "content-type": "application/json" },
            body: JSON.stringify({
              cloudaicompanionProject: project,
              currentTier: { id: "standard" },
            }),
          });
          return;
        }
        if (request.target.startsWith("/v1internal:streamGenerateContent")) {
          const text = replies[streamIndex] ?? `reply ${streamIndex + 1}`;
          const index = streamIndex;
          streamIndex += 1;
          // Three SSE frames: text, text, finish. Deterministic framing.
          const frames = [
            sseChunk(text.slice(0, Math.max(1, Math.floor(text.length / 2)))),
            sseChunk(text.slice(Math.max(1, Math.floor(text.length / 2)))),
            {
              response: {
                candidates: [
                  {
                    content: { parts: [{ text: "" }], role: "model" },
                    finishReason: "STOP",
                  },
                ],
                usageMetadata: usage,
              },
            },
          ];
          record.replyIndex = index;
          sendSse(tls, frames);
          return;
        }
        sendResponse(tls, { status: 404, body: JSON.stringify({ error: "unsupported" }) });
      });
    };
    socket.on("data", onData);
    socket.on("error", () => socket.destroy());
  });

  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  const { port } = server.address();
  return {
    proxyUrl: `http://127.0.0.1:${port}`,
    port,
    requests,
    unusedReplies: () => replies.slice(streamIndex),
    streamCount: () => streamIndex,
    close: () =>
      new Promise((resolve) => {
        server.close(() => resolve());
      }),
  };
}

function safeJson(buffer) {
  try {
    return JSON.parse(buffer.toString("utf8"));
  } catch {
    return { "<unparsed>": buffer.toString("utf8") };
  }
}
