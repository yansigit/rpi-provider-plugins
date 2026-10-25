# `/multilogin` production binding

**Status: DELIVERED for `/multilogin` against parity host
`8f25e899d1c6bf6f98a73d4800c0363c185ad5fc`. Not covered: a real-host run
(every end-to-end test here drives the production C entrypoint with a fake host
vtable), cross-extension `activeAccountChanged` delivery from a login commit, and
env/`models.json` readiness in the login presentation.**

Source change: `a245ab9`
(`feat(multiprovider): register /multilogin through a persist-first pending login`).
Base: `b776e56`, branch `lane/multilogin`.
Host pin: `8f25e899d1c6bf6f98a73d4800c0363c185ad5fc` (only package-side changes; the
host checkout was read-only and unmodified).

The companion L1 report
([`m-commands-l1.md`](m-commands-l1.md)) described `/multilogin` as unregistered
because the host exposed no login-method presentation. Host `8f25e89` added the
`auth` `methods` action with `loginLabel`/`interactiveLogin`, which is what this
change binds.

## Registration and routing

- `packages/rpi-multiprovider/src/native/mod.rs:1252-1258` adds `multilogin` to
  the registered command list (`multilogin`, `multilogout`, `accounts`,
  `switch-account`, `vprovider`). Until this change the manifest entry and the
  routing arm existed at the base commit (`b776e56`) but the name was never
  registered, so the command was unreachable.
- `packages/rpi-multiprovider/src/native/commands.rs:44` is the manifest entry
  (pre-existing); `:103` routes `"multilogin" => login::multilogin(ui, port,
  args)`, with the same `Ui`/`Port` pair `/switch-account` uses (pre-existing).
- `packages/rpi-multiprovider/src/native/commands.rs:115` derives `Clone` on
  `Dialogs` so a `SwitchPort` can own an optional copy for the pool manager.
- `packages/rpi-multiprovider/src/native/command_port.rs:281-295` extends the switch
  cache with a per-provider method cache and `SwitchPort` with
  `dialogs: Option<Dialogs>` (installed by the runner at `:189`); only
  `/multilogin` gets dialogs, so
  `/switch-account` never fetches `auth` `methods` and keeps its single snapshot.

## Design: persist-first pending reservation

Why a private account is needed at all:

- The host persists the credential **before** it reports success: on a
  `credential` response it locks the destination store account and calls
  `transaction.replace(credential)` and only then answers
  `{"kind":"success"}` (`/home/user/Projects/pi-rust--host-8f25e89/crates/rpi-cli/src/provider_runtime/auth.rs:347-366`).
  The destination is fixed when `login` starts, from `storeId` + `accountId`
  (`.../provider_runtime/auth.rs:28-61`), so it cannot be deferred.
- A freshly added pool account has no record yet: upstream calls
  `store.addAccount(...)` only after the login returned its credential
  (`/home/user/Projects/provider-references/multiprovider/extensions/multiprovider.ts:1256`),
  and a reauthentication calls
  `store.replaceAccountCredential(...)` on an account that already exists
  (`.../extensions/multiprovider.ts:1227`). Writing the host's own store write
  through would have created an account row for an id the pool never chose.

The mechanism, therefore, is a reservation that the store port recognizes but the
account file never receives:

- `packages/rpi-multiprovider/src/native/credentials.rs:25` `PENDING_ACCOUNT_PREFIX
  = "pending-login-"`; `:35-38` `PendingLogin { provider, credential }`;
  `State.pending_logins` (`src/native/mod.rs:85`, initialized at `:139`) holds
  them in memory only.
- `reserve_pending_login` (`src/native/credentials.rs:320-335`) mints one id per
  login; `take_pending_login` (`:337-345`) removes it and returns what the host
  persisted; `pending` (`:305-318`) resolves an id and rejects an id reserved by
  a different provider.
- The store port short-circuits every reservation before it reaches the file:
  `store_read`/`lock_credential` return the reserved credential instead of "no
  such account" (`src/native/credentials.rs:359-367` and `:370-414`, both through
  `pending` at `:305-318`), `replace_credential`
  (`:416-434`) captures the credential and reports success without writing,
  `remove_credential` (`:436-450`) clears the capture. The public pool and the
  scheduler never see a reservation, so nothing enumerates or schedules one.
  `set_pending` (`src/native/credentials.rs:346-356`) is the capture step.
- `packages/rpi-multiprovider/src/native/login.rs:99-105` is a `Drop` guard that
  drops the reservation on every exit path (success, cancellation, host error,
  presentation error), so a failed login leaves no pending entry behind.
- `login(...)` (`src/native/login.rs:108-176`) reserves, sends
  `{"port":"auth","action":"login","providerId":…,"methodId":…,
  "storeId":"multiprovider","accountId":<reservation>}`, then loops over the host
  replies: `success` returns the captured credential (and errors if the host
  claimed success without persisting one), `cancelled` maps to `CANCELLED`, and
  `interaction` drives `present` and answers with `continue`. A presentation
  error cancels the host login with `cancelLogin` on a fresh token
  (`src/native/login.rs:142-160`) because the host keeps an unanswered login
  until cancelled and the command's own token may already be cancelled.
- `present` (`src/native/login.rs:178-258`) maps `input` (`secret` → masked
  prompt, `url` → auth-url notification, `allowEmpty` and `message` honoured),
  `select` (`choices` id/label) and `wait` (`pollAfterMs`, cancellable) onto the
  existing `LoginDialog` interaction (`src/commands/login.rs:254`).

The command itself is the ported upstream flow
(`packages/rpi-multiprovider/src/commands/login.rs:552-606`): interactive-mode
gate, `reconcile`, registered non-virtual providers, `select_login`, the buffered
pool defaults, `pool_manager`, then `Add`/`Reauth`/`Closed` dispatch. A login
result is committed with the store's own write (`add_account` `:486`,
`reauthenticate` `:417`), and `CANCELLED` reads as "user backed out", not an
error.

## Host contract used

| Piece | Shape | Host source |
| --- | --- | --- |
| `auth` `methods` | `[{id, kind, name, loginLabel?, interactiveLogin?}]`; unknown `kind`s are ignored | `crates/rpi-cli/src/provider_runtime/auth.rs:69-97` |
| `auth` `login` | `{providerId, methodId, storeId, accountId}` → `{kind:"interaction", loginId, interaction}` or `{kind:"success"}`/`{kind:"cancelled"}` | `.../provider_runtime/auth.rs:133-155`, `:335-387` |
| `auth` `continue` / `cancelLogin` | `{providerId, loginId, value}`; the ticket must belong to the same owner | `.../provider_runtime/auth.rs:102-118` |
| interaction kinds | `input{title,message,secret,url,allowEmpty}`, `select{title,choices[{id,label}]}`, `wait{message,pollAfterMs}` | `crates/rpi-extensions/src/provider_auth.rs` (`AuthInteraction::parse`) |
| store for a login | `storeId` must be a store of this plugin; `accountId` is required | `.../provider_runtime/auth.rs:28-61` |
| presentation capability | `auth.methodPresentation` advertises that `loginLabel`/`interactiveLogin` are understood | `crates/rpi-extensions/src/native.rs:304`, `crates/rpi-plugin-sdk/src/lib.rs:1457` |

`packages/rpi-multiprovider/src/native/login.rs:28-54` parses exactly those four
fields (`loginLabel` keeps only a non-blank label, mirroring an absent field) and
`:56-70` turns them into the Pi presentation: an `oauth` method always has a
login, an `api_key` method has one unless the host says
`interactiveLogin: false`, and older hosts that omit the flag keep the provider's
own login. The capability string itself is not probed: the fields are read from
whatever the running host's `methods` reply contains, so a pre-`8f25e89` host
simply omits them.

## Pi parity mapping

| Pi | Port |
| --- | --- |
| `extensions/multiprovider.ts:1096-1102` `/multilogin` handler + interactive-mode notice | `src/commands/login.rs:552-555` |
| `:1103` `reconcile(ctx)` | `:556` (`Port::reconcile`) |
| `:1104-1106` `uniqueProviders(...).filter(!virtualProviders.has(id))` | `:557-559` |
| `src/multilogin.ts:85` `selectLogin` | `src/commands/login.rs:104` `select_login` |
| `:1117-1124` buffered pool defaults | `:572-590` |
| `:1195-1204` `methods` (`api_key_paste` when `login === undefined`) | `:373-408` `pool_methods` |
| `:1206-1215` `runLogin` (`api_key_paste` → paste prompt, else login dialog) | `:359-371` `run_login` |
| `src/multilogin.ts:342-356` `promptApiKeyCredential` (`Enter {name}:`, trim, `API key is required`) | `src/commands/login.rs:339-357` |
| `src/multilogin.ts:336-340`, `:361-398` `loginCredential`/`showLoginDialog` | `src/commands/login.rs:307-336` + `native/login.rs:108` |
| `:1211`, `:1222` `openPoolManager` loop | `src/commands/login.rs:592-604` + `native/command_port.rs:799-812` |
| `:1212-1241` reauthenticate (`Reauthenticate {label}`, replace, reset health, notice) | `src/commands/login.rs:417-485` |
| `:1242-1275` add (`Account label:`, default label, `addAccount`, notice) | `src/commands/login.rs:486-550` |

The paste prompt is deliberately a plain input, exactly like Pi:
`src/commands/login.rs:352` passes `masked = false`, matching
`ctx.ui.input(\`Enter ${name}:\`, '')` (`src/multilogin.ts:350`), and
`ExtensionUIContext.input` has no masking option at all
(`node_modules/@earendil-works/pi-coding-agent/dist/core/extensions/types.d.ts:78`).
Only the login dialog's own `secret` interactions are masked.

## Tests

`packages/rpi-multiprovider/tests/m_native.rs` (production C entrypoint, fake host
vtable):

- `:2403` `multilogin_adds_an_account_through_a_private_pending_login` — the host
  login is addressed to `storeId: "multiprovider"` and a `pending-login-*`
  account; the masked `secret` interaction is presented as
  `Add example account · Enter the key`; the committed account is stored through
  `add` with the trimmed label; the reservation no longer resolves.
- `:2479` `multilogin_reauthenticates_a_stored_account_through_a_pending_login` —
  the stored account is untouched until the command's own `replace`, which stores
  the new credential under the existing id and label; exactly one account exists.
- `:2549` `multilogin_pastes_a_key_when_the_host_reports_no_interactive_login` —
  `interactiveLogin: false` yields the `Enter API key:` input (unmasked), the
  key is trimmed, **no** `auth` request is made, and the pool still gains the
  account.
- `:2596` `cancelled_multilogin_cancels_the_host_login_and_stores_nothing` — a
  cancelled presentation sends `cancelLogin` for the host's ticket, sends no
  `continue`, stores nothing, and drops the reservation.
- `:2636` `switch_account_never_reads_login_methods` — `/switch-account` issues
  no `auth` request at all, so the new metadata fetch cannot regress it.

Unit tests in `src/native/login.rs`: `:260`
`host_methods_become_pi_auth_presentation`, `:283`
`older_hosts_without_presentation_keep_the_provider_login`, `:296`
`malformed_method_lists_are_refused`.

## Gates

All executions were offline, foreground and bounded (`timeout 270 …; echo exit=$?`);
no background jobs, no network. Environment: `source
/home/user/Projects/.parity-env.sh` with `RUSTUP_HOME=/home/user/.rustup`,
`CARGO_HOME=/home/user/.cargo`, `CARGO_NET_OFFLINE=true`,
`CARGO_BUILD_JOBS=6`. Logs: `/tmp/ml-logs`.

| Gate | Command | Result |
| --- | --- | --- |
| 1.95 lib (fresh, so the `m_native` cdylib test loads this build) | `CARGO_TARGET_DIR=/home/user/Projects/.targets/pkg-int-ml-195 cargo +1.95.0 build -p rpi-multiprovider --lib --locked --offline` | ok (`build-195-lib.log`) |
| 1.95 cohort | `CARGO_TARGET_DIR=…/pkg-int-ml-195 RPI_TEST_MULTIPROVIDER_LIBRARY=$CARGO_TARGET_DIR/debug/librpi_multiprovider.so cargo +1.95.0 test -p rpi-multiprovider --locked --offline --no-fail-fast -- --skip real_dll` | **107 / 0 / 2** over 21 suites (`test-195.log`) |
| 1.78 lib with `-Dwarnings` | `CARGO_TARGET_DIR=/home/user/Projects/.targets/ml-178 RUSTFLAGS=-Dwarnings cargo +1.78.0 build -p rpi-multiprovider --lib --locked --offline` | ok (`build-178-lib.log`) |
| 1.78 cohort | same test command on `+1.78.0` with `CARGO_TARGET_DIR=…/ml-178` and `RPI_TEST_MULTIPROVIDER_LIBRARY=$CARGO_TARGET_DIR/debug/librpi_multiprovider.so` | **107 / 0 / 2** over 21 suites (`test-178.log`) |

Counts are passed / failed / ignored. The pre-change baseline for the same scope
was 105 / 0 / 2, i.e. this lane adds exactly the two new `m_native` tests;
`tests/m_native.rs` itself is 38 / 0 / 0 in both cohorts. The 2 ignores are
pre-existing skips, and `real_dll` (the real-host suite) is out of scope here
because this lane has no real-host run.

`rustfmt` (`rustfmt +1.95.0 --edition 2021 --check <file>`) is clean on every file
this lane changed: `src/native/login.rs`, `src/native/credentials.rs`,
`src/native/command_port.rs`, `src/native/commands.rs`, `tests/m_native.rs`, and
`src/native/mod.rs` itself. Formatting `src/native/mod.rs` also pulls in its
`mod services;` child, which reports 5 pre-existing diffs in
`src/native/services.rs`; that file is untouched by this lane and was not
reformatted.

`cargo +1.95.0 clippy -p rpi-multiprovider --all-targets --locked --offline`
reports 24 warnings + 1 error, **all pre-existing**: the error is
`clippy::not_unsafe_ptr_arg_deref` on the `rpi_plugin_sdk::export_plugin!` macro
expansion (`src/native/mod.rs:1276`, the same line at `b776e56:src/native/mod.rs:1267`),
and the warnings sit on unchanged code (`src/native/command_port.rs:66/516/524/799`,
`src/native/credentials.rs:83/364`, `src/native/mod.rs:761`, plus other modules that
predate this lane). None of them falls inside the added line ranges of this diff
(checked against `git diff -U0` hunk ranges), so this lane adds no new findings.

## Mutation checks

Each mutation was applied alone to the committed tree, killed by a focused test
(exit 101), then reverted (`git status` clean afterwards). Run with Rust 1.95.0 as
`cargo test -p rpi-multiprovider --test m_native <filter>` (the library unit tests
for the presentation ones). Logs under `/tmp/ml-logs`.

| Mutation | Killing test | Result |
| --- | --- | --- |
| `presentation`: drop the `interactiveLogin` gate (`login: … \|\| true`) | `native::login::tests::host_methods_become_pi_auth_presentation` (`src/native/login.rs:274`) | killed (`mut-a-lib.log`) |
| `run_login`: send `api_key_paste` into the login dialog (start a host login for a paste method) | `multilogin_pastes_a_key_when_the_host_reports_no_interactive_login` (`tests/m_native.rs:2567`) | killed (`mut-a2.log`) |
| `replace_credential`: keep the reservation branch but fall through to the store write | `multilogin_reauthenticates_a_stored_account_through_a_pending_login` (`tests/m_native.rs:2524`) | killed (`mut-b.log`) |
| `replace_credential`: reservation branch captures nothing | `multilogin_adds_…` (`tests/m_native.rs:2449`) and `multilogin_reauthenticates_…` (`:2524`) | killed (`mut-capture.log`) |
| `Reservation::drop`: leak the reservation instead of dropping it | `cancelled_multilogin_cancels_the_host_login_and_stores_nothing` (`assert_reservation_dropped`, `tests/m_native.rs:2399`) | killed (`mut-drop.log`) |
| `login`: do not send `cancelLogin` when the presentation fails | `cancelled_multilogin_cancels_the_host_login_and_stores_nothing` (`tests/m_native.rs:2623`) | killed (`mut-cancel.log`) |
| `parse_methods`: keep a blank `loginLabel` (not treated as absent) | `native::login::tests::older_hosts_without_presentation_keep_the_provider_login` (`src/native/login.rs:292`, "a blank label is absent") | killed (`mut-c.log`) |

The `presentation` gate mutation is only killed by the unit test, and that is
inherent to the fake host: the pool manager receives the chosen method id from the
`select` reply, so the `m_native` paste test pins the *routing* (`api_key_paste`
never becomes a host login) rather than the derivation of the method id from
`interactiveLogin`. `pool_methods`' `(paste)` label/value split is pinned by
`src/commands/login.rs` parity with `extensions/multiprovider.ts:1195-1204` and by
the library unit test above, not by a dialog-level assertion.

## Residual gaps

- **No real-host run.** Every executable claim above was produced against a fake
  host vtable. The `real_dll` suite (and the real `e9c0526`-generation host binary)
  predates the new `auth` `methods` presentation and is not part of this lane's
  evidence.
- **`activeAccountChanged` after a login commit is still unwired.** `/multilogin`
  reconciles and announces through the same paths `/switch-account` uses
  (`Port::reconcile`), but the cross-extension delivery of a *login* commit has no
  test of its own in this lane.
- **Env/`models.json` readiness is not reported by host status.** The login
  presentation has no readiness/`authStatus` decoration for env-provided or
  `models.json`-provided credentials, because the pinned host exposes no runtime
  auth-status port for them (the same limitation `m-commands-l1.md` records);
  `/multilogin` therefore lists a provider whenever the catalog has it.
