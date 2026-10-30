# rpi-claude-agent

A `claude-code` provider for an unmodified rpi 0.12 host that runs Claude through
Anthropic's official [Claude Agent SDK](https://code.claude.com/docs/en/agent-sdk)
(`@anthropic-ai/claude-agent-sdk`, pinned in `worker/package-lock.json`). The SDK
runs the official Claude Code runtime, authenticated with your own token from
`claude setup-token`.

It does not implement its own OAuth flow, reuse Claude Code's client id, or imitate
Claude Code's headers or system prompt. The only credential it handles is the token
you paste, which it passes to the SDK as `CLAUDE_CODE_OAUTH_TOKEN`.

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

- Claude Code's built-in tools are off (`tools: []`), no settings files are read
  (`settingSources: []`), and the permission handler denies anything that is not
  one of rpi's tools.
- rpi's tools for the turn are offered to Claude as an in-process MCP server named
  `rpi`. When Claude calls one, the provider stream ends with an rpi `toolCall`
  (the `mcp__rpi__` prefix stripped), rpi runs the tool under its own policy, and
  the `toolResult` on the next turn answers the pending call, so the same Agent SDK
  query continues.
- One live query is kept per rpi session. If the conversation no longer matches it
  (another session, edited or compacted history, a different model, tool set or
  system prompt, a restart), the old query is closed and a new one starts from the
  transcript rendered as text.
- Text and thinking stream as deltas. rpi's thinking level maps to the SDK's effort.
- Cancel interrupts the query and the turn ends as aborted.
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
