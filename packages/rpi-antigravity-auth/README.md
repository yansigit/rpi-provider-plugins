# rpi-antigravity-auth

Work in progress: the native provider loads on Linux, but is not parity complete or ready for release.
The implemented catalog/routing module matches the pinned Pi integration, including
its `google` identity, six chat models, displayed zero prices, tier defaults and
retired model classification. The native catalog, auth callbacks and local stream driver are connected. Vault custody and commands
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
It emits through an awaited sink into the host's bounded queue. The loaded-library
regression checks missing-key streaming, preflight and public refresh refusals.

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

HTTP request framing and proxy selection are tested separately through the
original transport's intercepted TLS boundary. `transport-reference.test.ts`
runs on Node 24 (the integrated reference runtime), because Bun's response-header
iteration differs for `Set-Cookie`:

```sh
RPI_REFERENCE_DIR=/path/to/antigravity \
RPI_REFERENCE_OUTPUT="$PWD/tests/fixtures/transport.json" \
  node --experimental-test-module-mocks --test scripts/transport-reference.test.ts
```

The native serializer matches 66 raw requests, including header order, chunked
bodies, empty-body behavior and proxy precedence; 64 user-agent variants and
16 response/body cases and 15 response-error cases are retained too. It preserves the upstream rule
that `Content-Length` is removed only if `Content-Encoding: gzip` appeared first.
The native TLS driver also runs those responses through loopback TLS and authenticated
CONNECT tunnels. Gzip output and queued delivery are bounded to 16 KiB chunks;
multiple gzip members, trailing zero padding, checksum failures and truncated
responses retain the captured Node behavior. TLS tests use the public test key and
certificate under `tests/fixtures/tls`, never production credentials.
The URL dependency's IDNA adapter is locked to 1.2.0 with ICU 1.5 for Rust 1.78.
The lock retains tokio-util 0.7.19 and zeroize 1.8.2 for the same compiler baseline.

`network-reference.ts` probes the original transport on real loopback sockets.
It confirms stage-specific timeout messages, but also an upstream limitation:
body reads remain pending after idle timeout or cancellation. The candidate
currently closes the connection and returns a terminal error. This is a proposed
parity exception awaiting user agreement, not a passing parity assertion.
The driver implements the 600-second pre-response deadline and one-second trailing-usage
window. Production TLS trust settings and OS-specific socket diagnostics remain pending.

`auth-reference.ts` uses the original core and packed extension with Pi 1.3.0.
It captures 56 exchange/refresh cases and four complete callback/code login
sequences, plus PKCE, packed refresh, expiry and access-token cache cases. The
native tests compare full request forms, ordered headers, body reads, admission
checks, cancellation checkpoints, results and unconsumed requests. Run it with
Node 24's `--experimental-test-module-mocks` and the three reference environment
variables used above. Its network and UI boundaries are scripted; native callbacks are registered, while full loaded OAuth and malformed-JSON diagnostics
remain pending. No existing credentials or enrollments are changed by these tests.

`project-reference.ts` captures 30 managed-project scenarios against the original
core. The native store preserves endpoint order, tier selection, onboarding
delays, default-project fallback, refresh-key cache identity, 30-minute expiry,
concurrent request coalescing and failed-provisioning suppression. Resolution
continues when one waiter cancels; it remains owned by the provider runtime.
Real project acquisition and complete loaded OAuth acceptance are not tested yet.

`fetch-reference.ts` adds 30 ordinary Node 24 fetch cases, including compressed
OAuth replies, layered encodings, truncated payloads, reason phrases and header-only
deadlines. `local-reference.test.ts` runs seven original local-stream cases on
Node 24 with Pi 1.3.0. The driver replays all 65 original stream cases with exact
requests, events and results; separate clock tests cover deadline suspension,
cancellation and the late-usage window. TLS futures are heap-pinned at the source
boundary to keep loaded debug libraries within normal host worker stack sizes.

For offline native CLI acceptance, build the host including its CLI auth bridge
fix (`345a25f`) and this package, then run:

```sh
RPI_TEST_HOST=/path/to/pi-rust/target/debug/rpi \
RPI_TEST_PROVIDER_LIBRARY=/path/to/librpi_antigravity_auth.so \
  python3 scripts/native-acceptance.py
```

The script uses an isolated temporary agent directory and fixture key. It checks
actual library registration, explicit catalog replacement, auth discovery, private
durable API-key login, local status and logout without contacting a provider.
The host's `provider_packages` integration suite also loads this library using
`RPI_TEST_ANTIGRAVITY_LIBRARY`. These checks do not establish live OAuth/inference,
vault behavior, commands, cross-platform support or full package distribution.

The lock selects Rust 1.78-compatible UUID, async-compression, Clap, time,
hyper-rustls and optional Quinn versions. Brotli and Zstandard decoding is private
to this package; global reqwest decompression features remain disabled.

The native enrolled-vault boundary now includes scoped calls, enrollment
proposal/poll/rotation and failure reports, plus connection replacement with
coalesced reconnects and the original 60-second backoff. `vault-*-reference.ts`
executes the pinned clients and common-auth code. Fixtures cover wire decoding,
authenticated TCP frames, custody validation, enrollment authority and ceremony
write ordering. The real filesystem store passes all 168 captured ceremony scenarios. Separate
fixtures cover 341 parent-policy cases, nine secret-file cases, 74 token publication
cases and 38 lease scenarios. Linux subprocess tests inject actual `fsync` errors
and match the original old-token preservation, stage cleanup and zero-RPC result.
State and token writes retain upstream file synchronization before rename; upstream
does not synchronize the containing directory, so these APIs do not promise that.
The renewable lease preserves stale-lock and eviction-marker checks, never renews
an expired owner, and preserves a successor during release.

Windows retains both pinned native private-files policies: common-auth 0.2.1 and
Claustrum store 0.1.0. Only their N-API completion boundary is replaced by a Rust
C ABI; Antigravity needs no Node runtime. The package and tests cross-check on
Rust 1.78 for Windows GNU, and both retained C security suites compile. Windows
and macOS execution, the full fault matrix, consumer runtime, native credential-slot
custody, physical-send retry and menus remain required before vault integration
is usable. None of these checks establishes a live enrollment.

Generate these fixtures using Node 24 with `RPI_REFERENCE_DIR` pointing to the
locked Antigravity checkout and `RPI_REFERENCE_OUTPUT` naming the corresponding
`tests/fixtures/vault-*.json`. The wire generator also needs
`RPI_VAULT_PRODUCER_FIXTURE=../../vendor/claustrum-client/tests/fixtures/enrollment_wire_contract.json`.
`RPI_PRIVATE_TMP` can name a private scratch directory for filesystem fixtures;
these scripts never read a user's enrollment or credentials. Run candidate tests
with `cargo +1.78.0 test -p rpi-antigravity-auth --locked`.

Reviewed Rust components and their unchanged license notices are in `vendor/`.
Each `UPSTREAM.json` pins the original revision and source hashes and lists local
compatibility changes. The Rust Subc client preserves response representation
flags and can suppress inherited supervisor identity without changing process
environment. Its enrolled-call options preserve the pinned TypeScript response
timeout and module-reloading retry policy; other callers retain existing defaults.

The custody slot primitive now preserves the original credential bytes, writes
and synchronizes its private backup before the marker, and restores only a
matching marker or the already-restored original entry. It retains conflicts and
ambiguous backups for recovery. `vault-slot-reference.ts` captures 66 original
scenarios; `vault-slot-fsync-reference.ts` uses the scoped preload fixture to
capture failures before and after publication, including directory synchronization.
The Linux differential tests match all 71 scenarios; Windows code cross-checks
on Rust 1.78 but has not executed on Windows.

The primitive must run inside the host's `auth.credentialStoreLock` callback.
That callback uses the native login/refresh file lock, retains it through admitted
writes after cancellation, and cannot select another provider's path. The original
Pi mkdir/mtime lock is not used on native rpi's lock file. Its lease-specific timing
and compromise cases still need qualification at this native integration boundary;
these results do not declare A07 or vault startup complete. Malformed numeric JSON,
lone-surrogate JSON strings and filesystem close failures also remain unqualified.

Source selection now re-reads enrollment authority in the original order and
retains custody for an approved ceremony with a corrupt or missing token. The
settings parser preserves route identifiers and the distinct damaged/unreadable
messages. `vault-source-reference.ts` captures 286 settings/source scenarios;
`vault-startup-reference.ts` runs the original factory for 149 startup cases,
including enrollment changes after the slot write. Native source/slot logic matches
these fixtures. Actual registration, recovery menus, dynamic auth availability,
and custody requests are still pending; this is not loaded-provider vault acceptance.

The scoped custody state machine matches 40 original traces, including issuance-bound
failure reports, concurrent report coalescing, cancellation during credential acquisition,
and idle/send-triggered outage probes. It requests a fresh receipt while the vault is
reachable. During a classified outage, the pinned common-auth implementation permits
an in-memory validated receipt with more than one minute remaining; refusals, enrollment
failures and matching 401s discard it. There is no local credential fallback. The
candidate preserves these rules, including the original two-to-thirty-second idle
probe backoff. Consumer persistence, send-level retries and production
native ports remained pending at that checkpoint; these results do not qualify complete vault requests.

Roster projection and persistence now match the pinned identity/alias, incomplete
inventory, acceptance and decline rules. Differential fixtures cover 1,247
projections, 458 stored-state decodes, 16 mutations, 32 account interlocks and
19 filesystem/discovery traces, including lost ownership and lock timeout. Two
concurrent mutations also verify that the second reads the first committed view.
Locale-sensitive ordering uses the pinned Node runtime's ICU 78.3 / CLDR 48 data;
all Unicode scalars are checked for ordering and canonical decomposition. See
`data/README.md` for reproducible generation and licenses. The runtime remains
native Rust with Rust 1.78 support. Native consumer integration is still pending.

The Pi-reachable consumer now matches 41 original traces over private temporary
files. They check coalesced connection/discovery work, fresh per-attempt receipts,
peer roster changes, account decline/accept, grant visibility, token rotation,
reporting and cancellation during connection, discovery and credential reads.
Cancelled metadata waiters leave admitted shared work alive; late connections are
closed after shutdown. These are component tests, not loaded vault acceptance.

Pi never calls the embedded consumer's background `start` method. Consequently
its roster watch, periodic discovery and Insula paths are outside this Pi port's
runtime behavior. Enrollment approval watching in Pi's setup remains required.

The Antigravity send policy now matches 37 original traces, 189 admission checks
and 244 route projections. Each attempt uses a fresh receipt and project; a 401
retries once only when the same credential/account has a different record version.
The tests preserve kept-receipt handling, retry-body cancellation, final-attempt
failure reporting and cancellation ordering. These component tests do not yet
qualify the loaded provider's custody path or live inference.

Native startup now synchronizes the Google slot through the host's credential-file
lock, and session startup repeats that transition. Inherited child handlers keep
the parent's slot unchanged. Per-attempt auth preserves Pi 1.3's explicit/stored
API-key precedence and disables OAuth resolution in custody/refusal mode. Pi 1.3
still offers its inherited Gemini API-key login there; the vault stream rejects
any key other than its marker. The source checks fresh enrollment authority before
local OAuth operations and every custody request.

The loaded Linux tests cover 144 original startup configurations and 96 refusal
streams with an explicit marker, plus 40 persistent-session auth cases after the
stored credential changes. Discovery matches 68 original filesystem cases and
21 user tokens; account presentation matches 1,247 original lists and 81 request
selections. These tests use private temporary files and fake credentials. They do
not yet establish successful loaded vault inference, enrollment UI, or live access.
The new host capabilities are documented at `pi-rust@fb95b27`; the package probes
`auth.providerPolicy`, `context.inspect`, initialization, and locked slot transitions
before registration. Existing ABI offsets and package versions are unchanged.

The setup controller and menu components now match 106 original setup transitions,
260 menu/action models, 12 complete UI walks and 18 approval-watch traces. Tests
compare committed mode/enrollment files, callback order, exact salted account IDs,
messages, confirmation, choices and notifications. The watcher stops when another
process forgets the enrollment. The seam keeps the original secret-shape redaction
and shared-folder remedies. All 67 package tests pass with warnings denied on
Rust 1.78; the Windows cross-check compiles, without platform execution. Native
command registration and its production setup/watch ownership remain pending.
