# c-tools: differential tool scenarios (ledger C05/H08)

`tests/c_tools-reference.mjs` runs the same scripted tool scenario through two
runtimes and compares exact execution counts:

- **reference** — unchanged Pi 1.3.0 + the pinned `pi-cursor-sdk@0.5.3` adapter
  (`provider-references/integrated`), driven by the unchanged fake
  `@cursor/sdk@1.0.37` scenario harness;
- **candidate** — the rpi retained worker (`runtime/main.mjs`, built from
  `worker/`) driven through its real peer protocol by a scripted rpi host, with
  the same fake SDK injected.

The fake SDK harness lives in the sibling c-fake lane
(`../rpi-package--c-fake/packages/rpi-cursor-sdk/tests/fake-sdk`) and is never
edited here. After the c-fake lane is integrated it is read from `tests/fake-sdk`.
Override with `RPI_CURSOR_FAKE_SDK`.

## Scenarios

| File | What it pins |
| --- | --- |
| `scenarios/tool-turn.json` | One Cursor native `shell` tool and one `pi__fake_echo` bridge tool: exactly **1 Cursor / 1 bridge / 0 replay**. |
| `scenarios/native-only.json` | One Cursor native `shell` tool replayed by the host: 1 Cursor, 0 bridge, 0 replay work. |
| `scenarios/bridge-only.json` | One bridge tool, no native tool: 0 Cursor, 1 bridge. |
| `scenarios/bridge-failure.json` | A bridge tool that throws: executed once, one tool-level failure, 0 replay work. |
| `scenarios/incomplete-native.json` | A native tool that never completes: one inert replay-only `cursor` card, 0 replay work. |
| `scenarios/replay-not-bridgeable.json` | A direct MCP call to the replay-only tool is refused by the bridge and surfaces an inert card. |

`fixtures/failing-extension.mjs` registers the always-failing bridge tool.

## Counts

- `nativeToolCalls` — Cursor native tool calls, from fake-log `native_tool` records.
- `bridgeToolExecutions` / `bridgeToolFailures` — MCP `tools/call` results from the bridge.
- `hostToolInvocations` — tool names the host was asked to run (reference: Pi
  `tool_execution_start`; candidate: worker `toolcall_end` events). Recorded replay
  ids are refused by the host, so they never perform work.
- `replayToolExecutions` — recorded-activity invocations that performed work. Must be 0.

## Run

```sh
source /home/user/Projects/.parity-env.sh && lane_env c-tools
cd packages/rpi-cursor-sdk/worker && npm ci --ignore-scripts --no-audit --no-fund && npm run build
cd .. && node tests/stage-runtime.mjs
node tests/c_tools-reference.mjs            # check mode
node tests/c_tools-reference.mjs --write    # regenerate c_tools-evidence.json
```

`tests/c_tools.rs` loads `c_tools-evidence.json` with `include_str!` and compares
the complete count objects, so a stale or tampered fixture fails `cargo test`.
