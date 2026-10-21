# rpi-cursor-sdk

Integration fork of `fitchmultz/pi-cursor-sdk@f337dc4bb7cea936c52e69a7fe08e7e376c8467b`, retaining its MIT-licensed adapter and locked baseline tests under `worker/`. `upstream.json` records the Git blob identity of every retained upstream file. The SDK stays an external, exact `@cursor/sdk@1.0.37` dependency; release archives must not contain its implementation or platform binaries.

Implementation is in progress. This package is not yet installable or parity-complete. The authoritative feature ledger is `pi-rust/docs/provider-parity/README.md` and its inventory. Passing a transport test does not qualify adapter behavior.

The implementation sequence is:

1. Preserve the pinned adapter and baseline checks; build a private, authenticated local worker connection with bounded streams, generation-scoped calls, cancellation and shutdown.
2. Supply provider/auth, authoritative session, tools/UI and durable journal ports. Propagate awaited persistence through adapter call chains; keep synchronous getters on an acknowledged session snapshot.
3. Wire the native library, installed assets and retained adapter; verify local, continuity, cleanup, usage and cloud behavior against the reference host and fake SDK before bounded live acceptance.
4. Qualify packaging, imports, platform execution and composition with the other two extensions.

Host calls never use stdout/stderr. The worker transport uses a dedicated loopback connection with a random per-process secret passed through the child's private stdin, not argv or environment. Synchronous SDK callbacks use a separate transport thread, so bounded stream pressure cannot stop the IPC reader from processing cancellation or host replies. Queued stream payload uses the host's 64-item / 4 MiB accounting: an oversized item consumes the byte budget exclusively. Completion has a separate slot. Native provider streams deliver directly through this gate, without the retained Pi adapter’s two unbounded intermediate event queues. The user approved emission-time partial snapshots on 2026-10-21: unlike Pi’s shared mutable partial objects, an earlier IPC event does not acquire content from later events. Event order, deltas, signatures, usage and final results retain their original contract. These are payload limits, not a bound on SDK state, process memory, or consumer-held results.

The retained upstream README and development documents describe the original Pi host. Native installation and behavior are documented here as each boundary is implemented. Existing Pi Cursor agents, SDK stores and cloud management authority are never adopted implicitly.

Transport checks are recorded in `tests/worker-evidence.json`; the direct native stream boundary and agreed snapshot exception are verified in `tests/stream-evidence.json`.

The [session boundary](SESSION-PORT.md) adds acknowledged snapshots and committed
entry receipts, and propagates awaited persistence through the retained adapter.
Its initial verification is recorded separately in `tests/session-evidence.json`.
Native cloud/usage journals and cleanup durability are covered by
`tests/journal-evidence.json`. Complete inherited-history and remote-mutation
acceptance remains in progress.

Reproduce the transport checks with Node 24+ and Rust 1.78:

```sh
cd worker
npm ci --ignore-scripts --no-audit --no-fund
npm run build
node node_modules/vitest/vitest.mjs run test/rpi/protocol.test.ts
node --test test/rpi/transport.test.mjs
cd ..
RUSTFLAGS=-Dwarnings cargo +1.78.0 test -p rpi-cursor-sdk --locked -- --include-ignored
```

The Rust process tests are explicitly ignored in the ordinary Cargo run because they require the built Node assets; the command above enables them. Set `RPI_CURSOR_NODE` if Node 24 is not the default. The journal differential also requires the built pinned reference at `../../../provider-references/cursor`, or an explicit `RPI_CURSOR_REFERENCE` path. The tests launch the fixture worker from a different working directory and do not call Cursor. The retained adapter's original suite uses the established baseline environment: Node 24.22.0, `TERM=xterm-256color`, `CURSOR_API_KEY=offline-fixture`, and a private `TMPDIR` outside any Git repository. Its original lockfile remains unchanged.

Manual integration review and regression checks cover ordering, admission release, malformed frames, nested host calls, slow consumers, cancellation, crashes and draining admitted host work. A retained artifact-race test observed safe fixture bytes under its temporary rename path in one full run; unchanged original/fork targeted checks and a subsequent full run passed. This timing limitation remains recorded. The original 53 Pi loader checks do not establish native rpi loading.

Provider auth and awaited catalog publication boundaries are covered by
`tests/provider-ports-evidence.json`. They preserve the retained API-key login
and host-selected account environment, and delay private catalog/cache updates
until host acceptance. Native startup refresh is wired; scoped child catalogs,
configuration recomposition and full availability acceptance remain required.
`tests/catalog-evidence.json` covers concurrent discovery: only publication waits
for earlier acknowledgements, so a slow superseded fetch cannot block a newer
refresh or dispatch using the current catalog. The host's
`providers.refreshIsolation` capability prevents unrelated providers' updates
from invalidating a refresh and cancels superseded refreshes of the same provider.

The native directory boundary selects rpi-owned configuration, caches and SDK
SQLite stores before adapter registration. Its real SDK store and retained Pi
regressions are recorded in `tests/paths-evidence.json`. Worker startup and
image-relative assets execute in the Linux DLL test; package installation remains required.

Native stream replay maps owned Cursor results to the host recorded-tool path,
including incomplete partial markers and cancellation-safe terminal results.
`tests/replay-evidence.json` compares outcomes with the retained Pi replay wrapper.
Complete execution-count acceptance through the native library remains required.

The native factory boundary reuses Pi 1.3.0's ordered hook reducers, registers the
retained commands and flags, and awaits host session appends. Both provider stream
entries create their private request receipt with stable headers and cancellation
objects. Request-local credential replacement cannot restore an inherited worker
API key. The boundary refuses execution of native replay tools; executable bridge
tools belong to rpi. Verification is recorded in `tests/extension-evidence.json`.

The separate `runtime/` lock pins the integrated Pi 1.3.0 peers and external SDK
1.0.37. It does not change the retained baseline lock. Build in `worker/`, run
`npm ci --ignore-scripts --no-audit --no-fund` in `runtime/`, then run
`node tests/stage-runtime.mjs` and `node --test tests/native-extension.mjs` from this
directory to exercise the staged runtime. Background session leases and late
cloud/usage completion are covered by `tests/lifetime-evidence.json`. The native
dispatcher drains these leases before closing its host connection.

The worker entry is `runtime/main.mjs`, outside the retained Pi loader graph. It
sets rpi-owned paths before importing the adapter, then keeps a standing private
IPC call open while provider results and late session work have different
lifetimes. The dispatcher runs the retained factory, auth, catalog, hooks,
commands and skill/question tools. The Rust transport installs each supplied
invocation callback before sending its request. The native DLL retains and
routes the dispatcher's origin bindings until acknowledged release. Presentation
effects are acknowledged, while session writes and journals
continue using their explicitly awaited persistence ports.

`tests/dispatcher-evidence.json` records the dispatcher checks and remaining
integration work. `tests/native-runtime.mjs` executes this packaged entry with scripted host
responses. It covers reentrant startup catalog publication, awaited command
persistence, API-key login, both provider stream entries, and origin cleanup.
`worker/test/rpi/context.test.ts` checks private-auth conversion, dialog controls
and acknowledgement failures. These are offline process-boundary checks. The
complete host event/tool translation, installation and live Cursor acceptance
remain required. DLL dispatch and per-child ownership are checked separately below.

`src/native_host.rs` now supplies the Rust origin router used by the dispatcher.
An invocation installs its captured native context before the worker can call
back. Captured origins retain a restricted `sessions.persistenceLease` for late
session writes, while tools/auth/UI use the original live callback context.
Standing callbacks cannot borrow a root context, redirect an origin, or reopen
one after release. Journal paths come from the host's captured storage location.
Shutdown releases abandoned journal readers before draining host callbacks and
releases late captures that race shutdown. The router is bounded to 128 origins.
Its unit and real transport regression evidence is in
`tests/native-host-evidence.json`; these checks do not yet load the Cursor DLL.

Native catalog discovery resolves auth from the refresh callback itself. It
cannot reuse the registry context captured by an earlier event or restore the
worker process's inherited API key when the host reports no credentials. The
ordinary retained Pi registry/credential path is unchanged. The complete retained
suite passes 1,700 tests with two existing skips at this boundary.

`src/runtime.rs` owns adapter startup, one-time host flag configuration and
shutdown. Stream delivery runs on the foreign callback thread while IPC reads
continue on the provider runtime. Rejected emissions cancel and drain the call.
`tests/runtime-evidence.json` records real Node startup/auth/shutdown and a
blocked-emission test that still completes host callbacks, replies and cancellation.
`src/installed.rs` resolves assets beside the loaded module; staged assets and
actual DLL registration are now checked on Linux as described below. Package
installation and macOS/Windows image resolution remain unverified.

Native context snapshots require `context.trustProvenance`. Cursor project trust
uses both the effective decision and its explicit CLI/stored-decision provenance;
rpi default resource loading alone does not authorize project configuration.
The retained Pi trust path remains unchanged. `tests/trust-evidence.json` records
1,701 passing retained tests, two existing skips, the native trust boundary checks,
type/build/package checks and a real packaged dispatcher run.

The native library now registers the retained provider, API-key authentication,
catalog refresh, ten commands, flags, result hooks and executable skill/question
tools. Each owning session has a persistent Node worker; child scope leases keep
the library alive until cancellation, admitted callback drain and worker reaping
finish. Tool handles retain a single progress item and separate completion, wake
the host when ready, and join their callback thread on destruction.

`tests/native-registration-evidence.json` records real Linux library loading from
a staged directory and production host callbacks: login continuation, offline
startup catalog reentry, committed command entries, a question-tool error through
the harness, a separate child worker reaped without stopping its parent, and both
stream entries with replacement credentials that cannot restore an ambient key.
This does not test Cursor inference. Lifecycle payload parity, full tool metadata,
child catalog publication, fake-SDK feature acceptance, imports, distribution,
macOS/Windows loading and bounded live local/cloud acceptance remain required.

Native loading requires `events.lifecycleReasons` (host commit `014d492` or later).
The Linux reload check observes old-worker reaping and a newly active provider
binding. Shutdown also drains and reaps when event delivery fails before a
session context becomes available, and repeated shutdown is harmless.
`tests/shutdown-evidence.json` records those three installed-library checks;
unchanged Node adapter evidence is retained from the registration checkpoint.

Native prompt hooks require `events.promptContext` (host commit `40af9b1` or later). The host captures loaded
instruction files and skill metadata with the composed prompt, and the retained
adapter applies its original instruction deduplication and skill guidance.
Only the native boundary adapts context-path XML escaping; original Pi rendering
is unchanged. All custom messages retain listener order. `tests/prompt-evidence.json`
records the retained suite, factory/dispatcher checks and actual Linux DLL prompt
round trip. Complete trigger, lifecycle and tool parity remains required.

Native tools require `tools.metadata` (host commit `520c896` or later). Skill/question labels, execution mode and
prompt guidance reach the host's adapters; the question tool retains sequential
execution. The current retained executable tools all use direct exposure, and a
different exposure is rejected explicitly. Source provenance remains host-owned.

`worker/src/rpi/tool-surface.ts` translates replay-only declarations and activation
inside the worker. It sends only real host names to the host activation port and
includes the retained replay schemas in the Cursor request snapshot. Builtin-name
overlaps keep host execution ownership; recorded calls still use the inert result
path. A host activation change invalidates stale local replay choices.
`tests/tool-surface-evidence.json` records the metadata checks and a packaged TUI
regression that failed before this translation and passes afterward. Complete
SDK/bridge/replay execution-count acceptance remains separate.

The worker also requires `tools.exactActivation` and `tools.registrationPolicy`.
An explicitly empty host selection disables executable bridge tools, and retained
hooks preserve that selection. Local replay declarations honor the host's captured
allowlist, exclusions and `--no-tools` setting. Startup and reload supply the same
policy used to filter real registrations; missing or malformed policy is rejected.

`tools.selectionState` commits local replay choices with the host selection and
returns its exact revision. Each worker tags its presentation state with a fresh
runtime ID, so snapshots recover their own selection even before an acknowledgement
arrives. Repeated external selections clear that state, and a fresh worker ignores
the previous worker's choices. Late acknowledgements cannot replace newer choices.

The native request boundary translates Rust reasoning/cache-retention enum spelling
and absent optional values into Pi's request shape. It preserves explicit zeroes,
empty values and provider-specific options. Each request keeps the same reconstructed
headers and cancellation signal through receipt generation and either stream entry.
Legacy host provider serialization is unchanged.
