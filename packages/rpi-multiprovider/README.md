# rpi-multiprovider

Native port of `monotykamary/pi-multiprovider` at
`bd97d65a36f541935dc91e33ba15d7910a30f9d7` (0.11.3), targeting this fork's
native provider contracts and Pi 1.3.0 behavior. Original MIT notice is in `LICENSE`.

Implementation is in progress. The scheduler, account records and session-pin parser/replay are
implemented; this package does not yet register an installable native extension.
Native account authentication, stream adapter wiring, virtual providers, commands,
services, session lifecycle wiring and combined acceptance remain required.

`src/services.rs` retains inventory order, first-account bias, randomized smooth
weighted selection, least-inflight and priority selection, explicit and implicit
affinity, account/model health, configurable cooldowns and idempotent lease release.
Inventories are awaited outside the scheduler lock. Outstanding leases retain
their registration and runtime through replacement, and cancellation releases the
in-flight count. Public snapshots omit credential references.

`src/pins.rs` parses the original custom-entry and inherited-pin formats. Replay
defers unknown pools, reports stale/disabled accounts, preserves listener order
and applies later valid decisions. Host journal append and parent/child lifecycle
integration are still pending.

Run the tests from the package workspace:

```sh
RUSTFLAGS=-Dwarnings cargo +1.78.0 test -p rpi-multiprovider --locked
node --experimental-transform-types packages/rpi-multiprovider/tests/reference.mjs \
  /absolute/path/to/pinned/multiprovider > packages/rpi-multiprovider/tests/scheduler-reference.json
```

The reference script executes the unchanged source after checking its SHA-256
hashes. It injects time, random offsets and lease IDs. Candidate tests compare
every operation result, snapshot, error, pin notification and cooldown boundary;
JSON integer/float representations compare by numeric value. They also exercise
inventory waits during registration changes and cancelled-lease cleanup.
See `tests/scheduler-evidence.json` for the measured checks and remaining limits.

`src/store/` retains account-file validation, ordered JSON, private permissions,
atomic replacement, interprocess locking, account identity and normalized settings.
The source comparison checks every public result and exact persisted file; native
fault tests cover competing writers, stale locks, cancellation and failed writes.
See `tests/store-evidence.json`. Native provider auth/refresh and the malformed-JSON
diagnostic policy still require integration. Regenerate the source fixture with
Node 24 and `tests/store-reference.mjs` from this workspace.

`src/lift.rs` implements the pinned attempt loop. Its 96 differential traces
cover both stream entry paths, private response receipts, retry timing, output
boundaries, terminal/error precedence and affinity handoff. Native tests also
release leases when an auth future is dropped or a consumer stops. See
`tests/lift-evidence.json`; SDK delegation and native library loading remain pending.
The private `src/store/port.rs` transaction protocol validates account identity
and acknowledges replacement only after the account file has been replaced.
