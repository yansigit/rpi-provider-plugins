# Cursor session boundary

This is part of the native integration, not a parity-complete package. The worker
session implementation is `worker/src/rpi/session.ts`; provider registration and
the four ports' complete runtime wiring remain in progress.

`NativeSessionOrigin` captures a host-owned storage handle and reads its location
and atomic `view`. It accepts only committed append receipts, then verifies the
entry against a fresh authoritative view. Errors never reserve an ID or fabricate
durability. Missing custom data and explicit null stay distinct. `sync` reports
durable or ephemeral storage explicitly. Releasing an origin invalidates its
getters and operations.

`NativeSessionManager` supplies synchronous read-only getters from acknowledged
views. Its object identity stays stable; asynchronous operations retain their
captured view even when another session becomes selected. Getters return defensive
copies. Entry timestamps convert from host milliseconds to Pi ISO strings;
message timestamps remain milliseconds. Host compaction entries retain their real
IDs and retained tails. Host-derived projections supply the actual messages,
including custom-entry projectors, without inventing `firstKeptEntryId` values.
Native `active_tools_change` entries remain native history entries. These are
contracts for this retained adapter, not arbitrary TypeScript extensions.

The retained adapter now awaits custom-entry completion through settings, lineage,
resume, cleanup, cloud lifecycle and usage paths. SDK delta callbacks await usage
recording; a contract test executes the installed SDK 1.0.37 accumulator to verify
that it waits and propagates rejection. Capability-notice persistence is drained
before the owning operation completes, and failed reporting preserves the warning.
Concurrent lineage calls share one append; late lineage and resume acknowledgements
cannot update newly selected session state.

The original file-journal implementations still run in the retained Pi baseline.
Native cleanup/session synchronization, cloud/usage file-journal ports, SQLite
persistent-scope identity, background operation leases and installed-worker wiring
must be connected before native inference is qualified. This chunk does not
reinterpret SQLite file synchronization as transaction durability or activate
imported Cursor management authority.

The original source and tests remain pinned by `upstream.json`; host-boundary
patches are visible in the integration fork. The lockfile and SDK version are
unchanged. Focused tests cover delayed/rejected writes, receipt verification,
origin changes, malformed views and actual installed-SDK callback behavior.
