# rpi-multiprovider

Native port of `monotykamary/pi-multiprovider` at
`bd97d65a36f541935dc91e33ba15d7910a30f9d7` (0.11.3), targeting this fork's
native provider contracts and Pi 1.3.0 behavior. Original MIT notice is in `LICENSE`.

Implementation is in progress. The scheduler and session-pin parser/replay are
implemented; this package does not yet register an installable native extension.
Account persistence/authentication, stream wrapping, virtual providers, commands,
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
