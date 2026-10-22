# Fake `@cursor/sdk` for the C04 offline parity runs

A scenario-driven stand-in for `@cursor/sdk@1.0.37`, copied from the c-fake lane
(`tests/fake-sdk/`) and used by the C04 harness (`tests/c-tree/`). It lets the
unchanged Pi 1.3.0 reference (with the pinned `pi-cursor-sdk@0.5.3` extension) and
rpi's retained Cursor worker run the same scripted turns with no Cursor network
access. The only outbound HTTP it issues is the MCP JSON-RPC call to a bridge URL
that the adapter itself supplies in `mcpServers`.

The one behavioral correction for this lane is local agent ids: the pinned adapter's
`isCursorLocalAgentId` requires `agent-<id>`, which the real SDK returns, while the
c-fake copy used a bare UUID. Local `Agent.create` therefore returns
`agent-<fakeUuid>`, so lineage/resume entries are actually recorded.

## Files

| File | Purpose |
| --- | --- |
| `sdk.mjs` | The fake SDK. Exports the runtime surface the adapter imports. |
| `sqlite.mjs` | Fake for the `@cursor/sdk/sqlite` subpath (`SqliteLocalAgentStore`). |
| `hooks.mjs` | Node resolve hook. Redirects `@cursor/sdk` and `@cursor/sdk/sqlite`. Inert unless `RPI_CURSOR_FAKE_SCENARIO` is set. |
| `register.mjs` | `--import` entry that registers `hooks.mjs`. |
| `fixtures/echo-extension.mjs` | Pi extension that adds a `fake_echo` tool. |
| `..` | Scenario files and the two runners live in `tests/c-tree/`. |

## Usage

Inject the fake into any Node process that loads the Cursor SDK:

```sh
NODE_OPTIONS="--import /abs/path/tests/c-tree/fake-sdk/register.mjs" \
RPI_CURSOR_FAKE_SCENARIO=/abs/path/tests/c-tree/scenarios/continuity-turn.json \
RPI_CURSOR_FAKE_LOG=/tmp/opencode/fake.jsonl \
RPI_CURSOR_FAKE_LOG_ROOT=/tmp/opencode \
  node <program>
```

Environment variables:

- `RPI_CURSOR_FAKE_SCENARIO` (required to activate the fake). Path to a scenario file.
- `RPI_CURSOR_FAKE_LOG` (optional). Append-only JSONL log of every SDK call, run, delta, bridge request, tool execution and a final `process_counters` record.
- `RPI_CURSOR_FAKE_LOG_ROOT` (optional). Prefix replaced by `<root>` in the log.
- `RPI_CURSOR_FAKE_HOME` (optional). Persistent fake state directory. Without it, state lives only in the process.

Secrets are never logged: `apiKey` appears as `"<redacted>"`. See
`../README.md` for the reference/candidate runners and check modes.

## Scenario format

```jsonc
{
  "schema": "rpi-cursor-fake-sdk/1",
  "name": "continuity-turn",
  "run": { "model": "composer-2", "prompt": "...", "extensions": [], "args": [], "followUps": [] },
  "models": [ /* ModelListItem values for Cursor.models.list */ ],
  "turns": [
    {
      "name": "...",
      "expect": { "promptIncludes": "...", "mode": "agent", "modelId": "..." },
      "steps": [ /* ops below */ ],
      "outcome": { "status": "finished", "result": "..." }
    }
  ],
  "expect": { "exitCode": 0, "sdkCalls": {}, "runs": {}, "bridgeToolExecutions": 0, "toolExecutions": {}, "nativeToolCalls": {}, "violations": 0 }
}
```

Turns are consumed in order, one per `agent.send`. A turn's `expect` block is
checked when the send starts; a mismatch fails the run with
`fake_expectation_failed`. C04 scenarios additionally use `rpiOnly`, `rpi.steps`
(`turn`, `command`, `event`, `summary`, `identity`, `seedCleanupCandidate`),
`rpi.faults` (`lateBranch`, `ephemeralResult`, `appendFailure`, `releaseFailure`)
and the recorded `rpi.expectEntries`/`expectAppendDetails`/`expectNotifications`.

Step ops: `thinking`, `text`, `nativeTool`, `mcpCall`, `usage`, `delay`, `hang`,
`error`.

## Surface covered

Mirrors the declarations in `@cursor/sdk@1.0.37` for the members the adapter uses:
errors, `Agent.create`/`resume`/`prompt`/`list`/`listRuns`/`getRun`/`cancelRun`/
`get`/`archive`/`unarchive`/`delete`/`getUsage`/`messages.list`, `FakeAgent`
instance methods (`send`, `close`, `reload`, artifacts, `getUsage`,
`Symbol.asyncDispose`), `Run`, `Cursor.configure`/`me`/`models.list`/
`repositories.list`/`auth.*`, `configureCursorSdk`, `createAgentPlatform`,
constants, and `SqliteLocalAgentStore`.

Every SDK call is logged as `sdk_call`, every run as `run`/`run_settled`, every
native tool as `native_tool`, and every bridge request/execution as
`bridge_request`/`tool_execution`. Counters are emitted once per process as
`process_counters`.

## Known gaps

- `SqliteLocalAgentStore` is a minimal in-memory model; it does not reproduce the
  real on-disk SQLite layout.
- Cloud agents, real billing, live artifacts, images and the raw REST usage
  fallback are modelled from declarations only and are outside C04.
