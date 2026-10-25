# L1 switch-account production binding

**Status: DELIVERED for `/switch-account`. `/multilogin` is now registered; see
[`m-multilogin.md`](m-multilogin.md), which supersedes the login-presentation gap
below.**

Source change: `be20f31`
(`feat(multiprovider): bind production /switch-account runner with scope journal and announcements`).
Review fix: `33d1df3`
(`fix(multiprovider): map the host's unknown auth status faithfully and keep
announcements non-fatal`).
Base: `25737c3`, branch `lane/m-commands`.
Host pin: `e9c052653975c1d648092ab52e817a0b2f4b575c` (unchanged; no host-source edits).

## History: the commands were not dropped from the production entrypoint

`git log -S '"multilogin"' -- packages/rpi-multiprovider/src/native/mod.rs`
has no matching addition/removal. `git log -G` and blame distinguish the
reusable registrar from its production runner:

- `c69234cb54ceefa1048bfdd526f1d6d9622fb837` introduced the generic five-command
  registrar in `packages/rpi-multiprovider/src/native/commands.rs`, not a five-command
  binding in `packages/rpi-multiprovider/src/native/mod.rs`.
- `03a8b124096bbd871b2d0e086d0be10e831f8d43` explicitly bound only account/logout
  commands. Its message says the other three commands and services/virtuals were
  outside its scope; it makes no five-command parity claim.
- Blame assigns the previous production list to `b5501519`, which added `vprovider`.
  No evidence was found that production `multilogin` or `switch-account` was
  removed because a host API disappeared.

The package README already documents pending persist-first auth and awaited
switch journals/announcements. This agrees with the M02 audit in
`/home/user/Projects/.parity-lanes/ledger-audit-e9c0526.md`, not with an assumption
that merely restoring two command names restores working commands.

## Delivered: production `/switch-account`

`be20f31` registers `switch-account` in the production list in
`packages/rpi-multiprovider/src/native/mod.rs` (now
`multilogout`, `accounts`, `switch-account`, `vprovider`) and routes it in
`NativeRunner` to a new `SwitchPort`
(`packages/rpi-multiprovider/src/native/command_port.rs`) implementing the full
`commands::port::Port` surface. `Commands::with_runner` is kept; the fuller
`Commands::new` full-flow runner is not needed because it would additionally
demand `VproviderSources`, which switch never touches. Unused-by-switch `Port`
methods are honest stubs: `store_path` is empty (only login shows a save path),
`login`/`pool_manager` return errors (unreachable from this flow), and
`registered_providers` carries `oauth: None`/`api_key: None` because the host
catalog exposes only id/name/kind (see the auth-metadata gap below).

Per-method production bindings:

- `model`: `context/snapshot` `model` (`provider`, `id`, `name`); absent/null
  yields `None`, producing the TS-identical "No model is selected." notice.
- `provider_name`/`installed_provider_name`: host `providers/inspect` catalog
  entry name, falling back to the provider id exactly like the upstream
  `getProvider(...)?.name ?? providerId`.
- `has_integration`: stored pool (`service.has_provider`) whose base provider is
  still in the catalog. This mirrors `effectiveIntegration` (managed = stored
  pool + registry base); there are no external integrations natively, and
  virtual pools short-circuit before this is consulted.
- `is_virtual_pool`/`virtual_provider_name`/`is_virtual_provider`: stored
  `listVirtual` configs (`config.id == provider_id`, model id match; label from
  `VirtualProviderConfig.label`, which the Rust store requires).
- `runtime`: the catalog `authStatus` is the only production signal.
  `unknown`, `authenticated`, `expired` and a missing status all map to
  configured; only an explicit `unauthenticated` maps to unconfigured. This is
  the value the pinned host actually emits: every registration stores
  `AuthStatus::Unknown` (`crates/rpi-cli/src/model_registry.rs:117`, `:146`,
  `:175`, `:336`) because `ModelRegistry::update_auth_status`
  (`model_registry.rs:280`) has no production caller. The predicate is the
  upstream one: `/switch-account` keeps the upstream account unless
  `getProviderAuthStatus(providerId)?.configured` is exactly `false`
  (`extensions/multiprovider.ts:1377-1378`), which also covers a missing
  runtime and a missing status. label/source stay `None` and oauth stays empty.
  A failed catalog fetch yields `None`, preserving the upstream
  default-include of a missing runtime (`probeSessionRuntime` returning
  `undefined`). The host does have an extension-visible `auth` `status` action
  (`crates/rpi-cli/src/provider_runtime/auth.rs:100-163`), but it answers "is
  there a credential in `auth.json`" (compatibility credential store,
  `crates/rpi-cli/src/config/auth_store.rs:268-284`). Pi's `configured` also
  counts runtime keys, stored providers, provider config and environment
  (`node_modules/@earendil-works/pi-coding-agent/dist/core/model-runtime.js:441-451`),
  so narrowing on credential presence would hide the upstream account from
  environment-key users. That host gap is recorded below.
- `session_id`: the scope's live session id from shared state with the
  `context/inspect` `identity.sessionId` construction-time fallback — the same
  source the invoke path uses for its affinity key, so a command-written pin
  takes effect for subsequent requests.
- `snapshot`/`pin_account`/`clear_affinity`/`affinity`/`service_has_provider`/
  `reset_health`: the scope-owning scheduler via a new
  `Shared::command_service` (the scope's virtual catalog service when it owns
  the pool, else the shared service — the same selection `apply_pins` uses)
  and a new `Shared::command_snapshot` (shared plus virtual providers).
- `append_entry` (M05): a new `Shared::command_origin` returns the scope's
  origin, capturing the session first when the command scope has none, and a
  new `Shared::append_journal` journals the caller's custom type
  (`pi-multiprovider:switch-account`) through that origin. A missing origin
  fails the switch instead of claiming an unjournaled decision. `reconcile()`
  keeps `capture = false`; the capture happens lazily at journal time.
- `announce_active_account` (M06): builds the real `ServiceAnnouncement` with
  the scope scheduler, a snapshot-backed pool-accounts integration (announce
  only reads `accounts()`), no base-provider lookup (the resolve path is never
  called), and an affinity key equal to the switch's session key; then runs
  `get_active_account` + `notify_active_account_changed`. Nothing here talks to
  the host, but the call is not infallible: both steps return `Result` and the
  snapshot is a shared-state read, so it can fail. The pin and its journal
  entry are already committed at that point, so a failure is non-fatal — the
  switch stays committed and the failure is reported as a `ui/notify` warning
  ("The <pool> account selection changed, but sibling extensions were not
  notified: …") instead of an error. That matches the upstream emit shape:
  upstream publishes one announcement singleton with
  `pi.events.emit(MULTIPROVIDER_SERVICE_EVENT, …)`
  (`extensions/multiprovider.ts:686-689`), a fire-and-forget call that can
  never fail a committed switch. Delivery is still a gap, not a claim:
  `ServiceAnnouncement::new` allocates a fresh registry per call
  (`packages/rpi-multiprovider/src/announcement.rs:144-153`) and
  `notify_active_account_changed` returns early when no listener set is
  registered (`announcement.rs:328-337`), so the fan-out is structurally
  unreachable from a command invocation — not merely unsubscribed. Because the
  registry is per call, a first draft claim of "exactly like the upstream
  fan-out to an empty listener set" was wrong: upstream has one long-lived
  singleton that siblings subscribe to. Cross-extension delivery (service
  registration plus the `activeAccountChanged` topic) is unwired in the native
  binding and remains an explicit M06 remainder.

All user-facing and error strings are the ported flow's own strings
(TS-identical by construction and asserted in tests).

## Host contract gap: exact login-method presentation (unchanged, multilogin only)

No host source was changed. Under the pinned host, native auth discovery is not
rich enough to reconstruct the complete TS method presentation contract for
arbitrary providers without provider-specific guesses:

- `/home/user/Projects/pi-rust--host-e9c0526/crates/rpi-extensions/src/provider_auth.rs:25`
  defines `AuthMethod { id: String, name: String, kind: AuthMethodKind }` with
  `deny_unknown_fields`. There is no separate login label or indication of the
  TS distinction between absent and present `apiKey.login`.
- `/home/user/Projects/pi-rust--host-e9c0526/crates/rpi-cli/src/provider_runtime/auth.rs:61`
  returns that method list for `auth/methods`, including dynamically supplied
  `authMethods` results validated against the same schema.
- `/home/user/Projects/provider-references/multiprovider/extensions/multiprovider.ts:1192–1208`
  requires `oauth.loginLabel ?? oauth.name`; API-key rows depend on both
  `apiKey.name` and whether `apiKey.login` exists. An absent login produces
  `API key … (paste)` and action `api_key_paste`; an interactive login produces
  a different row and action `api_key`. Presence of an API-key native method
  alone does not communicate this distinction.

A host/provider contract decision is needed: expose canonical grouped auth
presentation metadata (OAuth name/loginLabel/login availability and API-key
name/interactive-vs-paste), or explicitly define an equivalent lossless mapping
for native method discovery. Adding guessed fields to this package's requests
would not fix the validated host response contract. No host change is proposed
as necessary for persist-first itself: the existing host already binds
`storeId`/`accountId` before login and supports resumable interactions.

Login binding was stopped at this contract gap rather than registering a command
with guessed labels or changing host source. **This gap does not block
switch-account**, whose production binding is delivered above.

## Host contract gap: runtime auth readiness (unchanged, historically M1)

No host source was changed; the package now maps the value the host actually
emits. The host cannot currently tell the package whether a provider is
configured:

- `ModelRegistry::update_auth_status`
  (`crates/rpi-cli/src/model_registry.rs:280`) is the only writer of
  `AuthStatus` and has no production caller; every registration stores
  `AuthStatus::Unknown` (`model_registry.rs:117`, `:146`, `:175`, `:336`), and
  that is what `providers/inspect` returns
  (`crates/rpi-cli/src/provider_runtime.rs:325-330`).
- The host does compute an environment/config-aware readiness internally:
  `crates/rpi-cli/src/credentials.rs:63` `resolve(...).source.is_present()`
  (builtin env names at `credentials.rs:49-56`, `models.json` API keys at
  `:84-90`), surfaced by `rpc_get_auth_status`
  (`crates/rpi-cli/src/modes.rs:1498-1529`, field `ready`). That is Pi's notion
  of readiness, but it is not reachable from an extension.
- The only extension-visible readiness query is `auth` `status`
  (`crates/rpi-cli/src/provider_runtime/auth.rs:100-163`), which reports
  `present` for the compatibility credential store (`auth.json`) only.

Proposed host interface (either form is sufficient, and the package needs no
further change): populate `AuthStatus` from
`credentials::resolve(...).source.is_present()` when providers are registered
or credentials change, or add an `auth` action `readiness` returning
`{present, source}` for the resolved credential. Until then the package keeps
`unknown` switchable, which matches upstream's `configured !== false` default
and cannot hide a working upstream account.

## Gates

All executions were offline and foreground. Cargo invocations used
`timeout 270 cargo ...; echo exit=$?`; no background jobs or network operations
were used. HOME and private TMP were `/tmp/m-commands-l1/home` and
`/tmp/m-commands-l1/tmp`, with mode 0700 (also the enclosing directory).
The preparation environment is `/tmp/m-commands-l1/env.sh`; logs are under
`/tmp/m-commands-l1/logs`. Neither `~/.pi` nor `~/.rpi` was used.

The review-fix run (`33d1df3`) used the same environment with
`/tmp/m-commands-fix/{home,tmp,logs}` (mode 0700), `source
/home/user/Projects/.parity-env.sh`, and `lane_env m-commands-195` /
`lane_env m-commands-178` for the two target directories.

The 1.78 runs additionally need `RPI_TEST_RUSTC` pointed at the 1.78 rustc so
the real-host suite builds its fixture cdylibs against the 1.78 rlibs; both
toolchains run the real e9c0526 host binary at
`/home/user/Projects/.targets/host-e9c0526-195/debug/rpi`. The fresh-DLL
procedure for the mtime-guarded real-host suite is: `cargo test --no-run`,
copy the freshly built `librpi_multiprovider.so` to a timestamped path, then run
the suite with `RPI_TEST_MULTIPROVIDER_LIBRARY` pointing at the copy (same
bytes the test binaries were built against; the mtime only satisfies the
harness's own freshness guard). The earlier `touch`-rebuild loop is not used.
The copy must be taken after the *last* relink of `tests/m_virtual_loaded`:
`cargo test` relinks that binary when the run environment differs from the
build environment (the `RPI_TEST_*` variables are part of the test target's
fingerprint), so set them on the first pass, let it relink, copy the library,
then run again — the second pass relinks nothing and the copy is newer than the
binary. This was verified in both cohorts and in a standalone direct-exe run.

| Gate | Rust 1.78.0, `-Dwarnings` | Rust 1.95.0 |
| --- | ---: | ---: |
| Provider cohort | **169 / 0 / 19** over 35 suites (`-p rpi-cursor-sdk -p rpi-multiprovider`) | **503 / 0 / 23** over 88 suites (`--workspace --exclude rpi-antigravity-auth --exclude rpi-voice`) |
| Fresh-DLL real-host suite, pinned e9c0526 host | 8 / 0 / 0 (inside the cohort run) | 8 / 0 / 0 (inside the cohort run; also 8 / 0 / 0 standalone from the direct-exe run) |
| Multiprovider native ABI (`m_native`) | 33 / 0 / 0 | 33 / 0 / 0 |

The previous L1 counts were 167 / 0 / 19 and 501 / 0 / 23 with `m_native` at
31 / 0 / 0; the review fix adds exactly two `m_native` tests per cohort and no
suite changed status.

Review-fix logs (all under `/tmp/m-commands-fix/logs`): `cohort-195.log`
(1.95 workspace cohort), `cohort-178.log` (1.78 provider cohort with
`RUSTFLAGS=-Dwarnings`), `vl-195-build.log` / `vl-195-lib.log` /
`vl-195-run.log` (standalone direct-exe real-host run against the e9c0526
host), `m_native-final.log`, and the mutation logs listed below.

Counts are passed / failed / ignored; no ignores were added (the 19/23 ignores
are pre-existing skips). The 1.95 cohort is the full workspace minus two
packages that cannot build in this container: `rpi-antigravity-auth` and
`rpi-voice` need `openssl-sys`, whose build requires OpenSSL development
headers that are not installed (no network to add them). The 1.78 cohort
additionally excludes `rpi-im-message` (`feishu-sdk` requires edition2024,
unstable on 1.78). All three exclusions are pre-existing environment/toolchain
limitations, and no workspace member depends on `rpi-multiprovider` (verified
by grepping every member `Cargo.toml`), so they hide no regression signal for
this lane. The prior L1 baseline (258 / 0 / 24) was a narrower package
selection; the current scopes are supersets and fully green.

The `m_native` switch tests (8 of that suite's 33) run the production C
entrypoint with a scripted host vtable: pin+journal through registration,
headless no-op without store/journal mutation, journal failure surfacing
without success UI, unknown model provider, picker cancel, upstream listing
while the catalog reports the host-emitted `unknown`, upstream hidden while the
catalog reports `unauthenticated`, and a switch whose announcement fails after
the pin was journaled (the journal append leaves the store unreadable) — that
last one asserts no error envelope, the committed journal entry, the success
notice and the warning notification. The registration-order assertions expect
`multilogout`, `accounts`, `switch-account`, `vprovider`.

### Differential and evidence freshness

- `commands-reference.ts`: regenerated with `--write` (67 scenarios), then
  `--check` twice; **67 scenarios byte-identical** over both checks. The two new
  scenarios are `switch-account offers the upstream account while configured`
  (upstream `configured: true` keeps `pi:default` switchable first in the
  picker; pinning it journals `accountId: pi:default`) and
  `switch-account reports an unknown model provider without pools` (provider
  id absent from the registry falls back to the id in the notice).
- `commands-a2-reference.ts`: two checks, **47 unchanged scenarios**.
- `virtual-reference.mjs`: two checks, **70 unchanged cases**.
- `ext-diff-reference.mjs`: two checks; the existing cases remain unchanged.
- All generators run locally against the pinned TS reference; no downloads.
- `commands-evidence.json` refreshed to 67/63/20 counts and fixture SHA-256
  `2693d866878be41c9d8794a42b53a3255076e25c1c1993a2c258db9157d7c94b`.
- Review fix (`33d1df3`): the fixture was regenerated twice with `--write`
  (both runs printed `wrote 67 scenarios`) and stayed byte-identical at the same
  SHA-256; `--check` printed `ok 67 scenarios byte-identical`, and
  `commands-evidence.json` is unchanged. The fix is not representable in this
  fixture: `commands-reference.ts` injects the already-mapped runtime
  (`getProviderAuthStatus: (id) => runtimeSpec.auth?.[id] ?? { configured: false }`),
  while the host `authStatus` → `configured` decision mirrors Pi's own
  `dist/core/model-runtime.js:441-451` inside the runtime object, and the
  existing `configured: true` / `configured: false` scenarios already pin both
  outcomes of the switch flow. Logs: `fixtures-write-1.log`,
  `fixtures-write-2.log`, `fixtures-check.log`.
- Freshness verification recomputed the fixture hashes, compared scenario totals
  and command-kind counts with the evidence, diffed the scenario name set
  against `25737c3` (only the two new scenarios added; all 65 prior scenarios
  preserved), and verified both mutation failures and their reversion.
  Result: PASS (`/tmp/m-commands-l1/logs/freshness.log`).
- The older `m-virtual-b2` report was not refreshed or represented as new lane
  evidence; its historical claims remain separate.

### Mutation checks

Run with Rust 1.95.0 as `cargo test -p rpi-multiprovider --test m_native`. Each
mutation was applied alone, killed by its focused production-registration test
(exit 101), and reverted (`git status` clean afterwards):

| Mutation | Killing test | Result |
| --- | --- | --- |
| `SwitchPort::runtime`: restore the pre-fix mapping (only `authenticated` and `expired` are configured) | `exported_switch_account_lists_upstream_while_host_reports_unknown_auth` (`tests/m_native.rs:1106`) | killed |
| `SwitchPort::runtime`: report `configured: true` for every provider | `exported_switch_account_hides_upstream_while_host_reports_unauthenticated` (`tests/m_native.rs:1158`) | killed |
| `SwitchPort::runtime`: always report `configured: false` | `exported_switch_account_lists_upstream_while_host_reports_unknown_auth` (`tests/m_native.rs:1106`) | killed |
| `SwitchPort::announce_active_account`: propagate the outcome again (`return Err(error)` after the commit) | `exported_switch_account_reports_a_failed_announcement_without_failing_the_switch` (`tests/m_native.rs:1189`; envelope `{"version":1,"error":"multiprovider: malformed account store"}`) | killed |
| `SwitchPort::append_entry`: swallow the journal error (`let _ = ...; Ok(())`) | `exported_switch_account_journal_failure_reports_without_success_ui` (`tests/m_native.rs:1019`) | killed |

Logs: `mut-a1.log`, `mut-a2.log`, `mut-d-configured-false.log`, `mut-b1.log`,
`mut-c-journal.log` under `/tmp/m-commands-fix/logs`. The pre-fix mapping and
`configured: false` are killed by the `unknown` test; `configured: true` is
killed by the `unauthenticated` one. The prior L1 flow-level journal mutations
(`switch_automatic_awaits_...`, `switch_selected_account_awaits_...`) still
pass unmutated in the cohort.

## Remaining requested work

- **M02 `/multilogin`:** registered after host `8f25e89` added the
  presentation metadata; see [`m-multilogin.md`](m-multilogin.md) for what is
  and is not covered.
- **M05 command gap (remainder):** exercise command-created pins through native
  branch/reload/child flows (creation path is bound and journaled; the
  restore flows predate this lane).
- **M06 command gap (remainder):** cross-extension active-account delivery —
  service registration plus the `activeAccountChanged` topic. The announce path
  executes the real notify logic but allocates a fresh registry per call, so it
  cannot reach a listener at all; a failure inside it is non-fatal and is
  reported as a warning notification after the committed switch.
- **Defense-only branches:** `No switchable accounts` is unreachable in both
  implementations (both stores drop empty pools and both registries filter
  them), and is not differentially covered; the ported branch stays as the
  upstream string. No ignores were added anywhere to cover this.
- Pool manager (`/multilogin` editing surface) belongs to the pool manager lane.

No host-source edits, remote push or user credential-directory changes were made.
