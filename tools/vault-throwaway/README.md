# Throwaway CortexKit vault for rpi's Antigravity vault client

This directory builds and runs a disposable CortexKit credential vault: the `ck-subc`
supervisor/router (from `provider-references/subconscious`) with the `ck-claustrum`
credential module (from `provider-references/claustrum`) registered under it. rpi's
Antigravity vault client (`packages/rpi-antigravity-auth`, `src/vault/*`) enrolls against
it, is approved by an operator, and receives a credential through custody.

Everything is isolated, fake, and loopback-only. It never reads or writes `~/.pi`,
`~/.config/cortexkit`, `~/.local/share/cortexkit`, or a real CortexKit install, and it never
holds a real credential. This is the rig for lane `vault-harness` (ledger rows A06 to A08,
R04). Live vault acceptance with real credentials is a separate step, run by the coordinator.

## Files

| file | what it does |
| --- | --- |
| `lib.sh` | Shared helpers: lane environment, the isolated `env -i` environment, process and socket discovery, state-directory checks. Sourced, not run. |
| `build.sh` | Copies the two reference trees into `/home/user/Projects/.targets/vault-src`, builds with `--locked`, prints each binary's path. |
| `start.sh <state-dir>` | Creates the isolated tree, writes `subc.jsonc`, bootstraps a file-backed master key, starts `ck-subc`, waits for health, prints the connection-file path. |
| `approve.sh <state-dir> <cmd>` | Operator wrapper: `pending`, `list`, `approve <id>`, `deny <id>`, `revoke`, `grant`. |
| `store-credential.sh <state-dir> <auth-json> --account-id ID` | Stores an Antigravity (`antigravity:google`) OAuth record from a Pi-format `auth.json` file and grants the enrolled name read access. Never prints values. |
| `teardown.sh [--keep-state] <state-dir>` | Stops the supervisor and the module, verifies no process and no listening socket is left, deletes the state directory. |
| `dry-run.sh` | The end-to-end check with fake data: build, start, enroll, approve, store, custody fetch, negative control, teardown. |
| `harness/` | `vault-check`, a small crate (own workspace, own `Cargo.lock`) that calls rpi's vault modules through their public API. |

## Steps

Prerequisites: the lane environment file `/home/user/Projects/.parity-env.sh`, Rust 1.95
(default), `rsync`, `ss`, `setsid`, `python3`. Nothing needs root.

Full run (fake data, about 2 minutes, cached builds are faster):

```sh
cd /home/user/Projects/rpi-package--vault-harness/tools/vault-throwaway
./dry-run.sh
```

Manual run, to keep a vault up for inspection or for the coordinator:

```sh
./build.sh
STATE="$(mktemp -d "$HOME/.vault-throwaway.XXXXXX")" && chmod 700 "$STATE"
./start.sh "$STATE"                       # prints the connection file rpi must use
. "$STATE/rpi.env"                        # exports SUBC_CONNECTION_FILE and RPI_VAULT_DIR
./approve.sh "$STATE" pending             # after rpi's Request vault access step
./approve.sh "$STATE" approve <request-id>
./store-credential.sh "$STATE" /path/to/auth-pi.json --account-id <non-secret id> --email <email>
./teardown.sh "$STATE"
```

`start.sh` requires a state directory that is absolute, empty or missing, and whose parent is
private. Any group- or other-writable ancestor is refused, because `ck-subc` itself refuses to
publish its connection file under such a path (`~/.cache` is mode 775 on this machine, so it
is not used).

### How rpi finds the daemon

`src/vault/discovery.rs::resolve` checks, in order: an explicit path, `SUBC_CONNECTION_FILE`,
`CLAUSTRUM_SUBC_CONNECTION`, `$XDG_RUNTIME_DIR/subc-connection.json` if it exists,
`$HOME/.local/share/cortexkit/run/subc-connection.json`, then the temp-directory fallback.
The rig sets `SUBC_CONNECTION_FILE` to `<state-dir>/run/subc-connection.json`, so the
discovery result is unambiguous and never falls through to another daemon.

## What is isolated

- Every daemon and operator process runs under `env -i` with `HOME`, `XDG_CONFIG_HOME`,
  `XDG_DATA_HOME`, `XDG_STATE_HOME`, `XDG_RUNTIME_DIR` and `TMPDIR` inside the state
  directory, plus `SUBC_CONNECTION_FILE`. `SUBC_MODULE_ID`, `SUBC_LAUNCH_NONCE` and `CK_*`
  variables from the caller are not inherited.
- `subc.jsonc` lives at `<state>/xdg/config/cortexkit/subc.jsonc`. The supervisor port is `0`,
  so the kernel picks a free port. On this run it bound `127.0.0.1` and `::1` on the same
  ephemeral port. Nothing listens on `8757`, the default port of a real install.
- The master key is created by `ck-auth bootstrap` at `<state>/keys/master.key` (mode 600,
  parent 700), outside the data directory, as the operator runbook requires. The key-source
  record is `file:<that path>`. The module receives the same path through `CK_MASTER_KEY_PATH`.
- The vault store is `<state>/xdg/data/cortexkit/claustrum/store.db`.
- Binaries are symlinks in `<state>/bin`. Every process therefore has an `argv[0]` under
  `<state>/bin`, which is how `teardown.sh` finds exactly this rig's processes and nothing else.
- `drain_timeout_ms` is 5000 (the default is 30000). A throwaway vault has nothing to flush.
- The rpi-side driver runs with the same `env -i` environment, so rpi's own code sees the
  throwaway `HOME` as well.
- Fake data is generated per run in `<state>/fake` (mode 600): a Google-shaped `auth.json`
  entry whose access token is valid for 24 hours and whose packed refresh value names a fake
  project. Because the token never expires inside the run, the vault never refreshes it, so
  no request ever reaches Google.

## What is not isolated, and why

- The build uses the real Cargo registry cache and the lane's private target directory
  `/home/user/Projects/.targets/vault`. The reference trees are copied to
  `/home/user/Projects/.targets/vault-src` first, so `provider-references` is only read.
- The rpi crate `rpi-antigravity-auth` is compiled from this worktree, against the lane's
  redirected `rpi-plugin-sdk` pin (see `.parity-env.sh`).

## Dry-run checks (what `dry-run.sh` asserts)

1. `build.sh` succeeds with `--locked` for all five binaries.
2. `start.sh` reports `vault: ok` and `claustrum  running`.
3. rpi's `Setup::connect_step` proposes `antigravity-auth-pi` and reports `pending` with a
   request id. `approve.sh pending` lists exactly that id.
4. After `approve`, rpi's poll reports `approved`, token generation 1.
5. `store-credential.sh` imports `antigravity:google` (category `antigravity-native`, plus
   `llm-provider`) and grants `enrolled:antigravity-auth-pi` read on `antigravity-native`.
6. rpi's `Setup::use_vault` selects `vault:antigravity:google~…`.
7. rpi's `Vault::send` (the per-request custody admission used by Pi's request path) receives
   the fake bearer. Its SHA-256 equals the fixture's, the admission carries the fake project
   and account identity, and the bearer is never printed.
8. `ck-auth events` shows `scoped_first_use` for `enrolled:antigravity-auth-pi`, and
   `verify-audit` reports `audit chain verified: intact`.
9. Negative control: after `approve.sh revoke`, the same custody admission is refused with
   `not_found`, and no bearer is delivered.
10. `teardown.sh` stops both processes, finds no listening socket on any recorded port, and
    deletes the state directory.

## Observed behavior (dry run, this machine)

- Enrollment: propose → `pending`; approve → `enroll poll` → `approved`, generation 1.
  The token file rpi writes is `pi-enrollment.json` under the vault directory, mode 600.
- Custody: `credential.list_scoped` and `credential.get_scoped` succeed with the enrollment
  token; the admission carries `credentialId: antigravity:google`, `recordVersion: 1`,
  `projectId` and `accountIdentity` from the vault.
- Teardown: `ck-subc` exits on SIGTERM and stops the module, which exits too. The rig needed
  no SIGKILL.
- A revoked enrollment answers `not_found`, the same as an unknown token. rpi's local status
  still reads `approved` and `custody` after the server-side revocation, until the next call
  reaches the vault. That is the expected boundary of a local, cached enrollment state.

## Findings that shaped the rig

- `ck-auth put` creates a static record with no OAuth shape. rpi refuses non-OAuth routes
  (`api-key-refused`, `send.rs::admission_metadata`), so a static key cannot be used to test
  Antigravity custody.
- `ck-auth import --source antigravity` stores an empty access token and no expiry
  (`oauth.rs::import_antigravity_account`). The first `get` would therefore refresh against
  Google's real token endpoint. That is a real network service and is avoided here. The rig
  uses `--source pi` with `--provider google` and an explicit access token and expiry. The
  antigravity adapter still reads the packed `<refresh>|<projectId>` form, so the project and
  identity behave the same way.
- Custody requires an asserted account identity (`--account-id`, and `--email` for display).
- The `ck health` line for claustrum reads `unknown` even while it is serving. The readiness
  signal the rig uses is `ck-auth status` printing `vault: ok`, which is a live round trip
  through the module.
- Both supervisor endpoints share one ephemeral port; the connection file lists both
  `127.0.0.1` and `::1`.
- The module's pinned crates.io `subc-protocol` (0.32.5) and the subconscious checkout's
  (0.32.8) worked together in the HELLO exchange (same wire version, 2).

## Not tested, and deviations from the lane brief

- The brief allowed driving enrollment through `rpi --mode rpc` and `/antigravity`, or a
  small Rust driver "if simpler". The rig uses the Rust driver, which calls the same
  `Setup::connect_step`, `Setup::use_vault` and `Vault::send` that the menu and request path
  call, so no rpi binary is built and no `pi-rust` build is run. The menu's own UI flow
  (`tests/native_menu.rs` shape) was not exercised against the real vault. That is a useful
  follow-up for the coordinator's live acceptance.
- The custody dispatch is a stub: the driver records the admission and returns `200`. No
  Antigravity or Google request is made, so the rig proves delivery of the bearer through
  custody, not that Google accepts it.
- The start-failure path (`start.sh` trap → `teardown.sh --keep-state`) is written but was not
  exercised by a failing run.
- Only one state directory, one vault and one enrolled name were exercised. No concurrent
  consumers, no refresh, no re-approval after a re-proposal, and no key rotation.
- `ck` and `ck-auth` were run from the `debug` profile. No release build was tested.
