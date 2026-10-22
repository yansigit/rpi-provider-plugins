// Protocol-shaped stand-in for Pi 1.3.0 (PARITY_FAKE_SIDE=pi) or rpi
// (PARITY_FAKE_SIDE=rpi). Used only by the runner's own tests, so the dialog
// projection and the normalizer are exercised without the real binaries.
//
// Prompt messages:
//   plain            response, agent_start, agent_settled
//   dialog:select    select dialog, then a notify with the answer, then settle
//   dialog:confirm   confirm dialog (same flow)
//   dialog:input     input dialog (same flow)
//   unknown-field    settles with a field that is not on the allowlist
import readline from "node:readline";

const side = process.env.PARITY_FAKE_SIDE;
if (side !== "pi" && side !== "rpi") {
  process.stderr.write("PARITY_FAKE_SIDE must be pi or rpi\n");
  process.exit(2);
}

let counter = 0;
let pending = null;
const choiceLabels = { v0: "Allow", v1: "Block" };

function emit(record) {
  process.stdout.write(`${JSON.stringify(record)}\n`);
}

function notify(text) {
  if (side === "pi") {
    counter += 1;
    emit({ type: "extension_ui_request", id: `pi-note-${counter}`, method: "notify", message: text, notifyType: "info" });
  } else {
    emit({ type: "ui_notification", text, kind: "info" });
  }
}

function settle(extra = {}) {
  emit({ type: "agent_settled", aborted: false, ...extra });
}

function openDialog(kind) {
  counter += 1;
  if (side === "pi") {
    const id = `pi-ui-${counter}`;
    if (kind === "select") {
      emit({ type: "extension_ui_request", id, method: "select", title: "Pick", options: ["Allow", "Block"] });
    } else if (kind === "confirm") {
      emit({ type: "extension_ui_request", id, method: "confirm", title: "Sure?", message: "Really?" });
    } else {
      emit({ type: "extension_ui_request", id, method: "input", title: "Name", placeholder: "type" });
    }
    return id;
  }
  const requestId = `rpi-req-${counter}`;
  const nativeDialog =
    kind === "select"
      ? { kind, title: "Pick", choices: [{ value: "v0", label: "Allow" }, { value: "v1", label: "Block" }] }
      : kind === "confirm"
        ? { kind, title: "Sure?", description: "Really?" }
        : { kind, title: "Name", placeholder: "type" };
  emit({ type: "ui_dialog_request", requestId, toolCallId: null, ui: { nativeDialog } });
  return requestId;
}

function resultText(kind, answer) {
  if (answer.cancelled) return "cancelled";
  if (side === "pi") {
    if (kind === "confirm") return `confirmed=${answer.confirmed}`;
    return `value=${answer.value}`;
  }
  const payload = answer.answer;
  if (payload.type === "confirm") return `confirmed=${payload.confirmed}`;
  if (payload.type === "select") return `value=${choiceLabels[payload.values[0]]}`;
  return `value=${payload.value}`;
}

function handlePrompt(command) {
  const message = command.message;
  emit({
    type: "response",
    id: command.id,
    command: "prompt",
    success: true,
    data: {},
  });
  if (message === "plain") {
    emit({ type: "agent_start" });
    settle();
    return;
  }
  if (message === "unknown-field") {
    settle({ zzz: 1 });
    return;
  }
  const match = /^dialog:(select|confirm|input)$/.exec(message);
  if (!match) {
    emit({ type: "agent_start" });
    settle();
    return;
  }
  const kind = match[1];
  const requestId = openDialog(kind);
  pending = { kind, requestId };
}

function handleAnswer(command) {
  if (!pending) {
    process.stderr.write("answer without a pending dialog\n");
    process.exit(3);
  }
  const expected = side === "pi" ? command.id : command.requestId;
  if (expected !== pending.requestId) {
    process.stderr.write(`answer for ${expected}, expected ${pending.requestId}\n`);
    process.exit(3);
  }
  const text = resultText(pending.kind, command);
  pending = null;
  notify(`result:${text}`);
  settle();
}

const rl = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });
rl.on("line", (line) => {
  if (line.trim() === "") return;
  const command = JSON.parse(line);
  if (command.type === "prompt") handlePrompt(command);
  else if (command.type === "extension_ui_response" || command.type === "ui_dialog_response") handleAnswer(command);
  else {
    process.stderr.write(`unexpected command ${command.type}\n`);
    process.exit(4);
  }
});
rl.on("close", () => process.exit(0));
