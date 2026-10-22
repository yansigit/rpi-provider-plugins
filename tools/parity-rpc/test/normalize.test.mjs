import assert from "node:assert/strict";
import { test } from "node:test";
import {
  answerFor,
  compareNormalized,
  diffPaths,
  normalizeRecord,
  normalizeTranscript,
  NormalizeError,
  OrdinalIds,
} from "../normalize.mjs";

const ROOT = "/work/run-1/pi";
// A fresh ordinal table per call, so each record is judged on its own IDs.
const ctx = () => ({ ids: new OrdinalIds(), roots: [ROOT] });

function expectNormalizeError(fn, code) {
  assert.throws(fn, (error) => error instanceof NormalizeError && error.code === code, `expected ${code}`);
}

test("unknown event types fail on both sides", () => {
  expectNormalizeError(() => normalizeTranscript("pi", [{ type: "mystery" }], []), "unknown-event");
  expectNormalizeError(() => normalizeTranscript("rpi", [{ type: "mystery" }], []), "unknown-event");
  expectNormalizeError(() => normalizeTranscript("pi", [{ id: "x" }], []), "no-type");
});

test("unknown top-level fields fail, even on a known event", () => {
  expectNormalizeError(
    () => normalizeTranscript("pi", [{ type: "agent_end", messages: [], willRetry: false, extra: 1 }], []),
    "unknown-field",
  );
  expectNormalizeError(
    () => normalizeTranscript("rpi", [{ type: "agent_settled", aborted: false, zzz: 1 }], []),
    "unknown-field",
  );
});

test("unknown extension UI methods and unknown fields inside a UI method fail", () => {
  expectNormalizeError(
    () => normalizeTranscript("pi", [{ type: "extension_ui_request", id: "a", method: "frobnicate" }], []),
    "unknown-event",
  );
  expectNormalizeError(
    () =>
      normalizeTranscript(
        "pi",
        [{ type: "extension_ui_request", id: "a", method: "select", title: "T", options: [], extra: true }],
        [],
      ),
    "unknown-field",
  );
});

test("generated IDs are remapped by first appearance, per side", () => {
  const pi = normalizeTranscript(
    "pi",
    [
      { type: "tool_execution_start", toolCallId: "call-A", toolName: "read", args: {} },
      { type: "tool_execution_end", toolCallId: "call-A", toolName: "read", result: {}, isError: false },
      { type: "tool_execution_start", toolCallId: "call-B", toolName: "read", args: {} },
    ],
    [],
  );
  assert.deepEqual(
    pi.map((record) => record.toolCallId),
    ["<id:1>", "<id:1>", "<id:2>"],
  );
  const rpi = normalizeTranscript("rpi", [{ type: "agent_settled", runId: "run-zz", aborted: false }], []);
  assert.equal(rpi[0].runId, "<id:1>", "a fresh side starts its own ordinals");
});

test("ID remapping makes two runs with different random IDs identical", () => {
  const run = (a, b) =>
    normalizeTranscript(
      "rpi",
      [
        { type: "tool_execution_start", toolCallId: a, toolName: "bash", args: { command: "ls" } },
        { type: "agent_settled", runId: b, aborted: false },
      ],
      [],
    );
  assert.deepEqual(run("0193-aaaa", "0193-bbbb"), run("0199-cccc", "0199-dddd"));
  assert.deepEqual(run("same", "same"), run("same", "same"));
  assert.notDeepEqual(run("same", "same"), run("same", "other"), "distinct IDs stay distinct");
});

test("timestamp and durationMs are removed at any depth; retry delays are kept", () => {
  const [message] = normalizeTranscript(
    "pi",
    [{ type: "message_end", message: { role: "user", timestamp: 1, nested: { timestamp: 2, keep: 3 } } }],
    [],
  );
  assert.deepEqual(message, { type: "message_end", message: { role: "user", nested: { keep: 3 } } });

  const [end] = normalizeTranscript(
    "pi",
    [{ type: "tool_execution_end", toolCallId: "c", toolName: "t", result: {}, isError: false, durationMs: 17 }],
    [],
  );
  assert.equal("durationMs" in end, false);

  const [retry] = normalizeTranscript(
    "pi",
    [{ type: "auto_retry_start", attempt: 1, maxAttempts: 3, delayMs: 2000, errorMessage: "529" }],
    [],
  );
  assert.equal(retry.delayMs, 2000);
});

test("absolute temp roots are replaced with <TMP> in every string", () => {
  const [record] = normalizeTranscript(
    "pi",
    [{ type: "message_start", message: { content: `cwd ${ROOT}/cwd and ${ROOT}/agent` } }],
    [ROOT],
  );
  assert.equal(record.message.content, "cwd <TMP>/cwd and <TMP>/agent");
});

test("message content, usage and event order are compared, not normalized", () => {
  const base = [
    { type: "message_update", usage: { input: 10, output: 6 }, assistantMessageEvent: { type: "text_delta", contentIndex: 0, delta: "hi" } },
    { type: "turn_end", message: {}, toolResults: [] },
  ];
  const changedUsage = structuredClone(base);
  changedUsage[0].usage.input = 11;
  const swapped = [base[1], base[0]];
  const reworded = structuredClone(base);
  reworded[0].assistantMessageEvent.delta = "hey";

  const left = normalizeTranscript("pi", base, []);
  for (const other of [changedUsage, swapped, reworded]) {
    const right = normalizeTranscript("pi", other, []);
    assert.equal(compareNormalized(left, right).identical, false);
  }
  assert.equal(compareNormalized(left, normalizeTranscript("pi", base, [])).identical, true);
});

test("select dialog projects to the same canonical form on both sides", () => {
  const pi = normalizeRecord(
    "pi",
    { type: "extension_ui_request", id: "p-1", method: "select", title: "Pick", options: ["Allow", "Block"] },
    ctx(),
  );
  const rpi = normalizeRecord(
    "rpi",
    {
      type: "ui_dialog_request",
      requestId: "r-9",
      toolCallId: null,
      ui: { nativeDialog: { kind: "select", title: "Pick", choices: [{ value: "v0", label: "Allow" }, { value: "v1", label: "Block" }] } },
    },
    ctx(),
  );
  assert.deepEqual(pi, rpi);
  assert.deepEqual(pi, { type: "extension_ui_request", id: "<id:1>", method: "select", title: "Pick", options: ["Allow", "Block"] });
});

test("confirm and input dialogs project to the same canonical form", () => {
  const confirmPi = normalizeRecord("pi", { type: "extension_ui_request", id: "a", method: "confirm", title: "Sure?", message: "Really?" }, ctx());
  const confirmRpi = normalizeRecord(
    "rpi",
    { type: "ui_dialog_request", requestId: "b", toolCallId: null, ui: { nativeDialog: { kind: "confirm", title: "Sure?", description: "Really?" } } },
    ctx(),
  );
  assert.deepEqual(confirmPi, confirmRpi);

  const inputPi = normalizeRecord("pi", { type: "extension_ui_request", id: "a", method: "input", title: "Name", placeholder: "type" }, ctx());
  const inputRpi = normalizeRecord(
    "rpi",
    { type: "ui_dialog_request", requestId: "b", toolCallId: null, ui: { nativeDialog: { kind: "input", title: "Name", placeholder: "type" } } },
    ctx(),
  );
  assert.deepEqual(inputPi, inputRpi);
});

test("notify projects to ui_notification; a missing Pi notifyType defaults to info", () => {
  const pi = normalizeRecord("pi", { type: "extension_ui_request", id: "n-1", method: "notify", message: "saved", notifyType: "warning" }, ctx());
  const rpi = normalizeRecord("rpi", { type: "ui_notification", text: "saved", kind: "warning" }, ctx());
  assert.deepEqual(pi, rpi);
  assert.equal("id" in pi, false, "the Pi notify ID has no rpi counterpart and is dropped");

  const defaulted = normalizeRecord("pi", { type: "extension_ui_request", id: "n-2", method: "notify", message: "hi" }, ctx());
  assert.equal(defaulted.notifyType, "info");
});

test("rpi dialog fields without a Pi counterpart fail the projection", () => {
  const withDescription = {
    type: "ui_dialog_request",
    requestId: "r",
    toolCallId: null,
    ui: { nativeDialog: { kind: "select", title: "T", choices: [{ value: "v0", label: "A", description: "extra" }] } },
  };
  expectNormalizeError(() => normalizeRecord("rpi", withDescription, ctx()), "unprojectable-field");

  const trimmed = { type: "ui_dialog_request", requestId: "r", toolCallId: null, ui: { nativeDialog: { kind: "input", title: "T", trim: true } } };
  expectNormalizeError(() => normalizeRecord("rpi", trimmed, ctx()), "unprojectable-field");

  const noNative = { type: "ui_dialog_request", requestId: "r", toolCallId: null, ui: { other: {} } };
  expectNormalizeError(() => normalizeRecord("rpi", noNative, ctx()), "unprojectable-ui");

  const unknownKind = { type: "ui_dialog_request", requestId: "r", toolCallId: null, ui: { nativeDialog: { kind: "frob", title: "T" } } };
  expectNormalizeError(() => normalizeRecord("rpi", unknownKind, ctx()), "unprojectable-ui");
});

test("rpi defaults at their serialized values are not counted as fields", () => {
  const record = {
    type: "ui_dialog_request",
    requestId: "r",
    toolCallId: null,
    ui: { nativeDialog: { kind: "select", title: "T", choices: [{ value: "v0", label: "A" }], searchable: false, multiple: false, query: "", selected: null, settings: [] } },
  };
  assert.deepEqual(normalizeRecord("rpi", record, ctx()), {
    type: "extension_ui_request",
    id: "<id:1>",
    method: "select",
    title: "T",
    options: ["A"],
  });
});

test("rpi settings dialogs have no Pi counterpart and are kept verbatim", () => {
  const record = { type: "ui_dialog_request", requestId: "r", toolCallId: null, ui: { nativeDialog: { kind: "settings", title: "S", settings: [{ id: "a" }] } } };
  const out = normalizeRecord("rpi", record, ctx());
  assert.equal(out.type, "ui_dialog_request");
  assert.equal(out.requestId, "<id:1>");
});

test("answers are mapped per side and map back to the scripted labels", () => {
  const piRequest = { type: "extension_ui_request", id: "p-1", method: "select", title: "Pick", options: ["Allow", "Block"] };
  const rpiRequest = {
    type: "ui_dialog_request",
    requestId: "r-9",
    toolCallId: null,
    ui: { nativeDialog: { kind: "select", title: "Pick", choices: [{ value: "v0", label: "Allow" }, { value: "v1", label: "Block" }] } },
  };
  const scripted = { kind: "select", title: "Pick", answer: { choice: "Block" } };

  const piAnswer = answerFor("pi", piRequest, scripted);
  assert.deepEqual(piAnswer, { type: "extension_ui_response", id: "p-1", value: "Block" });

  const rpiAnswer = answerFor("rpi", rpiRequest, scripted);
  assert.deepEqual(rpiAnswer, { type: "ui_dialog_response", requestId: "r-9", answer: { type: "select", values: ["v1"] } });
  const chosen = rpiRequest.ui.nativeDialog.choices.find((choice) => choice.value === rpiAnswer.answer.values[0]);
  assert.equal(chosen.label, scripted.answer.choice, "the rpi answer names the scripted choice");
});

test("confirm, input and cancel answers have the documented shapes", () => {
  const piConfirm = { type: "extension_ui_request", id: "a", method: "confirm", title: "Sure?", message: "m" };
  assert.deepEqual(answerFor("pi", piConfirm, { kind: "confirm", answer: { confirmed: true } }), {
    type: "extension_ui_response",
    id: "a",
    confirmed: true,
  });
  const rpiInput = { type: "ui_dialog_request", requestId: "b", toolCallId: null, ui: { nativeDialog: { kind: "input", title: "N" } } };
  assert.deepEqual(answerFor("rpi", rpiInput, { kind: "input", answer: { value: "typed" } }), {
    type: "ui_dialog_response",
    requestId: "b",
    answer: { type: "input", value: "typed" },
  });
  assert.deepEqual(answerFor("pi", piConfirm, { kind: "confirm", cancel: true }), {
    type: "extension_ui_response",
    id: "a",
    cancelled: true,
  });
  assert.deepEqual(answerFor("rpi", rpiInput, { kind: "input", cancel: true }), {
    type: "ui_dialog_response",
    requestId: "b",
    cancelled: true,
  });
});

test("a kind mismatch, an unknown choice or a title mismatch fails the answer", () => {
  const piSelect = { type: "extension_ui_request", id: "a", method: "select", title: "Pick", options: ["A"] };
  expectNormalizeError(() => answerFor("pi", piSelect, { kind: "confirm", answer: { confirmed: true } }), "dialog-mismatch");
  expectNormalizeError(() => answerFor("pi", piSelect, { kind: "select", answer: { choice: "Z" } }), "dialog-mismatch");
  expectNormalizeError(() => answerFor("pi", piSelect, { kind: "select", title: "Other", answer: { choice: "A" } }), "dialog-mismatch");
});

test("compareNormalized reports paths and missing records", () => {
  const same = compareNormalized([{ type: "a", x: 1 }], [{ type: "a", x: 1 }]);
  assert.equal(same.identical, true);

  const changed = compareNormalized([{ type: "a", x: 1 }], [{ type: "a", x: 2 }]);
  assert.equal(changed.identical, false);
  assert.deepEqual(changed.mismatches, [{ index: 0, pi: "a", rpi: "a", paths: ["$.x"] }]);

  const extra = compareNormalized([{ type: "a" }, { type: "b" }], [{ type: "a" }]);
  assert.deepEqual(extra.mismatches, [{ index: 1, pi: "b", rpi: null, paths: ["missing in rpi"] }]);
});

test("diffPaths lists array index and length differences", () => {
  assert.deepEqual(diffPaths({ a: [1, 2] }, { a: [1, 3, 4] }), ["$.a[1]", "$.a[2]", "$.a.length"]);
});
