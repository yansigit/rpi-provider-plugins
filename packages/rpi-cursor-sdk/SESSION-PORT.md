# Cursor session boundary

This is part of the native integration, not a parity-complete package. The worker
session implementation is `worker/src/rpi/session.ts`; provider registration and
the four ports' complete runtime wiring remain in progress.

`NativeSessionOrigin` captures a host-owned storage handle and reads its location
and atomic `view`. It accepts only committed append receipts, then verifies the
entry against a fresh authoritative view. Errors never reserve an ID or fabricate
durability. Missing custom data and explicit null stay distinct. `sync` reports
durable or ephemeral storage explicitly. Origins have explicit retained leases;
the final release invalidates getters and awaits the host release acknowledgement.

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
Native cleanup uses committed host receipts, including transaction durability for
SQLite. Results arriving after navigation append to their original branch. Native
persistent SDK scope keys include both storage location and session ID, keeping
sessions in the same SQLite database separate.

`src/journal.rs` and `src/journal/port.rs` implement the Rust journal port;
`worker/src/rpi/journal.ts` implements its awaited worker calls. The native caller
resolves a captured session's path and ID before constructing the port. Worker
requests cannot supply filesystem paths. File handles are checked for regular-file
identity and links; writes use private files, process-safe locks and file sync.
Unix acknowledgements also sync the directory, including recovery after a prior
directory-sync failure. A failed sync retains uncertain bytes and returns an error.

Reads use locked snapshots with at most 16 open readers and 1 MiB IPC chunks. The
worker verifies the complete byte length and SHA-256 digest before using a snapshot.
Cloud preserves its original newline repair and tolerant record parsing. Usage
preserves complete-history validation, its 16 MiB limit and refusal to append to a
torn tail. Conditional appends revalidate if a competing writer changed a usage
journal. No interrupted send or deletion is replayed. Shutdown must close journal
readers before draining admitted writes, so an abandoned snapshot cannot hold up a
writer forever.

Cloud and usage adapter paths now await this port for native sessions. Cloud
mutations require a durable intent and check branch ownership again after awaiting
persistence; uncertain outcomes remain unresolved. Usage claims use the actual
committed entry ID before spend, and late facts remain in the captured journal.
Native reads of inherited sessions' usage journals still need the runtime's
history-authorized origin resolver; currently they produce the existing explicit
incomplete-lineage result instead of reading arbitrary history-selected paths.
`NativeSessionWork` retains a request's origins through both its streamed response
and the SDK's background completion. The retained finalizer tracks its cleanup,
and abandonment callbacks restore their original asynchronous session context.
Cloud bindings hold their selected origin until replacement or shutdown; captured
recorders retain that origin through late completion. Released resources are not
rebound to a newly selected branch. Background failures remain visible through an
awaited settlement receipt. Runtime shutdown must drain those receipts before
closing the owning host connection. `tests/lifetime-evidence.json` covers these
leases; installed-worker/provider wiring remains in progress. Imported Cursor
management authority is not activated by these changes.

The original source and tests remain pinned by `upstream.json`; host-boundary
patches are visible in the integration fork. The worker lockfile and SDK version
are unchanged. Tests include delayed/rejected writes, receipt verification, origin
changes, malformed views, the installed SDK's callback behavior, actual Rust/Node
journal IPC, competing writers, sync failures, and 16 byte comparisons against the
unchanged pinned Node journal writers. Platform compilation is not platform execution.
