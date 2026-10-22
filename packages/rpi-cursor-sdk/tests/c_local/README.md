# Local Cursor agent-path parity (ledger C03)

Offline differential harness for the retained rpi Cursor worker's local agent
paths: session-agent create/reuse/resume, prompts, settings, custom agents,
HTTP controls, SDK pool ownership, cold start, cancellation and concurrent
owners. It uses the scenario-driven fake of `@cursor/sdk@1.0.37` copied from the
c-fake lane (`fake-sdk/`), so no Cursor network call is ever made.

Two runners drive the *same* scenarios:

| Runner | What it drives | Output |
| --- | --- | --- |
| `run-reference.mjs` | The unchanged Pi 1.3.0 CLI (`provider-references/integrated`) with the pinned `pi-cursor-sdk@0.5.3` extension, in `--mode json`. | `reference-evidence.json` |
| `run-rpi.mjs` | The retained rpi worker (`runtime/main.mjs`) through its private peer protocol with an emulated native host. | `evidence.json` |

Each runner runs every scenario twice and requires byte-identical fake-SDK
counters across both runs. `run-rpi.mjs` also compares its observable SDK
interactions against the committed Pi reference for every scenario that has one.

`../c_local.rs` loads both fixtures with `include_str!`, compares the whole
observable interaction objects and re-hashes every recorded scenario and script,
so it runs under `cargo test` without Node.

## Usage

```sh
cd packages/rpi-cursor-sdk
source /home/user/Projects/.parity-env.sh && lane_env c-local
export CARGO_BUILD_JOBS=4 CURSOR_API_KEY=offline-fixture

# Stage the packaged worker once (writes gitignored runtime/dist):
(cd worker && node scripts/build.mjs) && node tests/stage-runtime.mjs

node tests/c_local/run-reference.mjs            # check mode
node tests/c_local/run-reference.mjs --write    # regenerate reference-evidence.json
node tests/c_local/run-rpi.mjs                  # check mode (needs the reference fixture)
node tests/c_local/run-rpi.mjs --write          # regenerate evidence.json
```

`--only NAME`, `--keep DIR` and `RPI_C_LOCAL_DEBUG=1` aid debugging. Every path
(HOME, TMPDIR, fake state, sessions, agent dir) lives in a temporary directory
under `$HOME`; `~/.pi` and `~/.rpi` are never touched. `CURSOR_API_KEY` is only a
placeholder checked by the fake.

## Scenarios

| Scenario | Exercises | Reference compared |
| --- | --- | --- |
| `local-create` | Cold start + one local turn: thinking, text, usage. | yes |
| `local-reuse` | Two turns in one session; the second reuses the pooled SDK agent and sends only appended input. | yes |
| `local-settings` | `PI_CURSOR_HTTP_1_1=1` transport control (`Cursor.configure`) and its pool-key participation. | yes |
| `local-custom-agents` | User-configured custom subagents passed to `Agent.create`. | yes |
| `local-cancel` | Hung turn cancelled through the worker's private cancel frame; the SDK run is cancelled and the agent disposed. | no (rpi-only) |
| `local-concurrent` | Two overlapping `provider.stream` callers; the per-scope turn queue serializes them and the second reuses the agent. | no (rpi-only) |

The unchanged Pi 1.3.0 print CLI cannot deterministically cancel a turn or issue
two overlapping turns, so those two scenarios are rpi-only contract checks; their
expected counts are recorded in the scenario `expect` blocks.

## Emulated host

`run-rpi.mjs` answers the worker's private native ports (`context`, `tools`,
`auth`, `ui`, `dialogs`, `providers`, `sessions`, `journals`) with a single
in-memory session whose projection mirrors the conversation it sends, and a
durable usage journal under the session file. Session origins are released and
verified before the worker is closed.

## Known gaps

- Image prompts are not covered. Pi attaches `@image.png` references; matching
  that text/attachment shape between the two entry paths was not attempted.
- Cloud agents, tools/bridge execution, MCP, and the `cursor-usage` command are
  out of scope for C03 and covered (or not) by their own lanes.
- The rpi runner drives the retained worker directly, not through the rpi host
  CLI/cdylib. End-to-end host execution needs the host ports listed in the lane
  report; it is not claimed here.
- `local-cancel` records the SDK-level cancellation and the worker's stream
  reply, not a full ordered abort trace.
