# C-USAGE: usage/billing and lifecycle differentials

Offline differentials for ledger rows **C07** (usage/billing refresh, export and
interrupted recovery) and **C09** (flush, recovery records and stop admission at
the durable boundary).

They drive the **unchanged pinned upstream** modules from
`provider-references/integrated/node_modules/pi-cursor-sdk/dist` with the real
Pi 1.3.0 `SessionManager`, an injected clock/randomness/IDs and a fake
`@cursor/sdk` usage endpoint. No Cursor network calls and no repository writes
outside temporary directories under `$HOME`.

The fake SDK extends the `tests/fake-sdk/` (C-FAKE) pattern with the only
endpoint the usage modules use: `Agent.getUsage(agentId, options)` and the
instance `agent.getUsage()`.

## Files

| File | Purpose |
| --- | --- |
| `usage-reference.mjs` | Check/`--write` driver for the C07 fixture. Runs the child twice and requires byte-identical output. |
| `scenarios.mjs` | 20 C07 scenarios transcribing the pinned `test/cursor-usage-ledger.test.ts` cases plus fake-SDK cases. |
| `lifecycle-reference.mjs` | Check/`--write` driver for the C09 crashed-writer flush/recovery fixture. |
| `harness.mjs` | Minimal extension harness around the unchanged upstream modules and Pi's real `SessionManager`. |
| `sdk.mjs` | Fake `@cursor/sdk` usage/billing endpoint. |
| `hooks.mjs`, `register.mjs` | Resolve hook that redirects `@cursor/sdk` only while a scripted answer is set. |
| `inject.cjs` | Deterministic `Date` and `node:crypto` IDs via a CJS preload. |
| `usage-fixture.json`, `evidence.json` | Committed C07 fixture and provenance/evidence. |
| `lifecycle-fixture.json`, `lifecycle-evidence.json` | Committed C09 crash/recovery fixture and evidence. |

## Usage

```sh
cd packages/rpi-cursor-sdk
source /home/user/Projects/.parity-env.sh && lane_env c-usage
node tests/c-usage/usage-reference.mjs            # check mode
node tests/c-usage/usage-reference.mjs --write    # regenerate
node tests/c-usage/usage-reference.mjs --only raw-cache-correction-revised-bills
node tests/c-usage/lifecycle-reference.mjs
node tests/c-usage/lifecycle-reference.mjs --write
```

`RPI_CURSOR_REFERENCE` overrides the integrated reference root. Node 24 is
required.

## Rust comparison

`tests/c_usage_reference.rs` embeds both fixtures with `include_str!` and is pure
Rust, so it runs in the ordinary `cargo test -p rpi-cursor-sdk` gate:

- every recorded usage-journal frame sequence is replayed through the native
  `JournalPort` with digest chaining and compared **byte-for-byte**, including
  port read-back and digest;
- every crashed-writer prefix is replayed and the `committed`/`threw` outcome and
  complete bytes are compared (usage refuses a torn tail, cloud repairs it);
- `close()` stops admission, releases abandoned readers and leaves committed
  frames byte-identical.

## Normalization

Only the per-run temporary root is replaced by `<root>`; every string and every
recorded byte goes through the same replacement. No semantic normalization is
applied. The injected clock and IDs are recorded in `evidence.json`.

## Known limits

- No live Cursor billing, cloud agent or `/v1/agents/{id}/usage` REST fallback.
- Interactive/TUI/RPC rendering is not exercised; only the non-UI notify/export
  paths are recorded.
- The native worker's own usage ledger is verified at the `JournalPort` boundary,
  not end-to-end through the installed DLL.
- Process-level stop admission, cancellation, MCP/SDK close, worker reaping and
  reload/shutdown/crash of the installed library are Rust-owner behaviour with no
  upstream TypeScript counterpart. They remain covered by `tests/worker.rs`
  (crash partial preservation, admitted host drain, one-shot cancellation,
  capacity) and `tests/shutdown-evidence.json`, not re-run here.
