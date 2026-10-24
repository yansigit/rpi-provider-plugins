# I02 — production imported-authority enforcement

Lane `lane/i02-guards`, based on `daee348`; SDK/host pin
`e9c052653975c1d648092ab52e817a0b2f4b575c`. This supplements the historical
standalone `i02-import-guard-evidence.json` reports: those reports did not prove
production adapter wiring. No host sources, real user homes, or live providers
were modified or contacted. Tests use local process/loopback fixtures only.

## Production boundary inventory

Paths below are relative to the repository root. “Indirect” means the retained
adapter receives only the guarded native projection/journal; it does not depend
on every SDK caller remembering to check provenance separately.

| Boundary | Enforcement |
| --- | --- |
| `packages/rpi-cursor-sdk/src/native_host.rs:144` — every captured native session view, both snapshot and branch projection | Direct `guard_native_view`; unsafe management entries are relabelled, not deleted from history. One continuity warning per captured origin. |
| `packages/rpi-cursor-sdk/src/import_guard.rs:318` — native view ownership | Reject imported/legacy markers (including malformed/false/nested markers), imported paths, unsafe identities, unsupported versions and unsafe candidate IDs. Retain ordinary conversation/history entries. |
| `packages/rpi-cursor-sdk/worker/src/cursor-session-agent.ts:534` — `Agent.resume` | Indirect: its resume handle comes from the guarded projection. Rejected history yields no handle; existing `Agent.create` takes the fresh-agent path. |
| `packages/rpi-cursor-sdk/worker/src/cursor-session-agent-cleanup.ts:221` — local `Agent.delete` | Indirect: resume/cleanup candidates come from guarded native views; imported candidates never become cleanup authority. |
| `packages/rpi-cursor-sdk/src/native_host.rs:247` — production cloud journal construction | Uses `for_native_session`, not the legacy test/fixture constructor. |
| `packages/rpi-cursor-sdk/src/journal/port.rs:131` — cloud snapshot admission | Direct guard on the exact locked bytes delivered to the worker: origin/path, version, session/anchor/agent IDs and backup/staging siblings. Native pending intents remain readable for reconciliation. |
| `packages/rpi-cursor-sdk/worker/src/cursor-cloud-lifecycle.ts:471` — durable cloud ownership reads | A native read refusal propagates; it cannot be swallowed into an empty ledger and then replaced by branch ownership. |
| `packages/rpi-cursor-sdk/worker/src/cursor-cloud-lifecycle.ts:622` and `packages/rpi-cursor-sdk/worker/src/cursor-cloud-lifecycle.ts:623` — cloud archive/delete | Indirect: guarded branch projection plus guarded durable ledger; retained ownership rechecks still run before mutation. |
| `packages/rpi-antigravity-auth/src/vault/file_store.rs:94` — every production enrollment-state/token read | Direct guard before consumer decoding drops provenance. Checks the exact returned record, raw sibling records, stash and backup/import artifacts. Own-slot IO/decoder errors retain their previous contracts. |
| `packages/rpi-antigravity-auth/src/vault/source.rs:140` — current source/startup synchronization/local admission | Direct raw source guard, also using guarded FileStore. Ordinary parse failures preserve existing diagnostics. |
| `packages/rpi-antigravity-auth/src/vault/source.rs:168` — per-request custody admission | Direct raw source guard before routes/token requests; also covers vault list/change-route via `Vault::open`. |
| `packages/rpi-antigravity-auth/src/native/vault.rs:97` — credential-store transactions | Direct storage guard before both custody enter and local restore. Imported stash/enrollment cannot trigger a credential swap. |
| `packages/rpi-antigravity-auth/src/native/vault.rs:114` — custody slot enter | After the pre-mutation storage guard, inspect the real post-swap slot/stash and call `admit`; do not invent a placeholder classification before legitimate initial custody synchronization. |
| `packages/rpi-antigravity-auth/src/native/vault.rs:137` — custody slot exit | Same pre-mutation storage guard; imported custody cannot restore a foreign saved login. |
| `packages/rpi-antigravity-auth/src/vault/runtime.rs:64` — token handed to an enrolled connection | Guarded FileStore read; consumer token decoding cannot erase an imported marker first. |
| `packages/rpi-antigravity-auth/src/vault/setup.rs:53`, `packages/rpi-antigravity-auth/src/vault/setup.rs:94`, `packages/rpi-antigravity-auth/src/vault/setup.rs:109`, `packages/rpi-antigravity-auth/src/vault/setup.rs:117` — enrollment/source management | Guarded enrollment reads and source synchronization; imported enrollment is refused rather than reused. Fresh isolated storage retains the ordinary enrollment ceremony. |

`JournalPort::for_session` remains for historical differential/worker fixtures;
all its remaining external call sites are tests. Display/status decoding is not
an authority boundary. No host change was necessary: host import archives are
not native ownership receipts, and the host importer does not copy vault state.

## Required continuity notices

Cursor:

> Imported Pi history cannot resume a Cursor agent. This conversation continues with a fresh Cursor agent; earlier Cursor branch continuity, cleanup and cloud records are unavailable.

Vault:

> Vault state that was not enrolled for this agent cannot serve Antigravity. Request vault access again; no imported custody, backup, token or local login is activated.

The implementation does not erase imported files or invent approval. Cursor
continues through its existing fresh-agent creation path. Vault callers must
complete the normal fresh enrollment ceremony in their native isolated storage;
a refusal does not silently migrate tokens, approve enrollment, or adopt Pi's login.

## Gates and fixtures

All Cargo invocations were foreground, offline and bounded by `timeout 270`,
with the requested shared caches, six build jobs and isolated mode-0700
`/tmp/i02-guards-run/{home,tmp}`. Logs live in `/tmp/i02-guards-run/logs`.

| Gate | Rust 1.78.0 (`-Dwarnings`) | Rust 1.95.0 |
| --- | --- | --- |
| Provider cohort excluding the separately executed `real_dll_` cases | 251 passed / 0 failed / 24 inherited ignored | 251 passed / 0 failed / 24 inherited ignored |
| Eight real-host DLL cases, fresh multiprovider DLL | 8 passed | 8 passed |
| Total cohort accounting | **259 passed / 0 failed / 24 ignored** | **259 passed / 0 failed / 24 ignored** |
| Native ABI, loaded Antigravity and packaged Cursor branches | 1 passed | 1 passed |
| Loaded-host I02 vault fixture | 15 malicious-marker cases refused | 15 malicious-marker cases refused |
| Library builds | passed | passed |

Both DLL toolchains used the existing real host
`/home/user/Projects/.targets/host-e9c0526-195/debug/rpi`. No separate MSRV host
was built. To satisfy the loaded harness's executable-mtime freshness check,
tests were compiled first, the multiprovider DLL was forced fresh with
`timeout 270 cargo +<version> rustc --locked --offline --lib -p rpi-multiprovider -- -C metadata=<lane-specific-marker>; echo exit=$?`,
and the already-built eight-case executable was then run directly. The native
ABI executable was likewise run directly against explicit fresh library paths.
The optional legacy `rpi-providers` loading branch was not requested.

Additional gates: 12 explicitly enabled Rust worker tests, 8 Node IPC tests,
75 retained-worker Vitest tests (including the new native archive/delete refusal
case), worker build, and source/test TypeScript checks all passed. Cold encrypted
filesystem/module loading caused bounded timeouts, including Cursor's existing
10-second worker handshake; reruns after warming the pinned Pi 1.3.0 runtime
passed on both toolchains. No timeout increase or production startup change was made.

New malicious/negative-control fixtures:

* `packages/rpi-cursor-sdk/src/native_host/import_authority_tests.rs:19` calls
  the production Router with imported/malformed/false/legacy management markers,
  checks both native projections and the actual native journal path, and retains
  native-owned resume/cleanup/cloud records as negative controls.
* `packages/rpi-cursor-sdk/worker/test/rpi/ledgers.test.ts:132` makes the real
  retained archive/delete command path receive a native provenance refusal while
  branch ownership exists; neither SDK mutation may run. The existing adjacent
  native-owned mutation tests still succeed.
* `packages/rpi-antigravity-auth/tests/i02_production_authority.rs:13` exercises
  actual private FileStore/Source records and native-owned custody controls.
* `packages/rpi-antigravity-auth/scripts/native-import-authority.py` loads the
  real DLL into the pinned host with 3 marker variants in each of mode, state,
  token, stash and backup files (15 cases). It asserts the exact refusal notice,
  unchanged imported files and unchanged pre-existing Pi login. The cohort also
  runs the existing native vault inference/startup negative-control fixtures.

## Mutation sensitivity

Both edits were temporary and reverted before the final cohorts/builds:

| Mutation | Required failure |
| --- | --- |
| Remove Router's `guard_native_view` call | Production fixture fails: imported management `customType` remains authoritative (`left: true`, expected false). `mutation-view.log`, exit 101. |
| Remove native cloud snapshot's `guard_cloud` call | Production fixture fails: imported ledger open returns an `Ok` reader instead of the continuity refusal (`called Result::unwrap_err() on an Ok value`). `mutation-cloud.log`, exit 101. |

## Determinism and freshness

The multiprovider command/A2 and Cursor package-asset reference generators were
run twice; both SHA-256 lists are identical. The worker change necessarily
changes the packaged runtime bundle hash; R01's asset manifest and its enclosing
evidence digest were mechanically refreshed, not hand-authored as expectations.
The non-writing Cursor reference check verifies the staged assets again.

`docs/evidence/i02-production.json` records source, library and gate-log hashes.
Refresh/check it locally after rerunning the gates:

```sh
python3 .github/scripts/refresh-i02-evidence.py --write
cp docs/evidence/i02-production.json /tmp/i02-guards-run/evidence-first.json
python3 .github/scripts/refresh-i02-evidence.py --write
cmp /tmp/i02-guards-run/evidence-first.json docs/evidence/i02-production.json
python3 .github/scripts/refresh-i02-evidence.py
```

## Residual limits

* No live Cursor/vault calls were made. Malicious Cursor markers are tested
  through the production native Router and retained management command path,
  not a real-host Cursor model turn. Loaded malicious fixtures are vault-host
  cases; loaded Cursor ABI startup is a separate gate.
* A byte-identical, unmarked copy in trusted native storage cannot establish its
  cross-agent provenance cryptographically. This change enforces markers,
  imported paths, record identity/version, artifacts and real slot/stash ownership;
  it does not add a new host provenance ledger. Host import does not copy vaults.
* Historical standalone I02 and old host-cohort reports remain historical; this
  report and its hashed production evidence supersede their wiring limitations,
  not their upstream reference comparisons.
