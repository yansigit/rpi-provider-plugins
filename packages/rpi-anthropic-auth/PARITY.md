# Anthropic parity ledger

Primary reference: `leohenon/pi-anthropic-oauth` 0.3.6
(`github.com/leohenon/pi-anthropic-oauth`, commit
`3709dcb7bce5de10670f3a8912c4e14327f93594`, MIT). Secondary reference for the
status command and per-provider shaping: `@gotgenes/pi-anthropic-auth` 3.4.3
(`gotgenes/pi-anthropic-auth`, MIT), specifically `src/extra-provider-shaping.ts`
and `src/diagnostics.ts`.

Provider identity is `anthropic`, API `anthropic-messages`. Source behavior takes
precedence over README text. The native transport, the auth callbacks and both
commands are connected; the package is not parity complete or ready for release.

## Ported behavior

| Row | Upstream source | Rust port | Acceptance | Status |
| --- | --- | --- | --- | --- |
| P01 | `index.ts` provider registration | `src/native.rs`, `src/transport.rs` | declaration/registration shape, header fingerprint and payload tests | 18 transport cases pass on Rust 1.78: OAuth and API-key header sets, merged/removed overrides, `max_tokens`, thinking, billing header. Loaded-host registration pending |
| P02 | `auth.ts` PKCE login, token exchange, refresh | `src/auth.rs`, `src/auth/callback.rs`, `transport::Http` as the `Port` | `tests/auth.rs` (22), `tests/callback.rs` (6) | Scripted exchange/refresh/callback sequences pass; live sign-in pending |
| P03 | `convert.ts` history and tool conversion | `src/convert.rs` | `tests/message_conversion.rs` (29) | Pass |
| P04 | `prompt.ts` Claude Code system-prompt shaping | `src/prompt.rs` | `tests/system_prompt_shaping.rs` (24) | Pass |
| P05 | `stream.ts` request build and SSE loop | `src/transport.rs`, `src/stream.rs` | `tests/transport.rs` (18), `tests/stream.rs` (13) | Text, thinking (incl. redacted), tool-fragment, usage/cost, stop-reason, truncation, HTTP-error, cancellation and version-retry cases pass against a replay source; live API turn pending |
| P06 | `usage.ts` `/usage` report | `src/usage.rs`, `src/commands.rs` | `tests/usage.rs` (7) | Percent bars, reset countdowns, plans, limit/legacy/extra rows pass; live endpoints pending |
| P07 | `version.ts` plus the billing header | `src/version.rs`, `src/billing.rs` | `tests/version.rs` (35), `tests/billing.rs` (44) | Pass |
| P08 | `extra-provider-shaping.ts`, `diagnostics.ts` | `src/commands.rs`, `src/native.rs` | `tests/commands.rs` (8) | `^anthropic(-\d+)?$` matching, credential/expiry/beta/version/shaped-provider report pass; loaded command invocation pending |

`models: []` is deliberate: the host catalog owns the Anthropic model list, and
the plugin replaces nothing.

## Not yet accepted

- Loaded-host acceptance: registration against a real `rpi` host, native command
  invocation through the slash-command and RPC paths, auth through the host's
  auth port, and a streamed turn through the host decoder.
- Live acceptance against `api.anthropic.com` for a Pro/Max account and for an
  API key, including usage endpoints.
- Differential fixtures generated from the upstream TypeScript for `stream.ts`;
  the Rust tests assert the documented behavior rather than replayed upstream
  bytes.
- Platform execution beyond Linux, and the release workflow's client-secret
  gating for OAuth builds.
- Upstream's `ensureClaudeCodeSymlink` (`~/.Claude Code` → `~/.pi`) is not ported:
  it rewrites the user's home directory, and Pi does not read that path.

## Agreed differences

- `max_tokens` defaults to `floor(model.maxTokens / 3)` exactly as upstream; the
  legacy thinking budget is then clamped to `max_tokens - 1`.
- The Claude Code version is pinned (`2.1.280`) rather than read from an
  installed CLI, and a version rejection is retried once at the named floor
  before the hint is reported.
- Cache retention: the host's default retention is treated as "short" so
  `PI_CACHE_RETENTION=long` can upgrade it; explicit `Long`/`None` from the
  caller wins.
