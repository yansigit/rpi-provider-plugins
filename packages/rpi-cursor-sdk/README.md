# rpi-cursor-sdk

Integration fork of `fitchmultz/pi-cursor-sdk@f337dc4bb7cea936c52e69a7fe08e7e376c8467b`, retaining its MIT-licensed adapter and locked baseline tests under `worker/`. `upstream.json` records the Git blob identity of every retained upstream file. The SDK stays an external, exact `@cursor/sdk@1.0.37` dependency; release archives must not contain its implementation or platform binaries.

Implementation is in progress. This package is not yet installable or parity-complete. The authoritative feature ledger is `pi-rust/docs/provider-parity/README.md` and its inventory. Passing a transport test does not qualify adapter behavior.

The implementation sequence is:

1. Preserve the pinned adapter and baseline checks; build a private, authenticated local worker connection with bounded streams, generation-scoped calls, cancellation and shutdown.
2. Supply provider/auth, authoritative session, tools/UI and durable journal ports. Propagate awaited persistence through adapter call chains; keep synchronous getters on an acknowledged session snapshot.
3. Wire the native library, installed assets and retained adapter; verify local, continuity, cleanup, usage and cloud behavior against the reference host and fake SDK before bounded live acceptance.
4. Qualify packaging, imports, platform execution and composition with the other two extensions.

Host calls never use stdout/stderr. The worker transport uses a dedicated loopback connection with a random per-process secret passed through the child's private stdin, not argv or environment. Synchronous SDK callbacks use a separate transport thread, so bounded stream pressure cannot stop the IPC reader from processing cancellation or host replies. Queued stream payload uses the host's 64-item / 4 MiB accounting: an oversized item consumes the byte budget exclusively. Completion has a separate slot. These are payload limits, not a bound on SDK state, process memory, or consumer-held results.

The retained upstream README and development documents describe the original Pi host. Native installation and behavior are documented here as each boundary is implemented. Existing Pi Cursor agents, SDK stores and cloud management authority are never adopted implicitly.

Transport checks are recorded in `tests/worker-evidence.json`.

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
