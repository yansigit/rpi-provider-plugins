# parity-rpc: Pi 1.3.0 vs rpi RPC differential runner

Drives the reference Pi 1.3.0 and rpi in RPC mode with the same scripted
session, answers their extension UI dialogs from the scenario, and compares the
two transcripts after an explicit, minimal normalization. Plain Node 24, no
npm dependencies.

## Usage

```sh
node tools/parity-rpc/run.mjs <scenario.json> \
  --pi <pi dist/bundle/cli.js> \
  --rpi <rpi binary or .mjs shim> \
  [--rpi-extension <path> ...] [--pi-extension <path> ...] \
  [--out <dir>] [--work <dir>]
```

`PARITY_PI_CLI` and `PARITY_RPI_BIN` can replace `--pi` and `--rpi`.

Exit status: `0` transcripts identical, `1` transcripts differ, `2` run error
(scenario invalid, a side failed, a dialog did not match, or normalization
rejected a record).

Each side gets a fresh work directory with its own `home`, `agent` (set as
`PI_CODING_AGENT_DIR` or `RPI_CODING_AGENT_DIR`) and `cwd`. `~/.pi`, `~/.rpi`,
provider credentials and `*_BASE_URL`/proxy variables are never passed to a
child. Model traffic goes to a local fake OpenAI-compatible server
(`fixtures/fake-openai.mjs`) on 127.0.0.1.

Output in `--out` (default `<work>/out`):

| file | content |
| --- | --- |
| `pi.raw.jsonl`, `rpi.raw.jsonl` | every stdout line, unmodified |
| `*.inputs.jsonl` | every line written to the side's stdin |
| `*.provider-requests.jsonl` | every request the fake provider received |
| `*.stderr.log` | child stderr |
| `*.normalized.json` | the normalized transcript per side |
| `diff.json` | mismatched record indexes, JSON paths, and both records |
| `summary.json` | status, error, counts, type sequence, exit codes |

## Scenario format

```json
{
  "name": "prompt-basic",
  "provider": { "name": "parity", "model": "parity-model",
                "replies": ["reply text"], "chunkChars": 1000, "chunkDelayMs": 0 },
  "systemPrompt": "Parity fixed system prompt.",
  "args": [],
  "timeoutMs": 30000,
  "steps": [
    { "send": { "id": "p1", "type": "prompt", "message": "hi" },
      "await": [{ "type": "agent_settled" }] }
  ],
  "dialogs": [
    { "kind": "select", "title": "Pick", "answer": { "choice": "Block" } },
    { "kind": "confirm", "cancel": true }
  ]
}
```

- `send` is written to both sides. If it has an `id`, the runner waits for the
  matching `response` record.
- `await` waits for records of a type (and optional `id`), `count` times.
- `dialogs` are consumed in order. A request whose kind differs from the next
  scripted dialog, a select choice that is not an option, or a scripted title
  that does not match fails the run. An unscripted request also fails, and so
  does a scripted dialog that is never requested.

## Normalization (exhaustive)

Only these transforms are applied. Anything else is compared verbatim.

1. **Event allowlist per side.** `normalize.mjs` lists every event type Pi and
   rpi may emit and the top-level fields each may carry. An unknown event type
   or an unknown top-level field fails with `unknown-event` or `unknown-field`.
   Nested values are not validated.
2. **Extension UI methods.** Pi `extension_ui_request` methods are allowlisted
   (`select`, `confirm`, `input`, `editor`, `notify`, `setStatus`, `setWidget`,
   `setTitle`, `set_editor_text`) with their fields. Unknown methods fail.
3. **Timestamps and durations.** Keys named `timestamp` and `durationMs` are
   removed at any depth.
4. **Temp paths.** The side's work directory, both as given and as realpath, is
   replaced by `<TMP>` in every string.
5. **Generated IDs.** The fields `toolCallId`, `runId`, `requestId` (rpi
   `ready`/`agent_settled`/`ui_dialog_*`) and the Pi extension UI `id` are
   replaced by `<id:N>`, where N is the order of first appearance on that side.
   Two runs with different random IDs but the same structure compare equal.
6. **Dialog presentation projection.** Only the Pi `extension_ui_request`
   shapes for select/confirm/input and the rpi `ui_dialog_request` shapes for
   the same kinds map to one canonical form:
   - select: rpi `choices[].label` ⇄ Pi `options[]`. rpi choice `value`
     handles are not part of the canonical form. Answers address them by
     position.
   - confirm: rpi `description` ⇄ Pi `message`.
   - input: rpi `placeholder` ⇄ Pi `placeholder`.
   - An rpi nativeDialog field that is at its serialized default (`false`,
     `""`, `null`, `[]`, absent) is ignored. Any other field without a Pi
     counterpart fails with `unprojectable-field`.
   - rpi `settings` dialogs have no Pi counterpart. They are kept verbatim so
     the difference is reported.
   - notify: Pi `extension_ui_request` method `notify` ⇄ rpi
     `ui_notification` (`text` ⇄ `message`, `kind` ⇄ `notifyType`). The Pi
     notify `id` is dropped. A missing Pi `notifyType` defaults to `info`.
7. **Numbers.** Compared by value, as JavaScript numbers.

Not normalized: message content, tool calls and arguments, signatures,
retries, usage and cost numbers, response `data`, and event order. Records
are compared by position, so one inserted or missing record shows up as a
mismatch at every later index.

## Preload (Pi side)

`preload.mjs` is loaded with `node --import` before Pi's CLI. It makes
`Date.now()`, `new Date()` (no arguments), `Math.random()`,
`crypto.randomUUID()`, `crypto.randomBytes()` and the global
`crypto.getRandomValues()` deterministic: a fixed-start clock that advances 1 ms
per read, and a fixed-seed generator. Pi's own code is not modified.

rpi is not preloaded. Its nondeterminism is handled only in the normalizer.
In the prompt scenario, that is the wall-clock `timestamp` and `durationMs`
values (removed) and a UUIDv7 `runId` (remapped).

## Tests

```sh
node --test tools/parity-rpc/test/normalize.test.mjs \
  tools/parity-rpc/test/preload.test.mjs \
  tools/parity-rpc/test/runner.test.mjs \
  tools/parity-rpc/test/e2e.test.mjs
```

Node's test runner does not expand a directory argument on Node 24. Pass the
files explicitly.

- `normalize.test.mjs`: unknown events and fields are rejected; ID remapping
  is stable per side; timestamps and durations are removed; temp roots are
  replaced; presentation mapping and answers round-trip.
- `preload.test.mjs`: two processes see the same clock and randomness.
- `runner.test.mjs`: uses `fixtures/fake-agent.mjs` for both sides to test the
  dialog path, mismatch errors, and byte-identical summaries.
- `e2e.test.mjs`: runs `fixtures/scenarios/prompt-basic.json` on the real Pi
  bundle and the real rpi binary against the same scripted provider. It checks
  that both sides return the same assistant text and pins the set of known
  divergences in `fixtures/expected/prompt-basic.summary.json`. A new or
  resolved difference fails the test until the pin is regenerated on purpose
  with `PARITY_RPC_WRITE_EXPECTED=1`.

Environment for the e2e test: `PARITY_PI_CLI` (default: the Pi 1.3.0 bundle
under `provider-references`) and `PARITY_RPI_BIN` (default:
`/home/user/Projects/.targets/x-rpc/debug/rpi`). `PARITY_RPC_KEEP=1` keeps
the work directories under `~/.cache/parity-rpc/tests`.

## Known divergences (pinned, not normalized)

Recorded from `prompt-basic` on Pi 1.3.0 and rpi 0.7.0 at pi-rust 3b17e9d:

Event types, in order (from `summary.json` `typeSequence`):

- Pi: `response, agent_start, turn_start, message_start, message_end,
  message_start, message_end, message_start, message_update x3, message_end,
  turn_end, agent_end, agent_settled`.
- rpi: `ready, response, agent_start, turn_start, message_start,
  message_update x3, message_end, turn_end, agent_end, agent_settled`.

So rpi has a `ready` record that Pi lacks. Pi emits a `message_start`/
`message_end` pair for the system message and another for the user message,
and rpi emits neither.

Field-level differences in the same records:

- The `response` record: Pi's `data` is `{disposition: "started"}`, and rpi's
  is `{accepted: true}`.
- The `message` objects: Pi uses `api`, `provider`, `model`, `usage`,
  `stopReason`, `timestamp` and `kind`/`sections` fields. rpi uses `kind`,
  `responseId`, `responseModel` and `rawStopReason`, and its usage numbers
  differ.
- The `message_update` records: Pi carries `usage` and `assistantMessageEvent`
  (`text_start`, `text_delta`, `text_end`). rpi carries `message`,
  `assistantMessageEvent` and `eventType` (same three values), and no
  top-level `usage`.

The fake provider sees different requests from the two sides. Pi's system
prompt has a `<cwd>` section appended, and its user message is a content-part
array. rpi's system prompt has no `<cwd>` section, and its user message is a
plain string. Request bodies are recorded in `*.provider-requests.jsonl` but
are not compared by the runner.

## Limits

- One scenario and one provider so far. Tool calls, compaction, retries,
  extension commands, and abort are not covered.
- Comparison is positional, so a single inserted record produces a mismatch
  at every later index. This is intentional. Shifted event order is a
  difference.
- The provider request bodies are not compared. Only their count is.
- The preload does not cover `performance.now()`, `Date()` called as a
  function, timers, the `node:crypto` `getRandomValues` export, or entropy taken
  outside the patched APIs.
- The fake provider is OpenAI-compatible chat completions only. The scenario
  uses `api: "openai-completions"` through `models.json`.
- `ensureCacheDir` uses `~/.cache/parity-rpc` for work directories. That path
  is under `$HOME`, and it is never `~/.pi` or `~/.rpi`.
