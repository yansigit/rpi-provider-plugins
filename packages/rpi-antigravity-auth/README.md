# rpi-antigravity-auth

Work in progress: the native provider is not installable or parity complete.
The implemented catalog/routing module matches the pinned Pi integration, including
its `google` identity, six chat models, displayed zero prices, tier defaults and
retired model classification. Authentication, live transport, vault custody and commands
are still being implemented; see [PARITY.md](PARITY.md).

Request conversion also matches the original text, image, signed thinking/tool
history, cross-model signature removal, tool-result grouping and Gemini schema
transformations. Pi 1.3.0 system-message deltas determine the final prompt and
tool set, including ordered replacement and removal. Rust strings already contain
valid Unicode scalar values; the native JSON boundary rejects invalid UTF-8.

Request metadata and generation settings preserve the upstream session IDs,
monotonic request timestamps, cumulative model-use labels, execution rotation and
session-cache expiry/eviction. The stream state machine preserves thinking/text
signatures, tool events, partial failures, usage updates and terminal outcomes.
It emits through an awaited sink so the eventual transport driver can use the
host's bounded queue. These modules are not a loaded provider yet.

The reference is `cortexkit/antigravity-auth@a97f9055017f3c1546afbfe61a19d50b428be3d8`.
Its MIT notice is retained in [LICENSE.upstream](LICENSE.upstream).

Regenerate the catalog and 20,376 routing fixtures using the reference's locked Bun
1.4.2, a checkout at that exact revision and the established Pi 1.3.0 integration:

```sh
bun scripts/catalog-reference.ts /path/to/antigravity /path/to/integrated \
  tests/fixtures/catalog.json src/catalog.json
cargo test -p rpi-antigravity-auth --locked
cargo +1.78.0 test -p rpi-antigravity-auth --locked
```

The generator isolates its paths and refuses network access. Catalog registration
runs from the original packed extension with Pi 1.3.0; routing calls the original
source helper with its unchanged locked dependencies. The fixtures do not establish
host registration, authentication order, transport or inference acceptance.

Request/schema fixtures retain the original tests and add an edge-case corpus:

```sh
RPI_REFERENCE_DIR=/path/to/antigravity \
RPI_REFERENCE_INTEGRATED=/path/to/integrated \
RPI_REFERENCE_OUTPUT="$PWD/tests/fixtures/conversion.json" \
  bun test scripts/conversion-reference.test.ts
```

This instrumented run uses the original functions with Pi 1.3.0's transcript
helpers. Its 855 captured calls (803 schema, 52 request) are compared for values
and field order. It supplements the unchanged, locked-dependency baseline suite.
Ordered JSON requires indexmap; version 2.11.4 keeps the package on Rust 1.78.
The workspace lock also selects that version's compatible hashbrown and TOML
macro dependency chain; existing provider source and manifests are unchanged.

The same reference environment variables run `metadata-reference.test.ts`,
`stream-reference.test.ts` and `diagnostics-reference.test.ts`; set
`RPI_REFERENCE_OUTPUT` to their corresponding `tests/fixtures/*.json` file.
The stream collector invokes the original vault stream with a scripted physical
transport, fake credentials, controlled UUIDs and wall/monotonic clocks. All 59
sent JSON bodies match byte for byte, and all 303 emitted events from 65 cases
match, including six retired-model refusals before dispatch. The fixture does
not establish native enrollment, loaded-provider preflight, network framing or
deadline/cancellation acceptance. The byte-boundary tests separately preserve
when the original SSE parser produces frames, even across split CRLF and UTF-8.

The UTF-8 decoder uses the existing encoding_rs dependency, pinned to 0.8.35 for
Rust 1.78. JSON enables exact float round trips; usage values are compared without
rounding tolerances. The host compatibility contract also retains Pi 1.3.0's
`durationMs` on terminal results. Live acceptance remains not tested.
