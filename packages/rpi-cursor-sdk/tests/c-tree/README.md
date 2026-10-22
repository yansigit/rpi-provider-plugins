# C04 branch-continuity parity (Cursor session/resume/store)

Offline differential harness for the retained rpi Cursor worker's session,
resume, lineage, store and cleanup modules over the host's awaited session ports.
It uses a scenario-driven fake of `@cursor/sdk@1.0.37` (copied from the c-fake
lane, with local agent ids corrected to the real `agent-<uuid>` shape that the
adapter's `isCursorLocalAgentId` requires), so no Cursor network call is made.

Two runners drive the *same* scenarios:

| Runner | What it drives | Output |
| --- | --- | --- |
| `run-reference.mjs` | The unchanged Pi 1.3.0 CLI (`provider-references/integrated`) with the pinned `pi-cursor-sdk@0.5.3` adapter, in `--mode json`. | `reference-evidence.json` |
| `run-rpi.mjs` | The retained rpi worker (`runtime/main.mjs`) through its private peer protocol with an emulated native host whose `sessions` port acknowledges every commit authoritatively. | `evidence.json` |

Each runner runs every scenario twice and requires byte-identical normalized
counts and C04 entries across both runs. `run-rpi.mjs` also compares its C04
entries and observable SDK counters against the committed Pi reference for every
scenario that has one.

`../c_tree.rs` loads both fixtures with `include_str!`, compares the whole C04
entry objects and counters, checks the rpi-only contract expectations, and
re-hashes every recorded scenario and script, so it runs under `cargo test`
without Node.

## Usage

```sh
cd packages/rpi-cursor-sdk
source /home/user/Projects/.parity-env.sh && lane_env c-tree
export CARGO_BUILD_JOBS=4 CURSOR_API_KEY=offline-fixture

# Stage the packaged worker and its runtime dependencies once:
(cd worker && npm ci --ignore-scripts --no-audit --no-fund && node scripts/build.mjs)
node tests/stage-runtime.mjs
(cd runtime && npm ci --ignore-scripts --no-audit --no-fund)

node tests/c-tree/run-reference.mjs            # check mode
node tests/c-tree/run-reference.mjs --write    # regenerate reference-evidence.json
node tests/c-tree/run-rpi.mjs                  # check mode (needs the reference fixture)
node tests/c-tree/run-rpi.mjs --write          # regenerate evidence.json
```

`--only NAME`, `--keep DIR` and `RPI_C_TREE_DEBUG=1` aid debugging. Every path
(HOME, TMPDIR, fake state, sessions, agent dir) lives in a temporary directory
under `$HOME`; `~/.pi` and `~/.rpi` are never touched. `CURSOR_API_KEY` is only a
placeholder checked by the fake.

## Scenarios

| Scenario | Reference | Exercises |
| --- | --- | --- |
| `continuity-turn` | yes | One local turn: forensic `cursor-sdk-agent-lineage` at the send boundary and a `cursor-sdk-agent-resume` handle flushed at `turn_end`. |
| `continuity-reuse` | yes | Two turns: lineage is deduplicated for the reused agent and each `turn_end` flushes an updated resume handle (`incrementalSendCount`). |
| `cleanup-dry-run` | yes | A turn then `/cursor-local-resume-cleanup`: the dry-run plan protects exactly the latest agent and records no candidates. |
| `cleanup-empty` | yes | The cleanup command with no recorded agents: an empty dry-run plan with no SDK access. |
| `compaction-summary` | rpi-only | `session_before_compact` releases the pooled conversation run/agent, a separate temporary summary agent runs with `tools: []` and an ephemeral store, `session_compact` resets resume state, and the next turn resumes the recorded handle. |
| `cleanup-durability` | rpi-only | `--yes` with a stale candidate: a durable intent (`append`) precedes any deletion; the result is a `journalAppend` after an intervening branch move, does not advance the new branch, and records the exact candidate/protected/failed ids. |
| `cleanup-nondurable` | rpi-only | The same flow when the result append is only `ephemeral`: the result is retained, the durable intent remains the retry barrier, and the worker reports the partial ledger. |

## Emulated host

`run-rpi.mjs` answers the worker's private native ports (`context`, `tools`,
`auth`, `ui`, `dialogs`, `providers`, `sessions`, `journals`) with one in-memory
session whose projection mirrors the conversation it sends. The `sessions` port
returns committed receipts, verifies them against a fresh authoritative view, and
records `append` vs `journalAppend`, `advancedLane` and durability. The
`cleanup-durability`/`cleanup-nondurable` scenarios seed a prior resume entry
(the same session/scope identity the worker appended) carrying a stale
`cleanupCandidateAgentIds` value, then simulate a branch move between the durable
intent and its result.

## Normalization

The reference runner normalizes the Pi JSON event stream (timestamps, UUIDs,
session-file times, Pi's random 8-hex entry ids, replay/bridge run ids, store
hashes, the temporary root). Both runners normalize C04 entry data identically
(see `normalize.mjs`):

- `sessionId`, `sessionFile`, `scopeKey`, `cwd`, timestamps, `branchPathHash`,
  `storeIdentity.stateRoot` and the environment components of `poolKey` are
  replaced with placeholders.
- The whole `sendState.contextFingerprint` is replaced because Pi carries its
  system prompt as a leading `system` session message while the native port
  carries it in `context.systemPrompt`; the semantic outcome is still compared
  through `bootstrapped`, `incrementalSendCount` and `compactionGeneration`.
- `agentId`, cleanup `action`/`phase` and every `candidateAgentIds`,
  `protectedAgentIds`, `deletedAgentIds`, `failedAgentIds` value are compared
  exactly.

## Known gaps

- Pi's built-in `/compact` command is not dispatched in `--print`/`--mode json`,
  so compaction is exercised rpi-only through scripted host events. The summary
  request text is not compared; only the observable agent/store/counter behavior
  and the resume-entry sequence are.
- The rpi runner drives the retained worker directly, not through the rpi host
  CLI/cdylib. End-to-end host wiring (including inherited-origin journal access)
  remains required and is not claimed here.
- Interactive/TUI and RPC modes, images, cloud agents, tools/MCP execution and
  the `cursor-usage` command are outside C04.
