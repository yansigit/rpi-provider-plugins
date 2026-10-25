# rpi-multiprovider

Native port of `monotykamary/pi-multiprovider` at
`bd97d65a36f541935dc91e33ba15d7910a30f9d7` (0.11.3), targeting this fork's
native provider contracts and Pi 1.3.0 behavior. Original MIT notice is in `LICENSE`.

The package has a loadable native extension entry point, a global provider wrapper,
and credential-store and lifecycle integration. It registers `/multilogin`,
`/multilogout`, `/accounts`, `/switch-account` and `/vprovider`. This is partial
integration, not full parity or release completion. Same-provider v1 delegation
and the DLL command callbacks are tested with a fake host vtable (not a real rpi
host RPC). `/multilogin` logs in through the host `auth` port into a private
pending account of this plugin's credential store and commits the captured
credential with the store's own `add`/`replace`
([evidence](../../docs/evidence/m-multilogin.md)). Cross-extension
`activeAccountChanged` delivery, virtual-provider application and host
delegation support remain pending. Full cross-provider behavior, virtual/native services, Fabric, managed
provider integration and end-to-end runtime acceptance remain incomplete.

The three SDK pins are ECC9-aligned with the host; see the
[SDK cohort record](../../docs/provider-parity-sdk-cohort.json) and
[native command evidence](tests/m_native_cmd-evidence.json). This does not imply
release-qualified installation: the SDK-pin change is local and unpublished.

`src/services.rs` retains inventory order, first-account bias, randomized smooth
weighted selection, least-inflight and priority selection, explicit and implicit
affinity, account/model health, configurable cooldowns and idempotent lease release.
Inventories are awaited outside the scheduler lock. Outstanding leases retain
their registration and runtime through replacement, and cancellation releases the
in-flight count. Public snapshots omit credential references.

`src/pins.rs` parses the original custom-entry and inherited-pin formats. Replay
defers unknown pools, reports stale/disabled accounts, preserves listener order
and applies later valid decisions. Native core paths for awaited journal append
and scoped parent/child lifecycle exist; command-driven writes (including
`/switch-account`) and complete parent/child acceptance remain pending.

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
`tests/lift-evidence.json`; native library loading exists, while full SDK delegation
and final integration remain pending.
The private `src/store/port.rs` transaction protocol validates account identity
and acknowledges replacement only after the account file has been replaced.
