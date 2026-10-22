// Normalization for the Pi 1.3.0 <-> rpi RPC differential runner.
//
// Each side has an explicit allowlist: an unknown event type or an unknown
// top-level field is an error, never a silent pass. Only the transforms listed
// in README.md are applied:
//   1. ordinal remapping of generated IDs (first appearance per side -> <id:N>)
//   2. removal of `timestamp` and `durationMs` keys at any depth
//   3. replacement of the side's absolute temp directory with <TMP>
//   4. the extension-UI presentation projection (see projectRpiDialog and
//      normalizePiUi); it maps only the select/confirm/input/notify shapes.
// Message content, tool calls, signatures, retries, usage numbers and event
// order are compared verbatim.

export class NormalizeError extends Error {
  constructor(code, message) {
    super(message);
    this.name = "NormalizeError";
    this.code = code;
  }
}

const SCRUBBED_KEYS = new Set(["timestamp", "durationMs"]);
const NOTIFY_KINDS = new Set(["info", "warning", "error"]);
const RPI_DIALOG_KINDS = new Set(["select", "confirm", "input", "settings"]);

// Top-level record allowlists. `ids` lists the fields holding generated IDs.
// Nested values are not validated; they are scrubbed and compared verbatim.
const RECORD_SPECS = {
  pi: {
    response: { fields: ["id", "command", "success", "data", "error"] },
    agent_start: { fields: [] },
    agent_end: { fields: ["messages", "willRetry"] },
    agent_settled: { fields: ["aborted"] },
    turn_start: { fields: [] },
    turn_end: { fields: ["message", "toolResults"] },
    message_start: { fields: ["message"] },
    message_update: { fields: ["usage", "assistantMessageEvent"] },
    message_end: { fields: ["message"] },
    tool_execution_start: { fields: ["toolCallId", "toolName", "args"], ids: ["toolCallId"] },
    tool_execution_update: {
      fields: ["toolCallId", "toolName", "args", "partialResult"],
      ids: ["toolCallId"],
    },
    tool_execution_end: {
      fields: ["toolCallId", "toolName", "result", "isError", "durationMs"],
      ids: ["toolCallId"],
    },
    queue_update: { fields: ["steering", "followUp"] },
    entry_appended: { fields: ["entry"] },
    session_info_changed: { fields: ["name"] },
    thinking_level_changed: { fields: ["level"] },
    compaction_start: { fields: ["reason"] },
    compaction_end: { fields: ["reason", "result", "aborted", "willRetry", "errorMessage"] },
    auto_retry_start: { fields: ["attempt", "maxAttempts", "delayMs", "errorMessage"] },
    auto_retry_end: { fields: ["success", "attempt", "finalError"] },
    summarization_retry_scheduled: {
      fields: ["attempt", "maxAttempts", "delayMs", "errorMessage"],
    },
    summarization_retry_attempt_start: { fields: ["source", "reason"] },
    summarization_retry_finished: { fields: [] },
    bash_execution_update: { fields: ["id", "delta"] },
    extension_error: { fields: ["extensionPath", "event", "error"] },
  },
  rpi: {
    ready: {
      fields: ["protocol", "piCompatibilityTarget", "supportedCommands", "semantics", "limits", "parityComplete"],
    },
    response: { fields: ["command", "success", "id", "data", "error"] },
    agent_start: { fields: [] },
    agent_end: { fields: ["messageCount", "messages"] },
    agent_settled: {
      fields: ["runId", "outcome", "aborted", "error", "summary", "pendingResume"],
      ids: ["runId"],
    },
    turn_start: { fields: [] },
    turn_end: { fields: ["message", "toolResultCount", "toolResults"] },
    message_start: { fields: ["message"] },
    message_update: {
      fields: ["message", "assistantMessageEvent", "eventType", "contentIndex", "delta"],
    },
    message_end: { fields: ["message"] },
    tool_execution_start: {
      fields: ["recorded", "toolCallId", "toolName", "args"],
      ids: ["toolCallId"],
    },
    tool_execution_update: {
      fields: ["toolCallId", "toolName", "args", "partialResult"],
      ids: ["toolCallId"],
    },
    tool_execution_end: {
      fields: ["recorded", "toolCallId", "toolName", "isError", "result"],
      ids: ["toolCallId"],
    },
    retry_scheduled: { fields: ["attempt", "maxRetries", "delayMs", "error"] },
    ui_dialog_closed: { fields: ["requestId"], ids: ["requestId"] },
  },
};

// Allowed top-level fields per Pi extension_ui_request method.
const PI_UI_METHODS = {
  select: ["id", "method", "title", "options", "timeout"],
  confirm: ["id", "method", "title", "message", "timeout"],
  input: ["id", "method", "title", "placeholder", "timeout"],
  editor: ["id", "method", "title", "prefill", "timeout"],
  notify: ["id", "method", "message", "notifyType"],
  setStatus: ["id", "method", "statusKey", "statusText"],
  setWidget: ["id", "method", "widgetKey", "widgetLines", "widgetPlacement"],
  setTitle: ["id", "method", "title"],
  set_editor_text: ["id", "method", "text"],
};

// Dialog methods that expect an extension_ui_response.
export const PI_DIALOG_METHODS = new Set(["select", "confirm", "input", "editor"]);

// The rpi nativeDialog keys that the projection understands, per kind.
const RPI_PROJECTED_KEYS = {
  select: ["kind", "title", "choices"],
  confirm: ["kind", "title", "description"],
  input: ["kind", "title", "placeholder"],
};
const RPI_CHOICE_KEYS = ["value", "label"];

const isPlainObject = (value) =>
  value !== null && typeof value === "object" && !Array.isArray(value);

// Values that the rpi dialog serializer leaves at their defaults.
const isDefault = (value) =>
  value === undefined ||
  value === null ||
  value === false ||
  value === "" ||
  (Array.isArray(value) && value.length === 0);

export class OrdinalIds {
  constructor() {
    this.ordinals = new Map();
  }

  remap(raw) {
    if (raw === null || raw === undefined) return raw;
    if (typeof raw !== "string") {
      throw new NormalizeError("bad-id", `generated ID must be a string, got ${typeof raw}`);
    }
    let ordinal = this.ordinals.get(raw);
    if (ordinal === undefined) {
      ordinal = this.ordinals.size + 1;
      this.ordinals.set(raw, ordinal);
    }
    return `<id:${ordinal}>`;
  }
}

// Replace every occurrence of each root directory with <TMP>, longest first.
function replaceRoots(text, roots) {
  let out = text;
  for (const root of roots) {
    if (root) out = out.split(root).join("<TMP>");
  }
  return out;
}

// Deep transform: drop timestamp/durationMs keys and scrub temp roots.
export function scrub(value, roots) {
  if (typeof value === "string") return replaceRoots(value, roots);
  if (Array.isArray(value)) return value.map((item) => scrub(item, roots));
  if (isPlainObject(value)) {
    const out = {};
    for (const key of Object.keys(value)) {
      if (SCRUBBED_KEYS.has(key)) continue;
      out[key] = scrub(value[key], roots);
    }
    return out;
  }
  return value;
}

// Timestamp and duration keys are removed wherever they appear, so they pass
// the field check at the top level too.
function checkFields(record, allowed, side) {
  for (const key of Object.keys(record)) {
    if (SCRUBBED_KEYS.has(key)) continue;
    if (!allowed.includes(key)) {
      throw new NormalizeError(
        "unknown-field",
        `${side} ${record.type ?? record.method ?? "record"}: unknown field "${key}"`,
      );
    }
  }
}

function pickScrubbed(record, keys, ctx) {
  const out = {};
  for (const key of keys) {
    if (key in record) out[key] = scrub(record[key], ctx.roots);
  }
  return out;
}

function normalizePiUi(record, ctx) {
  const allowed = PI_UI_METHODS[record.method];
  if (!allowed) {
    throw new NormalizeError("unknown-event", `pi extension_ui_request: unknown method "${record.method}"`);
  }
  checkFields(record, ["type", ...allowed], "pi");
  if (record.method === "notify") {
    // rpi ui_notification carries no request ID; the Pi notify ID is dropped.
    const kind = record.notifyType ?? "info";
    if (!NOTIFY_KINDS.has(kind)) {
      throw new NormalizeError("bad-value", `pi notify: unsupported notifyType "${kind}"`);
    }
    const out = { type: "extension_ui_request", method: "notify", notifyType: kind };
    if ("message" in record) out.message = scrub(record.message, ctx.roots);
    return out;
  }
  const out = pickScrubbed(record, allowed.filter((key) => key !== "id"), ctx);
  if ("id" in record) out.id = ctx.ids.remap(record.id);
  return { type: "extension_ui_request", ...out };
}

function checkDefaults(nativeDialog, projected) {
  for (const key of Object.keys(nativeDialog)) {
    if (projected.includes(key)) continue;
    if (!isDefault(nativeDialog[key])) {
      throw new NormalizeError(
        "unprojectable-field",
        `rpi native dialog "${nativeDialog.kind}": field "${key}" has no Pi counterpart`,
      );
    }
  }
}

// Project an rpi ui_dialog_request onto the Pi extension_ui_request shape.
// select: choices[].label <-> options[]; rpi choice values are rpi handles and
// are not part of the canonical form, so answers address them by position.
function projectRpiDialog(record, ctx) {
  checkFields(record, ["type", "requestId", "toolCallId", "ui"], "rpi");
  const nativeDialog = record.ui?.nativeDialog;
  if (!isPlainObject(nativeDialog)) {
    throw new NormalizeError("unprojectable-ui", "rpi ui_dialog_request without ui.nativeDialog");
  }
  const kind = nativeDialog.kind;
  if (!RPI_DIALOG_KINDS.has(kind)) {
    throw new NormalizeError("unprojectable-ui", `rpi native dialog kind "${kind}" is unknown`);
  }
  if (kind === "settings") {
    // No Pi counterpart; kept verbatim so the difference is reported.
    const out = { type: "ui_dialog_request", requestId: ctx.ids.remap(record.requestId), ui: scrub(record.ui, ctx.roots) };
    if (!isDefault(record.toolCallId)) out.toolCallId = ctx.ids.remap(record.toolCallId);
    return out;
  }
  checkDefaults(nativeDialog, RPI_PROJECTED_KEYS[kind]);
  const out = { type: "extension_ui_request", id: ctx.ids.remap(record.requestId), method: kind };
  if (!isDefault(record.toolCallId)) out.toolCallId = ctx.ids.remap(record.toolCallId);
  if (!isDefault(nativeDialog.title)) out.title = scrub(nativeDialog.title, ctx.roots);
  if (kind === "select") {
    if (!Array.isArray(nativeDialog.choices)) {
      throw new NormalizeError("unprojectable-ui", "rpi select dialog without choices");
    }
    out.options = nativeDialog.choices.map((choice) => {
      if (!isPlainObject(choice)) throw new NormalizeError("unprojectable-ui", "rpi choice is not an object");
      checkDefaults(choice, RPI_CHOICE_KEYS);
      return scrub(choice.label, ctx.roots);
    });
  } else if (kind === "confirm") {
    if (!isDefault(nativeDialog.description)) out.message = scrub(nativeDialog.description, ctx.roots);
  } else if (!isDefault(nativeDialog.placeholder)) {
    out.placeholder = scrub(nativeDialog.placeholder, ctx.roots);
  }
  return out;
}

function projectRpiNotification(record, ctx) {
  checkFields(record, ["type", "text", "kind"], "rpi");
  if (!NOTIFY_KINDS.has(record.kind)) {
    throw new NormalizeError("bad-value", `rpi ui_notification: unsupported kind "${record.kind}"`);
  }
  return {
    type: "extension_ui_request",
    method: "notify",
    message: scrub(record.text, ctx.roots),
    notifyType: record.kind,
  };
}

// Normalize one raw record from `side` ("pi" or "rpi"). Throws NormalizeError.
export function normalizeRecord(side, record, ctx) {
  if (!isPlainObject(record) || typeof record.type !== "string") {
    throw new NormalizeError("no-type", `${side}: record without a string "type"`);
  }
  if (side === "pi" && record.type === "extension_ui_request") return normalizePiUi(record, ctx);
  if (side === "rpi" && record.type === "ui_dialog_request") return projectRpiDialog(record, ctx);
  if (side === "rpi" && record.type === "ui_notification") return projectRpiNotification(record, ctx);
  const spec = RECORD_SPECS[side]?.[record.type];
  if (!spec) {
    throw new NormalizeError("unknown-event", `${side}: unknown event type "${record.type}"`);
  }
  checkFields(record, ["type", ...spec.fields], side);
  const out = { type: record.type };
  for (const key of spec.fields) {
    if (!(key in record) || SCRUBBED_KEYS.has(key)) continue;
    out[key] = spec.ids?.includes(key) ? ctx.ids.remap(record[key]) : scrub(record[key], ctx.roots);
  }
  return out;
}

// Normalize a whole transcript for one side. `roots` are absolute paths of the
// side's temp directory (both the given and the realpath form).
export function normalizeTranscript(side, records, roots) {
  const ctx = { ids: new OrdinalIds(), roots: [...roots].filter(Boolean).sort((a, b) => b.length - a.length) };
  return records.map((record, index) => {
    try {
      return normalizeRecord(side, record, ctx);
    } catch (error) {
      if (error instanceof NormalizeError) {
        error.message = `${side} record ${index}: ${error.message}`;
      }
      throw error;
    }
  });
}

// Deep structural diff returning JSON paths where the two values differ.
export function diffPaths(a, b, path = "$", out = [], limit = 50) {
  if (out.length >= limit) return out;
  if (Array.isArray(a) && Array.isArray(b)) {
    const n = Math.max(a.length, b.length);
    for (let i = 0; i < n; i++) {
      if (i >= a.length || i >= b.length) out.push(`${path}[${i}]`);
      else diffPaths(a[i], b[i], `${path}[${i}]`, out, limit);
      if (out.length >= limit) break;
    }
    if (a.length !== b.length && !out.includes(`${path}.length`)) out.push(`${path}.length`);
    return out;
  }
  if (isPlainObject(a) && isPlainObject(b)) {
    const keys = [...new Set([...Object.keys(a), ...Object.keys(b)])].sort();
    for (const key of keys) {
      if (!(key in a) || !(key in b)) out.push(`${path}.${key}`);
      else diffPaths(a[key], b[key], `${path}.${key}`, out, limit);
      if (out.length >= limit) break;
    }
    return out;
  }
  if (a !== b) out.push(path);
  return out;
}

const recordType = (record) => (isPlainObject(record) && typeof record.type === "string" ? record.type : null);

// Compare two normalized transcripts by position. Event order is part of the
// comparison, so a shifted record shows up as a mismatch at every later index.
export function compareNormalized(piRecords, rpiRecords) {
  const mismatches = [];
  const n = Math.max(piRecords.length, rpiRecords.length);
  for (let index = 0; index < n; index++) {
    const pi = piRecords[index];
    const rpi = rpiRecords[index];
    if (index >= piRecords.length || index >= rpiRecords.length) {
      mismatches.push({
        index,
        pi: recordType(pi),
        rpi: recordType(rpi),
        paths: [index >= piRecords.length ? "missing in pi" : "missing in rpi"],
      });
      continue;
    }
    const paths = diffPaths(pi, rpi);
    if (paths.length > 0) {
      mismatches.push({ index, pi: recordType(pi), rpi: recordType(rpi), paths });
    }
  }
  return {
    identical: mismatches.length === 0,
    piRecords: piRecords.length,
    rpiRecords: rpiRecords.length,
    mismatches,
  };
}

// Deterministic JSON text: object keys sorted, two-space indent.
export function canonicalJson(value) {
  return JSON.stringify(sortKeys(value), null, 2) + "\n";
}

function sortKeys(value) {
  if (Array.isArray(value)) return value.map(sortKeys);
  if (isPlainObject(value)) {
    const out = {};
    for (const key of Object.keys(value).sort()) out[key] = sortKeys(value[key]);
    return out;
  }
  return value;
}

// Build the answer for a scripted dialog. `request` is the raw record from
// `side`. Returns the record to write to that side's stdin.
export function answerFor(side, request, scripted) {
  let kind;
  let id;
  let labels = [];
  let choices = [];
  if (side === "pi") {
    if (!PI_DIALOG_METHODS.has(request.method)) {
      throw new NormalizeError("dialog-mismatch", `pi: ${request.method} does not take an answer`);
    }
    kind = request.method;
    id = request.id;
    labels = request.options ?? [];
  } else {
    if (request.type !== "ui_dialog_request") {
      throw new NormalizeError("dialog-mismatch", `rpi: ${request.type} does not take an answer`);
    }
    kind = request.ui?.nativeDialog?.kind;
    id = request.requestId;
    choices = request.ui?.nativeDialog?.choices ?? [];
    labels = choices.map((choice) => choice.label);
  }
  if (kind !== scripted.kind) {
    throw new NormalizeError(
      "dialog-mismatch",
      `${side}: request is "${kind}" but the next scripted dialog is "${scripted.kind}"`,
    );
  }
  if (scripted.title !== undefined) {
    const title = side === "pi" ? request.title : request.ui?.nativeDialog?.title;
    if (title !== scripted.title) {
      throw new NormalizeError("dialog-mismatch", `${side}: dialog title "${title}" != scripted "${scripted.title}"`);
    }
  }
  if (scripted.cancel === true) {
    return side === "pi"
      ? { type: "extension_ui_response", id, cancelled: true }
      : { type: "ui_dialog_response", requestId: id, cancelled: true };
  }
  const answer = scripted.answer ?? {};
  if (side === "pi") {
    if (kind === "select") {
      if (!labels.includes(answer.choice)) {
        throw new NormalizeError("dialog-mismatch", `select answer "${answer.choice}" is not an option`);
      }
      return { type: "extension_ui_response", id, value: answer.choice };
    }
    if (kind === "confirm") {
      if (typeof answer.confirmed !== "boolean") throw new NormalizeError("dialog-mismatch", "confirm needs a boolean");
      return { type: "extension_ui_response", id, confirmed: answer.confirmed };
    }
    if (typeof answer.value !== "string") throw new NormalizeError("dialog-mismatch", "input needs a string value");
    return { type: "extension_ui_response", id, value: answer.value };
  }
  if (kind === "select") {
    const index = labels.indexOf(answer.choice);
    if (index < 0) throw new NormalizeError("dialog-mismatch", `select answer "${answer.choice}" is not an option`);
    return {
      type: "ui_dialog_response",
      requestId: id,
      answer: { type: "select", values: [choices[index].value] },
    };
  }
  if (kind === "confirm") {
    if (typeof answer.confirmed !== "boolean") throw new NormalizeError("dialog-mismatch", "confirm needs a boolean");
    return { type: "ui_dialog_response", requestId: id, answer: { type: "confirm", confirmed: answer.confirmed } };
  }
  if (typeof answer.value !== "string") throw new NormalizeError("dialog-mismatch", "input needs a string value");
  return { type: "ui_dialog_response", requestId: id, answer: { type: "input", value: answer.value } };
}

// True for a record that expects an answer on that side.
export function isDialogRequest(side, record) {
  if (side === "pi") return record?.type === "extension_ui_request" && PI_DIALOG_METHODS.has(record.method);
  return record?.type === "ui_dialog_request";
}
