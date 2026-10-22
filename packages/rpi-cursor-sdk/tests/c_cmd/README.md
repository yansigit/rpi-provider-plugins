# C08 — commands, flags, config precedence, trust, provenance

Differential acceptance for ledger row C08:

> All commands/flags/config precedence/trust/limitations, stable reconstructed
> header/signal identities — Cursor index/settings/provenance; host facade —
> command/receipt/operation identity comparisons.

## Method

`c_cmd-reference.mjs` imports two module graphs **in the same Node process**:

* **reference** — the unchanged pinned `pi-cursor-sdk@0.5.3` sources under
  `provider-references/cursor`. Every imported upstream file's sha256 is asserted
  before the import (the hashes are also pinned in
  `pi-rust/docs/provider-parity/file-mapping.json`). A mismatch aborts.
* **candidate** — the retained rpi worker port under `worker/src` (the C08
  `portFiles` in the ledger mapping).

The same scripted inputs run through both graphs. For each case the script records
the **complete** reference value and the complete candidate value; the Rust test
`tests/c_cmd.rs` asserts byte-for-byte JSON equality of every pair. A second compute
per graph must equal the first (determinism), and the fixture is regenerated in
check mode and compared byte-for-byte.

TypeScript sources are loaded with `--experimental-transform-types` plus a small
resolve hook (`tests/c_cmd/resolve.mjs`) that retries failed `./x.js` specifiers as
`./x.ts`. The hook only changes module resolution; it never rewrites source text.

`worker/src/cursor-config.ts` differs from upstream only by importing
`getConfigDirName()` instead of the `CONFIG_DIR_NAME` constant; the differential
proves the resolved values are unchanged.

Both graphs resolve the pinned adapter's own dependency set — `@earendil-works/pi-*`
1.1.0 and `@cursor/sdk` 1.0.37 — not the separate integrated Pi 1.3.0 harness. The
script reads the installed versions, asserts both graphs agree and records them in
the evidence.

## Coverage (391 cases)

| Group | Cases | What is compared |
| --- | --- | --- |
| `config-precedence` | 41 | `resolveCursorSdkConfig` over cli/env/session/project/user/builtin layers: per-field source order, trust levels, `safer-source` caps, field-specific omissions (cloud skips project; force skips project+session; HTTP/1.1 skips project), env-name allowlist filtering, subagents, builtin cloud merge, invalid CLI errors |
| `config-parse` | 8 | `parseCursorSdkConfig`: unknown/typed fields, trimming, env-name sanitisation, subagent policy, sandbox scalar/object |
| `config-env` | 9 | `cursorSdkConfigFromEnv`: every `PI_CURSOR_*` variable, invalid values, comma lists, `CURSOR_`-prefix refusal |
| `config-merge` | 4 | `mergeCursorSdkConfigForUpdate` nested cloud/local/sandboxOptions preservation; `cursorFastDefaultsFromConfig` |
| `config-trust` | 12 | `loadCursorSdkUserConfig`/`loadCursorSdkProjectConfig`/`loadCursorSdkConfig`: missing, malformed, non-object, trusted vs untrusted projects; `updateCursorSdkConfig` atomic replace, `0o600` new-file mode, existing-mode preservation, invalid-JSON refusal |
| `config-guards` | 22 | `isCursorRuntime`, `isCursorCloudContextHandoff`, `isCursorCloudEnvironmentType`, `parseExplicitCursorCloudEnvNames` |
| `config-paths` | 4 | user/project config paths, `cursor-sdk.json` file name |
| `config-constants` | 1 | every exported `CURSOR_*`/`PI_*` environment name (the aliases flags/config accept) |
| `store-root` | 5 | `resolveCursorLocalStoreRoot`: env/user precedence and blank handling |
| `fast-default` | 216 | exhaustive `resolveCursorFastDefault` matrix (cli force-fast/no-fast, alias, session, user, model default) |
| `setting-sources` | 27 | `resolveCursorSettingSources`/`serialize`/`includes`/default/effective over all accepted and refused tokens |
| `env-boolean` | 21 | `parseOptionalEnvBoolean`/`parseEnvBoolean` accepted, refused and unknown values |
| `api-key` | 11 | `normalizeCursorApiKey`/`resolveCursorApiKey` placeholders vs real keys vs ambient `CURSOR_API_KEY` |
| `provenance` | 9 | `captureCursorRequestProjection` digest, post-compaction measurements and native source roles; `resolveCursorRequestProvenance` purpose gating and occupancy-floor selection |
| `extension-identity` | 1 | full registration identity of the extension entry point: 10 commands (name+description), 21 flags (name+description+type+default), 17 event-handler names, provider declaration (`cursor`/`Cursor`/`https://cursor.com`/`Cursor API key`), factory-time tool names |

The extension-identity case runs `src/index.ts` against a scripted Pi extension API.
That is the identity the native host facade consumes (`register_native_command`,
`register_flag`, `declare`, `register_provider_auth`).

## Regenerate / check

```sh
source /home/user/Projects/.parity-env.sh && lane_env c-cmd
cd packages/rpi-cursor-sdk

# check mode (default; exits non-zero on any difference)
node --experimental-transform-types --import ./tests/c_cmd/register.mjs \
  tests/c_cmd-reference.mjs

# regenerate the committed fixture
node --experimental-transform-types --import ./tests/c_cmd/register.mjs \
  tests/c_cmd-reference.mjs --write
```

The script needs `worker/node_modules` (gitignored; install with `npm ci` in
`worker/`) and the pinned reference at `provider-references/cursor` (override with
`RPI_PROVIDER_REFERENCES` or `RPI_CURSOR_REFERENCE`). It sets `HOME`,
`PI_CODING_AGENT_DIR`, `TMPDIR` and `CURSOR_API_KEY` to a private temporary
directory and removes it afterwards; it never reads the real home, creates no
network connections and touches no credentials.

## Rust acceptance

```sh
RUSTFLAGS=-Dwarnings cargo +1.78.0 test -p rpi-cursor-sdk --test c_cmd --locked
cargo test -p rpi-cursor-sdk --test c_cmd --locked
```

## Retained upstream tests

The pinned adapter tests for this row also pass unchanged in the worker
(`npx vitest run` in `worker/`, Node 24.22.0, `TERM=xterm-256color`,
`CURSOR_API_KEY=offline-fixture`, private `TMPDIR`):

* `test/cursor-config.test.ts`
* `test/cursor-setting-sources.test.ts`
* `test/cursor-state.test.ts`
* `test/cursor-runtime-state.test.ts`
* `test/index-registration.test.ts`
* `test/cursor-project-trust-contract-{1,2,3,4}-*.test.ts`
* `test/project-trust-contract-fixture.test.ts`
* `test/cursor-fast-persistence.test.ts`
* `test/cursor-http1-state.test.ts`
* `test/cursor-configured-cost-persistence.test.ts`

13 files, 198 tests, all passing.

## Not tested (explicit)

* Interactive command handlers, selectors and notifications: only registration
  identity is compared, not dialog transcripts.
* Receipt/operation identity across a **live** stream: the private
  `before_provider_headers` receipt (`event.headers` object identity) and the
  per-`AbortSignal` operation purpose are exercised indirectly through
  `resolveCursorRequestProvenance`, not by executing a real stream. That wiring
  belongs to the stream-boundary lanes.
* Config lock acquisition timeout / concurrent writers (covered by the store
  lanes).
* Native library loading, plugin ABI flags and the packaged worker (install/host
  lanes).
* Live Cursor SDK inference, cloud execution, real credentials.
* macOS/Windows execution.
