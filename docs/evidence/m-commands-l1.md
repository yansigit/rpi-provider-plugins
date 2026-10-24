# L1 switch-account production binding

**Status: DELIVERED for `/switch-account`. `/multilogin` remains unregistered.**

Source change: `be20f31`
(`feat(multiprovider): bind production /switch-account runner with scope journal and announcements`).
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
- `runtime`: the catalog `authStatus` is the only production signal (no
  session-runtime auth-status port exists on this host). `authenticated` and
  `expired` map to configured; `unauthenticated`/missing maps to unconfigured;
  label/source stay `None` and oauth stays empty. A failed catalog fetch yields
  `None`, preserving the upstream default-include of a missing runtime
  (`probeSessionRuntime` returning `undefined`).
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
  `get_active_account` + `notify_active_account_changed`. This executes the
  exact ported notify path and is infallible with no host I/O. With no
  subscribers registered it notifies nobody, exactly like the upstream fan-out
  to an empty listener set. Cross-extension delivery (service registration
  plus the `activeAccountChanged` topic) is still unwired in the native
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

## Gates

All executions were offline and foreground. Cargo invocations used
`timeout 270 cargo ...; echo exit=$?`; no background jobs or network operations
were used. HOME and private TMP were `/tmp/m-commands-l1/home` and
`/tmp/m-commands-l1/tmp`, with mode 0700 (also the enclosing directory).
The preparation environment is `/tmp/m-commands-l1/env.sh`; logs are under
`/tmp/m-commands-l1/logs`. Neither `~/.pi` nor `~/.rpi` was used.

The 1.78 runs additionally need `RPI_TEST_RUSTC` pointed at the 1.78 rustc so
the real-host suite builds its fixture cdylibs against the 1.78 rlibs; both
toolchains run the real e9c0526 host binary at
`/home/user/Projects/.targets/host-e9c0526-195/debug/rpi`. The fresh-DLL
procedure for the mtime-guarded real-host suite is: `cargo test --no-run`,
copy the freshly built `librpi_multiprovider.so` to a timestamped path, then run
the suite with `RPI_TEST_MULTIPROVIDER_LIBRARY` pointing at the copy (same
bytes the test binaries were built against; the mtime only satisfies the
harness's own freshness guard). The earlier `touch`-rebuild loop is not used.

| Gate | Rust 1.78.0, `-Dwarnings` | Rust 1.95.0 |
| --- | ---: | ---: |
| Provider cohort | **167 / 0 / 19** over 35 suites (`-p rpi-cursor-sdk -p rpi-multiprovider`) | **501 / 0 / 23** over 88 suites (`--workspace --exclude rpi-antigravity-auth --exclude rpi-voice`) |
| Fresh-DLL real-host suite, pinned e9c0526 host | 8 / 0 / 0 (inside the cohort run) | 8 / 0 / 0 (inside the cohort run) |
| Multiprovider native ABI (`m_native`) | 31 / 0 / 0 | 31 / 0 / 0 |

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

The six new `m_native` switch tests run the production C entrypoint with a
scripted host vtable: pin+journal through registration, headless no-op without
store/journal mutation, journal failure surfacing without success UI, unknown
model provider, picker cancel, and upstream listing while the catalog reports
`authenticated`. The registration-order assertions now expect
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
- Freshness verification recomputed the fixture hashes, compared scenario totals
  and command-kind counts with the evidence, diffed the scenario name set
  against `25737c3` (only the two new scenarios added; all 65 prior scenarios
  preserved), and verified both mutation failures and their reversion.
  Result: PASS (`/tmp/m-commands-l1/logs/freshness.log`).
- The older `m-virtual-b2` report was not refreshed or represented as new lane
  evidence; its historical claims remain separate.

### Mutation checks

Both checks use the final generated fixtures and Rust 1.95.0. Each mutation was
applied alone, killed by its focused production-registration test (exit 101),
and reverted:

| Mutation | Killing test | Result |
| --- | --- | --- |
| `SwitchPort::append_entry`: swallow the journal error (`let _ = ...; Ok(())`) | `exported_switch_account_journal_failure_reports_without_success_ui` | killed |
| `SwitchPort::runtime`: always report `configured: false` | `exported_switch_account_lists_upstream_while_host_reports_authenticated` | killed |

Logs: `mutation-journal-195.log`, `mutation-runtime-195.log`. The prior L1
flow-level journal mutations (`switch_automatic_awaits_...`,
`switch_selected_account_awaits_...`) still pass unmutated in the cohort.

## Remaining requested work

- **M02 `/multilogin`:** still unregistered, blocked on the host auth-metadata
  contract above. Pending-account workflow, nested auth binding, labels/reauth/
  cancellation/error parity, and virtual editing acceptance remain incomplete.
- **M05 command gap (remainder):** exercise command-created pins through native
  branch/reload/child flows (creation path is bound and journaled; the
  restore flows predate this lane).
- **M06 command gap (remainder):** cross-extension active-account delivery —
  service registration plus the `activeAccountChanged` topic. The announce path
  executes the real notify logic; with no subscribers it notifies nobody.
- **Defense-only branches:** `No switchable accounts` is unreachable in both
  implementations (both stores drop empty pools and both registries filter
  them), and is not differentially covered; the ported branch stays as the
  upstream string. No ignores were added anywhere to cover this.
- Pool manager (`/multilogin` editing surface) belongs to the pool manager lane.

No host-source edits, remote push or user credential-directory changes were made.
