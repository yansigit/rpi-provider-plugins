# R03 evidence: all three providers together

Harness: [`../tools/combined`](../tools/combined/README.md). Ledger row R03
("All three together, pooling over Antigravity/Cursor").

## Result

| Gate | Result |
| --- | --- |
| Pi 1.3.0 reference combined run | **pass** — 5 positive segments + 2 pinned negative segments (7 total), `problems: []`, `normalizationErrors: []` |
| rpi candidate run (executable segments) | **pass** — 4 segments (`agy-a`, `agy-b`, `cursor-a`, `cursor-b`), `problems: []` |
| Candidate multiprovider pools + virtual provider | **blocked** — `librpi_multiprovider.so` registers nothing at base `adc459b`; candidate catalog has no `parity-virtual` |
| Pinned differential | 4/4 segments differ at the pinned X-RPC shape level; the pin matches across runs |
| Request-level agreement | Antigravity method/path/account/project identical 4/4; Cursor account digests identical 2/2 |
| `node --test tests/r03-combined.test.mjs` | 2 pass, 0 fail, 0 skipped |

Full details, hashes and commands: [`r03-combined.evidence.json`](r03-combined.evidence.json).

## What the diff contains

The comparison is positional and complete; it is **pinned**, not normalized.
`tools/combined/fixtures/expected/combined.diff.json` records every mismatched
record index. Categories:

- rpi `ready` record absent from Pi;
- system/user `message_start`/`message_end` pairs emitted only by Pi;
- assistant `message` field sets and usage differ;
- `message_update` shape differs (`usage` vs `message`/`eventType`);
- prompt `response.data` differs (`disposition:started` vs `accepted:true`);
- Pi Cursor capability `entry_appended` records (6) absent from rpi;
- extension status/notification count and order differ.

Nothing in this list is normalized away. `normalize.mjs` is a byte-identical
copy of the X-RPC normalizer; **zero new normalization rules** were added. Any
future attempt to normalize one of these differences needs an explicit
decision; it is not silently applied here.

## Upstream finding (needs a decision, not a workaround)

Pooling Cursor or serving it through a virtual provider on **Pi 1.3.0** fails:

```
Cursor provider runtime failed: Cursor provider ownership is unavailable: this call has no native session request receipt.
```

Cause: `pi-cursor-sdk@0.5.3` stores its request-ownership receipt in a `WeakMap`
keyed by the `headers` object supplied by Pi's `before_provider_headers` event.
`pi-multiprovider@0.11.3`'s `lift.ts` (`applyResolvedAuth`) and `src/virtual.ts`
call the backing provider's `streamSimple` with a newly merged headers object,
so the receipt is missing. This is upstream behavior; the lane records it with
two negative segments (`cursor-pooled`, `virtual-composer`) that assert the
error text and zero SDK calls. Fixing it would require an upstream
multiprovider or cursor-extension change (preserve the headers object identity)
— outside this lane.

## Reproduction

```sh
source /home/user/Projects/.parity-env.sh && lane_env r03 && export CARGO_BUILD_JOBS=4
cargo build -p rpi-antigravity-auth -p rpi-cursor-sdk -p rpi-multiprovider --locked
node tools/combined/stage-rpi.mjs --out "$HOME/r03stage" \
  --antigravity "$CARGO_TARGET_DIR/debug/librpi_antigravity_auth.so" \
  --cursor "$CARGO_TARGET_DIR/debug/librpi_cursor_sdk.so" \
  --multiprovider "$CARGO_TARGET_DIR/debug/librpi_multiprovider.so" \
  --runtime packages/rpi-cursor-sdk/runtime
node tools/combined/run.mjs --side pi --work "$(mktemp -d "$HOME/.r03-XXXXXX")"
node tools/combined/run.mjs --rpi /home/user/Projects/.targets/x-rpc/debug/rpi \
  --rpi-antigravity "$HOME/r03stage/librpi_antigravity_auth.so" \
  --rpi-cursor "$HOME/r03stage/librpi_cursor_sdk.so" \
  --rpi-multiprovider "$HOME/r03stage/librpi_multiprovider.so" \
  --work "$(mktemp -d "$HOME/.r03-XXXXXX")"
```

The Cursor runtime in the command is the packaged worker build
(`runtime/main.mjs` + `runtime/dist` + `runtime/node_modules`). It is not
tracked; rebuild/stage it as R01-PKG documents.

## Not tested

- Antigravity request-body parity (recorded, not compared).
- Cursor tools/images/cloud/cancel/retry/compaction.
- rpi pool and virtual-provider inference (blocked as above).
- Live OAuth refresh, project onboarding, vault custody.
- macOS/Windows.
- The candidate libs were built with the default 1.95 toolchain; the packages'
  1.78 gates are covered by their own lanes.
