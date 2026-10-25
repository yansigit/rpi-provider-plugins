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
| `packages/rpi-cursor-sdk/src/import_guard.rs:318` — native view ownership | Admit a view only in the host `SessionSnapshot` shape (`metadata{id,createdAt,parentSessionId}`, `revision`, `branches`, `entries`, `name`, `labels`, wrapped as `snapshot`/`leafId`/`projection`/`branchRevision`). Any other field — including an `imported`/`importedFrom`/`origin` marker or a metadata `version` the host never writes — removes authority, as do imported paths, unsafe identities and unsafe candidate IDs. Retain ordinary conversation/history entries. |
| `packages/rpi-cursor-sdk/worker/src/cursor-session-agent.ts:534` — `Agent.resume` | Indirect: its resume handle comes from the guarded projection. Rejected history yields no handle; existing `Agent.create` takes the fresh-agent path. |
| `packages/rpi-cursor-sdk/worker/src/cursor-session-agent-cleanup.ts:221` — local `Agent.delete` | Indirect: resume/cleanup candidates come from guarded native views; imported candidates never become cleanup authority. |
| `packages/rpi-cursor-sdk/src/native_host.rs:247` — production cloud journal construction | Uses `for_native_session`, not the legacy test/fixture constructor. |
| `packages/rpi-cursor-sdk/src/journal/port.rs:131` — cloud snapshot admission | Direct guard on every frame of the locked cloud-journal snapshot delivered to the worker: origin/path, version, session/anchor/agent IDs. A sibling is refused only when it is a rotated tail segment of the same journal (`<journal>.<digits>`) whose bytes would make delivery ambiguous; `.bak`, staging and prefix-sharing copies are never opened and no longer block cloud authority. Frames are validated individually from the held snapshot; the delivered digest is not compared against a whole-file hash. Native pending intents remain readable for reconciliation. |
| `packages/rpi-cursor-sdk/worker/src/cursor-cloud-lifecycle.ts:471` — durable cloud ownership reads | A native read refusal propagates; it cannot be swallowed into an empty ledger and then replaced by branch ownership. |
| `packages/rpi-cursor-sdk/worker/src/cursor-cloud-lifecycle.ts:622` and `packages/rpi-cursor-sdk/worker/src/cursor-cloud-lifecycle.ts:623` — cloud archive/delete | Indirect: guarded branch projection plus guarded durable ledger; retained ownership rechecks still run before mutation. |
| `packages/rpi-antigravity-auth/src/vault/file_store.rs:94` — every production enrollment-state/token read | Direct guard before consumer decoding drops provenance. Checks the returned record, the raw sibling records, the stash and imported paths/backup artifacts; the vault's own interrupted-write staging files (`<record>.<token>.tmp`, with or without the leading dot, including the ceremony lease) are ignored because they carry no authority. A provenance refusal surfaces as the typed `Failure::Refused`, not a string comparison. Own-slot IO/decoder errors retain their previous contracts. |
| `packages/rpi-antigravity-auth/src/vault/source.rs:140` — current source/startup synchronization/local admission | Direct raw source guard, also using guarded FileStore. Ordinary parse failures preserve existing diagnostics. |
| `packages/rpi-antigravity-auth/src/vault/source.rs:168` — per-request custody admission | Direct raw source guard before routes/token requests; also covers vault list/change-route via `Vault::open`. |
| `packages/rpi-antigravity-auth/src/native/vault.rs:97` — credential-store transactions | Direct storage guard before both custody enter and local restore. Imported stash/enrollment cannot trigger a credential swap. |
| `packages/rpi-antigravity-auth/src/native/vault.rs:114` — custody slot enter | Admits the post-swap custody state predictively before `slot.enter()` moves Pi's login into `google-login-stash.json`, then re-admits the real post-swap slot/stash. Any refusal rolls the login back with `slot.exit()` and removes the stash, so a refused transition leaves `auth.json` bytes and mode untouched. |
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
| Provider cohort excluding the separately executed `real_dll_` cases | 259 passed / 0 failed / 24 inherited ignored | 259 passed / 0 failed / 24 inherited ignored |
| Eight real-host DLL cases, fresh multiprovider DLL | 8 passed | 8 passed |
| Total cohort accounting | **267 passed / 0 failed / 24 ignored** | **267 passed / 0 failed / 24 ignored** |
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

Additional gates rerun at this lane head: 12 explicitly enabled Rust worker
tests (including native journal round-trip through worker IPC), 8 Node IPC tests
and 75 retained-worker Vitest tests (including the native archive/delete refusal
case) all passed. The loaded-host vault fixture asserts the same 15 malicious
markers per toolchain. Cold encrypted
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
  actual private FileStore/Source records and native-owned custody controls, plus
  interrupted-write staging recovery (own staging names ignored, a real `.bak`
  still refused), concurrent token writes with interleaved reads, and the typed
  `Failure::Refused` provenance path.
* `packages/rpi-antigravity-auth/src/native/vault.rs` custody tests drive every
  reachable refusal code (`missing-`, `invalid-`, `obsolete-enrollment`,
  `mismatched-custody`, `obsolete-custody-record`) and assert the host `auth.json`
  bytes and mode, and every file in the vault directory, are unchanged.
* `packages/rpi-cursor-sdk/src/import_guard.rs` admits only the host snapshot
  shape and refuses each marker the host never writes; the journal test proves a
  `.bak`/staging sibling is ignored while a rotated `<journal>.1` tail segment is
  refused.
* `packages/rpi-antigravity-auth/scripts/native-import-authority.py` loads the
  real DLL into the pinned host with 3 marker variants in each of mode, state,
  token, stash and backup files (15 cases). It asserts the exact refusal notice,
  unchanged imported files and unchanged pre-existing Pi login. The cohort also
  runs the existing native vault inference/startup negative-control fixtures.

## Mutation sensitivity

All edits were temporary and reverted before the final cohorts/builds:

| Mutation | Required failure |
| --- | --- |
| Remove Router's `guard_native_view` call | Production fixture fails: imported management `customType` remains authoritative (`left: true`, expected false). `mutation-view.log`, exit 101. |
| Remove native cloud snapshot's `guard_cloud` call | Production fixture fails: imported ledger open returns an `Ok` reader instead of the continuity refusal (`called Result::unwrap_err() on an Ok value`). `mutation-cloud.log`, exit 101. |
| Vault custody transition admits after `slot.enter()` and never rolls back | `custody_refusals_never_migrate_the_host_login` fails: `auth.json` is left holding the custody placeholder record instead of the host login (`mutation-h1.log`, exit 101). |
| `native::vault::rollback` becomes a no-op | `rollback_restores_the_login_and_drops_the_stash` fails; the stash and placeholder survive (`mutation-h1-rollback.log`, exit 101). |
| Treat the vault's own staging files as artifacts again | Admission reference fixture reports `active-backup`/fresh-enrollment refusal for its own staging names, and the interrupted-write path returns `Storage(...)` (`mutation-h2.log`, `mutation-h2-path.log`, exit 101). |
| Drop the host-shape admission in `guard_native_view` | `native_view_requires_the_host_snapshot_shape` fails; a snapshot carrying a metadata `version` the host never writes keeps its resumable entry (`mutation-m3.log`, exit 101). |
| Restore the prefix sibling check in `guard_cloud` | `cloud_authority_ignores_non_journal_siblings_and_refuses_tail_segments` fails; an ordinary `.bak`/staging sibling refuses cloud authority (`mutation-m4.log`, exit 101). |
| Return `Failure::Storage` for provenance refusals | `provenance_refusal_reaches_the_store_as_a_typed_failure` fails with `Err(Storage(...))` instead of `Failure::Refused` (`mutation-l6.log`, exit 101). |

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
* The native view shape is mirrored from the two host source locations named in
  `packages/rpi-cursor-sdk/src/import_guard.rs`; no captured real-host Cursor view
  fixture is wired, so a future host snapshot field must update both the host and
  the whitelist. Only a rotated journal tail segment is refused as a sibling;
  copied journals that were never opened were never authority sources.
* Historical standalone I02 and old host-cohort reports remain historical; this
  report and its hashed production evidence supersede their wiring limitations,
  not their upstream reference comparisons.
