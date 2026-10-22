# Fake `@cursor/sdk` for offline parity runs

A scenario-driven stand-in for `@cursor/sdk@1.0.37`. It lets the unchanged Pi 1.3.0
reference (with the pinned `pi-cursor-sdk@0.5.3` extension) and rpi's retained Cursor
worker run the same scripted turns with no Cursor network access. The fake makes no
outbound requests of its own; the only HTTP it issues is the MCP JSON-RPC call to a
bridge URL that the adapter supplies in `mcpServers`.

## Files

| File | Purpose |
| --- | --- |
| `sdk.mjs` | The fake SDK. Exports the runtime surface the adapter imports. |
| `sqlite.mjs` | Fake for the `@cursor/sdk/sqlite` subpath (`SqliteLocalAgentStore`). |
| `hooks.mjs` | Node resolve hook. Redirects `@cursor/sdk` and `@cursor/sdk/sqlite`. Inert unless `RPI_CURSOR_FAKE_SCENARIO` is set. |
| `register.mjs` | `--import` entry that registers `hooks.mjs`. |
| `scenarios/*.json` | Scenario files (`rpi-cursor-fake-sdk/1` schema). |
| `fixtures/echo-extension.mjs` | Pi extension that adds a `fake_echo` tool, used by `tool-turn`. |
| `run-reference.mjs` | Runs every scenario against the unchanged Pi 1.3.0 CLI, twice each, and checks counts and determinism. |
| `evidence.json` | Committed output of `run-reference.mjs --write`. |

## Usage

Inject the fake into any Node process that loads the Cursor SDK:

```sh
NODE_OPTIONS="--import /abs/path/tests/fake-sdk/register.mjs" \
RPI_CURSOR_FAKE_SCENARIO=/abs/path/tests/fake-sdk/scenarios/text-turn.json \
RPI_CURSOR_FAKE_LOG=/tmp/opencode/fake.jsonl \
RPI_CURSOR_FAKE_LOG_ROOT=/tmp/opencode \
  node <program>
```

Environment variables:

- `RPI_CURSOR_FAKE_SCENARIO` (required to activate the fake). Path to a scenario file.
- `RPI_CURSOR_FAKE_LOG` (optional). Append-only JSONL log of every SDK call, run, delta, bridge request, tool execution and a final `process_counters` record.
- `RPI_CURSOR_FAKE_LOG_ROOT` (optional). Prefix replaced by `<root>` in the log.
- `RPI_CURSOR_FAKE_HOME` (optional). Persistent fake state directory. Without it, state lives only in the process.

Secrets are never logged: `apiKey` appears as `"<redacted>"`.

Reference check (check mode compares against `evidence.json`):

```sh
cd packages/rpi-cursor-sdk
source /home/user/Projects/.parity-env.sh && lane_env c-fake
node tests/fake-sdk/run-reference.mjs            # check mode
node tests/fake-sdk/run-reference.mjs --write    # regenerate evidence.json
node tests/fake-sdk/run-reference.mjs --only tool-turn --keep /tmp/opencode/fk
```

`run-reference.mjs` runs each scenario through `pi --mode json` with `HOME`,
`PI_CODING_AGENT_DIR` and all fake paths inside a temporary directory under `$HOME`,
which is removed afterwards. It requires Node 24 or newer.

## Scenario format

```jsonc
{
  "schema": "rpi-cursor-fake-sdk/1",
  "name": "text-turn",
  "run": { "model": "composer-2", "prompt": "...", "extensions": [], "args": [], "followUps": [] },
  "models": [ /* ModelListItem values for Cursor.models.list */ ],
  "repositories": [ /* Cursor.repositories.list */ ],
  "account": { /* Cursor.me extras */ },
  "cloud": { "artifacts": [ { "path", "content" } ] },
  "turns": [
    {
      "name": "...",
      "expect": { "promptIncludes": "...", "imageCount": 0, "mode": "agent", "modelId": "..." },
      "prUrl": "...",
      "steps": [ /* ops below */ ],
      "outcome": { "status": "finished", "result": "..." },
      "sendError": { "name": "RateLimitError", "message": "...", "code": "...", "status": 429 }
    }
  ],
  "expect": { "exitCode": 0, "sdkCalls": {}, "runs": {}, "bridgeToolExecutions": 0, "toolExecutions": {}, "nativeToolCalls": {}, "piToolExecutions": {}, "violations": 0 }
}
```

Turns are consumed in order, one per `agent.send`. A turn's `expect` block is checked
when the send starts; a mismatch fails the run with `fake_expectation_failed`.

Step ops:

- `thinking` (`chunks`, optional `completed`): thinking deltas.
- `text` (`chunks`, optional `step`): text deltas and an assistant message step.
- `nativeTool` (`callId`, `toolCall: { type, args, result }`, optional `phase: "started-only"`): a native Cursor tool such as `shell`.
- `mcpCall` (`callId`, `server`, `tool`, `arguments`): a real MCP `tools/call` to the bridge URL that the adapter supplied. Each execution is logged as `tool_execution` with `source: "bridge"`.
- `usage` (`usage`, optional `cost`): turn-end usage.
- `delay`, `hang`: timing control. `hang` waits until cancelled.
- `error`: a turn-level error.

## Surface covered

Mirrors the declarations in `@cursor/sdk@1.0.37` for the members the adapter uses:

- Errors: `CursorSdkError`, `CursorAgentError`, `AuthenticationError`, `RateLimitError`,
  `ConfigurationError`, `AgentBusyError`, `NetworkError`, `UnknownAgentError`,
  `AgentNotFoundError`, `IntegrationNotConnectedError`, `UnsupportedRunOperationError`,
  plus `wrapSdkError`, `convertError`, `toRunError`.
- `Agent.create` / `resume` / `prompt` / `list` / `listRuns` / `getRun` / `cancelRun` /
  `get` / `archive` / `unarchive` / `delete` / `getUsage` / `messages.list`. Cloud agents
  (`options.cloud` with repos, env and metadata) and local agents (`local.cwd`, `store`).
- `FakeAgent` instance methods: `send`, `close`, `reload`, `listArtifacts`,
  `downloadArtifact`, `getUsage`, `Symbol.asyncDispose`. Local agents enforce a single
  active run (`AgentBusyError`) unless `local.force` is set.
- `Run`: streamed deltas via `onDelta`/`onStep`, `stream`, `wait`, `cancel`,
  `conversation`.
- `Cursor.configure`, `me`, `models.list`, `repositories.list`, `auth.login/logout/status`.
- `configureCursorSdk`, `getDefaultSdkStateRoot`, `getDefaultUseHttp1ForAgent`,
  `getDefaultWorkspaceScanCacheTtlMs`, `createAgentPlatform`, the
  `LOCAL_RUN_STREAM_*` and `DEFAULT_LOGIN_API_KEY_TTL_MS` constants.
- `SqliteLocalAgentStore` (`@cursor/sdk/sqlite`): `open`, `agents`, `runs`,
  `checkpoints`, `dispose`. Store directories are logged with the per-session hash
  replaced by `<scope>`.

Every SDK call is logged as `sdk_call`, every run as `run`/`run_settled`, every native
tool as `native_tool`, and every bridge request and execution as `bridge_request` and
`tool_execution`. Counters are emitted once per process as `process_counters`.

## Scenarios

| Scenario | What it exercises | Reference counts (Pi 1.3.0) |
| --- | --- | --- |
| `text-turn` | One local turn: thinking, two text deltas, turn-end usage, no tools. | `agent.send` 1, `Agent.create` 1, bridge 0 |
| `tool-turn` | One bridge MCP tool call (`pi__fake_echo`) executed exactly once, plus one native `shell` call. | `agent.send` 1, bridge 1, native `shell` 1 |
| `cloud-create-archive` | Cloud agent with repo, branch, pool env. One turn, list, archive, then delete with `--yes`. | `Agent.create` 1, `agent.send` 1, `agent.listArtifacts` 1, `Agent.archive` 1, `Agent.delete` 1 |

The exact per-scenario counts, the fake log hash, the normalized Pi JSON hash and the
determinism results are in `evidence.json`.

## Normalization

The fake log is not normalized beyond replacing the temporary root with `<root>`. The
Pi JSON stream is normalized, and every rule is listed in `evidence.json` under
`normalization`. The rules cover timestamps, durations, UUIDs, session-file times,
Pi's random 8-hex entry IDs, bridge run IDs and the per-session store hash.

## Known gaps

- No scenario yet exercises image prompts, turn-level errors (`sendError`, `error`),
  cancellation (`hang` plus `cancel`), concurrent local-agent owners, or mid-run crashes.
  The fake supports them, but these runs are not yet recorded as evidence.
- No scenario exercises `Cursor.models.list` errors (`modelsError`), `Cursor.auth`, or
  `Agent.unarchive`.
- Cloud behaviour is modelled from the SDK declarations only. Real cloud agents,
  billing, the raw REST usage fallback (`/v1/agents/{id}/usage`) and the live
  `artifacts` API are not exercised.
- Interactive/TUI mode, RPC mode and the `cursor-usage` command are not run.
- `SqliteLocalAgentStore` is a minimal in-memory model. It does not reproduce the
  on-disk layout of the real SQLite store.
- The stretch goal (running `text-turn` through the rpi host CLI and cdylib) is not
  done in this lane.
