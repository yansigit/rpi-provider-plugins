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
`tests/journal-evidence.json`. Runtime wiring and history-authorized inherited
journal reads remain in progress.

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
until host acceptance. Complete ModelRuntime refresh orchestration, configuration
recomposition, availability and native dispatcher wiring remain required.
`tests/catalog-evidence.json` covers concurrent discovery: only publication waits
for earlier acknowledgements, so a slow superseded fetch cannot block a newer
refresh or dispatch using the current catalog. The host's
`providers.refreshIsolation` capability prevents unrelated providers' updates
from invalidating a refresh and cancels superseded refreshes of the same provider.

The native directory boundary selects rpi-owned configuration, caches and SDK
SQLite stores before adapter registration. Its real SDK store and retained Pi
regressions are recorded in `tests/paths-evidence.json`; worker startup wiring
and installation verification remain required.

Native stream replay maps owned Cursor results to the host recorded-tool path,
including incomplete partial markers and cancellation-safe terminal results.
`tests/replay-evidence.json` compares outcomes with the retained Pi replay wrapper.
Native library integration and execution-count acceptance remain required.

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
dispatcher must drain these leases before closing its host connection;
installed-library loading still requires integration.

The worker entry is `runtime/main.mjs`, outside the retained Pi loader graph. It
sets rpi-owned paths before importing the adapter, then keeps a standing private
IPC call open while provider results and late session work have different
lifetimes. The dispatcher runs the retained factory, auth, catalog, hooks,
commands and skill/question tools. The Rust transport installs each supplied
invocation callback before sending its request. The native DLL must retain and
route the dispatcher's origin bindings until acknowledged release. Presentation
effects are acknowledged, while session writes and journals
continue using their explicitly awaited persistence ports.

`tests/dispatcher-evidence.json` records the dispatcher checks and remaining
integration work. `tests/native-runtime.mjs` executes this packaged entry with scripted host
responses. It covers reentrant startup catalog publication, awaited command
persistence, API-key login, both provider stream entries, and origin cleanup.
`worker/test/rpi/context.test.ts` checks private-auth conversion, dialog controls
and acknowledgement failures. These are offline process-boundary checks. The
native DLL dispatcher, complete host event/tool translation, per-child worker
ownership, installation and live Cursor acceptance remain required.
