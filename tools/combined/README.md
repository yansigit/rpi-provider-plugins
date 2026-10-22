# combined: three-provider inference differential run (R03)

Runs one scripted combined session through **unchanged Pi 1.3.0** with the three
pinned upstream extensions and through **rpi** with the three ported cdylibs,
then compares the normalized RPC transcripts. The scripted Antigravity upstream
is a loopback HTTP CONNECT proxy that terminates TLS with a disposable
certificate for `cloudcode-pa.googleapis.com`; Cursor runs on the vendored fake
`@cursor/sdk@1.0.37`. No real credential, `~/.pi`, `~/.rpi`, or external network
is used.

## Segments

One fresh child process per segment. Transcripts are compared per segment, so a
segment boundary is never part of a comparison.

| Segment | Side(s) | Provider/model | What it verifies |
| --- | --- | --- | --- |
| `agy-a` | Pi + rpi | `google/antigravity-gemini-3.8-flash` | Antigravity inference; pool account `agy-a` |
| `agy-b` | Pi + rpi | same | pool account `agy-b` (pool order reversed) |
| `cursor-a` | Pi + rpi | `cursor/composer-2` | Cursor inference on the fake SDK; account `cursor-a` |
| `cursor-b` | Pi + rpi | same | account `cursor-b` |
| `virtual-gemini` | Pi | `parity-virtual/virtual-gemini` | virtual provider model backed by Antigravity |
| `virtual-composer` | Pi (negative) | `parity-virtual/virtual-composer` | must fail with the Cursor ownership error, zero SDK calls |
| `cursor-pooled` | Pi (negative) | `cursor/composer-2` with a Cursor pool | must fail with the Cursor ownership error, zero SDK calls |

The reference side loads all three extensions in every segment. The candidate
side loads `librpi_antigravity_auth.so`, `librpi_cursor_sdk.so`, and, when
given, `librpi_multiprovider.so`.

## Usage

```sh
# Reference only (Pi 1.3.0, six positive/negative segments).
node tools/combined/run.mjs --side pi --work "$(mktemp -d "$HOME/.r03-XXXXXX")"

# Stage the candidate libraries. The Cursor cdylib resolves runtime/main.mjs
# beside its own loaded path, so the .so must be a real copy.
node tools/combined/stage-rpi.mjs --out "$HOME/r03stage" \
  --antigravity <librpi_antigravity_auth.so> \
  --cursor <librpi_cursor_sdk.so> \
  --multiprovider <librpi_multiprovider.so> \
  --runtime <rpi-package>/packages/rpi-cursor-sdk/runtime

# Full differential run. Refuses to pass unless the pinned diff matches.
node tools/combined/run.mjs \
  --rpi <rpi binary> \
  --rpi-antigravity "$HOME/r03stage/librpi_antigravity_auth.so" \
  --rpi-cursor "$HOME/r03stage/librpi_cursor_sdk.so" \
  --rpi-multiprovider "$HOME/r03stage/librpi_multiprovider.so" \
  --work "$(mktemp -d "$HOME/.r03-XXXXXX")"

# Regenerate the pinned diff after a reviewed change.
node tools/combined/run.mjs ... --write
```

Defaults: `--pi` is the Pi 1.3.0 bundle under `provider-references/integrated`;
`--rpi` is `/home/user/Projects/pi-rust/target/debug/rpi`; the extension/library
paths can also be set with `PARITY_PI_*` / `PARITY_RPI_*` environment variables.

Exit status `0` when every segment's self-checks pass and the pinned comparison
is unchanged, `1` when a check or the pin differs, `2` on a run error.

## Output

Under `<work>/out`:

| File | Content |
| --- | --- |
| `<side>-<segment>.raw.jsonl` | every stdout record, unmodified |
| `<side>-<segment>.normalized.json` | the normalized transcript (only for comparable sides) |
| `<side>-<segment>.provider-requests.json` | Antigravity stream requests projected to method/target/account/project |
| `<side>-<segment>.cursor-fake.jsonl` | the fake SDK log |
| `<side>-<segment>.stderr.log` | child stderr |
| `diff.json` | the full positional comparison (mismatched indexes and paths) |
| `summary.json` | self-checks, provider catalogs, blocked items, request/account comparisons, per-segment diff counts |

`summary.json` also records the providers each side registered and whether the
candidate registered `parity-virtual`.

## Determinism

- Pi runs with `preload.mjs` (fixed clock advancing 1 ms per read, fixed-seed
  randomness); the vendored copy is byte-identical to
  `rpi-package/tools/parity-rpc/preload.mjs`.
- rpi is not preloaded. Its wall-clock `timestamp`/`durationMs` fields are
  removed by the normalizer and generated IDs are remapped ordinally.
- The fake Cursor SDK has its own deterministic UUID sequence. The
  `apiKeyDigest` field added to the vendored copy is the first 12 hex digits of
  `sha256(apiKey)`; it is only used to prove pool-account rotation.
- Two runs of the same side produce identical `diff.json` and identical request
  projections (checked before pinning).

## Normalization

`normalize.mjs` is a byte-identical copy of
`rpi-package/tools/parity-rpc/normalize.mjs` (X-RPC, commit `5cd2513`). **No new
normalization was added.** In particular:

- Message content, usage/cost numbers, retries, provider/model identifiers and
  event order are compared verbatim.
- Only `timestamp`/`durationMs` are dropped, the segment's temp root is
  replaced by `<TMP>`, and generated IDs are remapped ordinally.
- Unknown event types or top-level fields fail the run instead of being
  ignored.

Because the two sides differ structurally (the known X-RPC divergences), the
comparison is **pinned**, not normalized: `fixtures/expected/combined.diff.json`
records every mismatched record index. A new or resolved difference fails check
mode until the pin is regenerated on purpose.

## What the pin shows (Pi 1.3.0 vs rpi at `adc459b`)

Per compared segment the transcripts are not identical, for the reasons already
recorded by X-RPC plus the provider-specific records below. The two sides do
agree on:

- the exact reply text for every positive segment;
- the Antigravity request projection (method, target, account, project) for
  `agy-a` and `agy-b`, including which pool account was used;
- the Cursor account digest for `cursor-a` and `cursor-b`;
- one prompt and one stream per segment, no retries.

Transcript differences, by category:

1. rpi emits `ready`; Pi does not.
2. Pi emits `message_start`/`message_end` for the system and user messages; rpi
   emits only the assistant pair.
3. `message` objects differ: rpi carries `kind`, and usage/`timestamp` handling
   differs; Pi carries `api`/`provider`/`model`/`usage`/`stopReason`.
4. `message_update`: Pi carries top-level `usage` and
   `assistantMessageEvent`; rpi carries `message`/`assistantMessageEvent`/
   `eventType` and no top-level usage.
5. `response` data differs (`{accepted:true}` vs `{disposition:"started"}`).
6. Cursor adds Pi `entry_appended` records (capability notices); rpi emits none.
7. Extension status widgets/notifications (`setStatus`, `notify`) are emitted in
   a different order and count.

## Upstream limitations recorded as negative segments

Pi 1.3.0 + `pi-cursor-sdk@0.5.3` cannot be used through `pi-multiprovider`
(`0.11.3`) pools or a virtual provider. The Cursor extension registers a
request-ownership receipt keyed by the `headers` object that Pi's
`before_provider_headers` event supplies. `multiprovider/src/lift.ts`
(`applyResolvedAuth`) and `src/virtual.ts` call the backing provider's
`streamSimple` with a **new** headers object, so the receipt is missing and the
request fails before the SDK is touched:

```
Cursor provider runtime failed: Cursor provider ownership is unavailable: this call has no native session request receipt. ...
```

`cursor-pooled` and `virtual-composer` pin that exact behavior and assert zero
`Agent.create` calls. This is an upstream composition limitation, not a
normalization.

The harness writes `multiprovider-auth.json` (both provider pools and the
virtual provider) into each side's agent directory, so the candidate catalog
gains `parity-virtual` as soon as the native wrapper lands.

On the candidate side at base `adc459b`, `rpi-multiprovider` has no
`rpi_plugin_register` entrypoint yet (M-NATIVE/M-VIRTUAL and the H-DELEG host
port are unmerged). Loading `librpi_multiprovider.so` is accepted but registers
nothing, so the candidate provider catalog has no `parity-virtual` and no pool
behavior. `summary.json` records `multiproviderRegistered: false`.

## Limits

- Request bodies are not compared. The Antigravity projection compares method,
  path, account token digest and project; the body (system prompt, contents,
  generation config) is recorded only in `<side>-<segment>.provider-requests.json`
  as the server received it, and is known to differ.
- Cursor tool execution, images, cloud agents, cancellation, retries and
  compaction are not exercised.
- The fake Cursor SDK is a stand-in for `@cursor/sdk@1.0.37`; its fidelity is
  bounded by the C-FAKE evidence. It is vendored from
  `rpi-package` lane c-fake commit `2067a25` (`tests/fake-sdk`), with the
  `apiKeyDigest` field added for account-rotation evidence.
- The Antigravity certificate is the disposable loopback fixture from
  `rpi-antigravity-auth/tests/fixtures/tls`; it is not a production credential.
- One process per segment. Pool rotation across sessions is driven by rewriting
  the persisted pool order, not by a persistent scheduler cursor.
