# rpi-anthropic-auth

Work in progress: the native transport, the two owned commands and the Claude
Pro/Max auth callbacks are connected, but the package is not parity complete or
ready for release. See [PARITY.md](PARITY.md).

This package gives Pi the `anthropic` provider over Anthropic's Messages API,
speaking the Claude Code wire protocol so a Claude Pro/Max subscription can be
used the way Claude Code itself uses it: the required beta headers, the
`claude-cli/<version>` user agent, the thinking budgets the CLI sends, and the
billing header derived from the first user turn.

## What it registers

| Registration | Value |
| --- | --- |
| Provider | `anthropic` |
| API | `anthropic-messages` |
| Base URL | `https://api.anthropic.com` (`/v1/messages`) |
| Models | none — the host catalog provides the Anthropic models |
| Auth methods | `api_key` (Anthropic API key) and `oauth` (Claude Pro/Max) |
| Runtime actions | `preflight` |
| Commands | `/anthropic-auth-status`, `/anthropic-usage` |

The provider declares `models: []` on purpose: Pi's catalog already lists the
Anthropic models, and replacing it would freeze a model list inside a plugin.

## Sign-in

`/login` offers two methods.

- **Claude Pro/Max** opens the Claude authorize URL in a browser and starts the
  local callback listener on `127.0.0.1:53692`. While the browser flow is in
  progress the host is asked to poll; if the listener cannot bind, the same URL
  is shown with a field for the pasted `code#state` or callback URL.
- **Anthropic API key** takes the key directly. API keys are sent as
  `x-api-key` and do not get the Claude Code headers or the billing header.

Refresh, `toAuth`, `resolveApiKey` and `revoke` are implemented; credentials are
stored by the host, never by this package.

## Commands

`/anthropic-auth-status` prints the transport, the credential family and source,
the token length (never the token), its expiry when this session minted it, the
beta list in force, the Claude Code version pin and any learned floor, and the
provider ids the shaping applies to.

`/anthropic-usage` reads the OAuth usage and profile endpoints and renders the
5-hour, weekly and extra-usage windows. It needs an OAuth token; an API key
cannot read them.

## Environment

| Variable | Effect |
| --- | --- |
| `ANTHROPIC_API_KEY` | Used when neither the credential store nor the request carries a key. |
| `PI_ANTHROPIC_AUTH_CLAUDE_CODE_VERSION` | Pins the Claude Code version sent in the user agent and billing header instead of the built-in pin. |
| `PI_CACHE_RETENTION` | `long` requests 1-hour cache retention; the host's default retention is upgraded by it. |
| `RPI_ANTHROPIC_OAUTH_CLIENT_ID`, `RPI_ANTHROPIC_OAUTH_CLIENT_SECRET` | Override the OAuth client identity at build time or runtime. |

The Claude Code version is also *learned*: when Anthropic rejects a request with
a version floor, the transport retries once at the named version and remembers it
for later turns. `/anthropic-auth-status` shows that floor next to the pin.

## Building and testing

```sh
cargo +1.78.0 build -p rpi-anthropic-auth
RUSTFLAGS=-Dwarnings cargo +1.78.0 test -p rpi-anthropic-auth
```

The crate builds as `rlib` and `cdylib`; the cdylib is what Pi loads as a native
plugin. Every module below the C ABI is pure: `auth`, `billing`, `convert`,
`prompt`, `stream`, `transport` and `usage` take and return values, so the tests
need no host, no network and no clock.

## Licenses

This port is MIT licensed, copyright Aiden Yoon. The behavior it reproduces comes
from `pi-anthropic-oauth` by Leo Henon (MIT); the upstream notice is
[LICENSE.upstream](LICENSE.upstream) and is also recorded in the repository's
`THIRD_PARTY_NOTICES.md`.
