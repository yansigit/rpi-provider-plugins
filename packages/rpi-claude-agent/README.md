# rpi-claude-agent

A `claude-code` provider for an unmodified rpi 0.12 host that runs Claude through
Anthropic's official [Claude Agent SDK](https://code.claude.com/docs/en/agent-sdk)
(`@anthropic-ai/claude-agent-sdk`, pinned in `worker/package-lock.json`). The SDK
runs the official Claude Code runtime, authenticated with your own token from
`claude setup-token`.

It does not implement its own OAuth flow, reuse Claude Code's client id, or imitate
Claude Code's headers. The system prompt is Claude Code's own, from the SDK's
`claude_code` preset. The only credential it handles is the token you paste, which
it passes to the SDK as `CLAUDE_CODE_OAUTH_TOKEN`.

## Install

```sh
packages/rpi-claude-agent/scripts/stage.sh ~/.rpi-plugins/claude-agent
rpi --extensions-dir ~/.rpi-plugins/claude-agent --list-models claude
```

The stage holds the plugin library, the Node worker (`runtime/main.mjs`) and its
`node_modules`, including the bundled Claude Code executable at
`runtime/node_modules/@anthropic-ai/claude-agent-sdk-<platform>/claude`. Node 24 or
newer must be on `PATH` (or set `RPI_CLAUDE_AGENT_NODE`).

## Log in

```sh
OUT=~/.rpi-plugins/claude-agent
$OUT/runtime/node_modules/@anthropic-ai/claude-agent-sdk-linux-x64/claude setup-token
rpi auth login --provider claude-code    # paste the token
```

`CLAUDE_CODE_OAUTH_TOKEN` in the environment also works. Each rpi agent directory
(`RPI_CODING_AGENT_DIR`) stores its own token, and Claude Code keeps its state under
`<agent dir>/claude-code`, so separate accounts stay separate. `ANTHROPIC_API_KEY`
and `ANTHROPIC_AUTH_TOKEN` are removed from Claude Code's environment so the
subscription token is the credential that is used.

## How it works

- Claude gets Claude Code's own system prompt (`preset: "claude_code"`), with a
  one-line note that it runs in rpi appended, followed by rpi's own instructions:
  the guidelines of rpi's default prompt (where extension tools add their
  guidance), appended prompt text, project context files and the skills listing.
  The default prompt's introduction and tool list and its working-directory line
  are dropped, because Claude Code writes its own; a custom `--system-prompt` or
  `SYSTEM.md` is kept.
  The working directory from rpi's prompt is passed as the SDK's `cwd`.
- Claude Code's built-in tools are off (`tools: []`), and only rpi's tools are
  allowed (`permissionMode: "dontAsk"`, `allowedTools: ["mcp__rpi__*"]`), so
  anything else is refused without asking.
- Claude Code reads none of the user's Claude settings: no settings files
  (`settingSources: []`), hooks, skills, CLAUDE.md files, auto memory, auto
  compaction, away summaries, tool search or advisor tool. Prompts are sent
  verbatim. `CLAUDE_CODE_SIMPLE`, `CLAUDE_CODE_EFFORT_LEVEL` and `MCP_TOOL_TIMEOUT`
  are cleared from Claude Code's environment, and MCP tool output may be up to
  200k tokens so rpi's own truncation applies.
- rpi's tools for the turn are offered to Claude as an in-process MCP server named
  `rpi`. When Claude calls one, the provider stream ends with an rpi `toolCall`
  (the `mcp__rpi__` prefix stripped), rpi runs the tool under its own policy, and
  the `toolResult` on the next turn answers the pending call, so the same Agent SDK
  query continues.
- rpi's history is the source of truth; Claude Code also saves its own session
  for each rpi session (under `<agent dir>/claude-code`), so a new query can give
  Claude its earlier turns natively, thinking included, instead of as text. After
  each reply the worker
  records which point of rpi's history matches which message of Claude's session,
  in `<agent dir>/claude-code/rpi-sessions/<sha256 of the rpi session id>.json`
  (no token in it). A Claude session the record no longer refers to (after a fork
  or a fresh start) is deleted.
- One live query is kept per rpi session. It stops after 5 minutes idle; while one
  of Claude's tool calls waits for its result it is not idle, and stops only if
  the result has not come after an hour. A request the live query cannot continue
  (including one whose last reply is not the one Claude gave) starts a new one:
  - if rpi's history still contains everything Claude saw (after an idle stop, an
    rpi restart, or a model, effort, system prompt or tool set change), it resumes
    Claude's session and sends only what Claude missed;
  - after an edit, undo or compaction, or when the last turn never finished (a
    crash, or a tool call whose query was stopped), it forks Claude's session at
    the last complete reply both histories share (never between a tool call and
    its result) and sends what follows;
  - otherwise, or with a different token, it starts a new Claude session from the
    whole history.

  When what Claude missed is more than new user input (for example replies another
  model wrote), the missed turns are sent as text, without thinking, inside one
  `<conversation-checkpoint>` user message, followed by the new user input.
  Requests without an rpi session id, and requests the host marks as something
  other than a prompt (compaction, summaries, titles), are not saved, and their
  query ends with the reply. Released rpi hosts (0.12 to 0.15) do not mark
  requests, so there every request with a session id is saved.
- Text and thinking stream as deltas; thinking is adaptive with summaries.
  rpi's thinking level maps to the SDK's effort.
- Claude Code reports a response's usage several times while it streams. Each
  response counts once: output adds up across the turn's responses, and input and
  cache tokens are the last response's, which is the context in use.
- Cancel interrupts the query and the turn ends as aborted. Claude's session keeps
  its partial reply, and the next request continues that session without resending
  rpi's copy of the partial reply or the results of tool calls in it.
- A rejected rate limit or usage limit ends the turn with an error whose text starts
  with `rate_limited; resets_at=<unix seconds> ` (or `rate_limited; ` when the reset
  time is unknown), for Zephyr's account pool.

## Terms

Using a Claude subscription this way is governed by Anthropic's terms, including
the Consumer Terms and the usage policy for Claude Code. Check that unattended or
automated use fits your plan before relying on it.

## Tests

```sh
(cd packages/rpi-claude-agent/worker && npm ci --ignore-scripts)
cargo test -p rpi-claude-agent -- --ignored
```

`tests/worker.rs` and `tests/host.rs` replace the SDK with `tests/fake-sdk.mjs`
(`RPI_CLAUDE_AGENT_SDK`). `real_agent_sdk_runs_an_rpi_tool_offline` runs the real SDK
and Claude Code against `tests/fake-api.mjs` inside a network namespace with only
loopback. No test contacts Anthropic. `RPI_CLAUDE_AGENT_STAGE=<dir>` runs the host
tests against a staged directory.
