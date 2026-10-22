# Presentation map (lane m-cmd-a2): pool manager, settings primitives, `/vprovider`

How each Pi UI call made by the upstream pool manager (`extensions/pool-manager.ts`),
the settings-view primitives (`extensions/settings-ui.ts`) and the `/vprovider`
editor (`extensions/multiprovider.ts`) is projected to a native dialog control.
The native contract is `pi-rust/docs/provider-parity/native-dialogs.md`.

> Merge note: lane m-cmd-a1 wrote `docs/presentation-map.md` for
> `/multilogin`, `/multilogout`, `/accounts` and `/switch-account` on the same
> branch base. This file is the m-cmd-a2 half; the two should be merged into one
> `docs/presentation-map.md` (the primitive table below is compatible with the
> one there). Nothing here overrides m-cmd-a1's projections.

The differential reference is `tests/commands-a2-reference.ts`; its trace is the
source of truth. The Rust replay is `tests/commands_a2.rs`.

## Primitive projections

| Upstream | Native control | Reply handling |
| --- | --- | --- |
| `SettingsList` screen (root or `SectionSubmenu`) | `{"kind":"settings","title":T,"description":D,"searchable":true,"settings":rows,"query":Q,"selected":ID}` | `{"type":"change","id":ID,"value":V,"query":Q}` cycles a row, `{"type":"open","id":ID,"query":Q}` opens its submenu, `null` cancels the screen. |
| `SelectSubmenu` (`SelectList`) | `{"kind":"select","title":T,"description":D,"choices":[{"value":V,"label":L}],"selected":V}` | `{"type":"select","values":[V]}`; `null` cancels. The upstream `SelectList` has no search box, so the control is not `searchable`. |
| `IntegerInputSubmenu` | `{"kind":"input","title":T,"description":D,"initialValue":C,"trim":true,"integerMin":M}` | `{"type":"input","value":V}`; `null` cancels. The host trims, requires ASCII digits, the safe-integer range and `V >= M`, and answers with the normalized decimal. |
| `StringInputSubmenu` | `{"kind":"input","title":T,"description":D,"initialValue":C,"trim":true}` | `{"type":"input","value":V}` (trimmed); `null` cancels. |
| `ctx.ui.confirm` (virtual provider removal) | `{"kind":"confirm","title":T,"description":M}` | `{"type":"confirm","confirmed":B}`; `null` declines. |
| `ctx.ui.notify` | `notify` with the same text and kind | none. Without an attached UI Pi drops notifications and the port shows nothing. |

A settings row carries `id`, `label`, `value`, an optional `description`, and
either `values` (inline cycling) or `submenu: true`. `markDrillIn` is preserved:
rows that open a submenu end their label with ` ›`. Theme color codes are
dropped, so labels, values and descriptions are plain text (upstream's backend
rows and dialog titles are wrapped in `theme.fg`).

`formatMs` keeps JavaScript's number-to-string formatting (`100ms`, `1.5s`,
`1.5m`, `1.5h`), and `BOOLEANS` is `['true','false']`.

## Pool manager screen

`openPoolManager(ctx, provider, callbacks, methods)` becomes one control per
screen. The root screen is `"<provider name> pool"` with the upstream subtitle
(`N stored account(s) · changes save immediately`, `No pool yet · upstream
credential detected · settings apply once the first account is added`, or
`No pool yet · settings apply once the first account is added`).

| Upstream step | Projection |
| --- | --- |
| Root rows | `pool.policy` (cycles `round-robin`, `weighted-round-robin`, `least-inflight`, `priority`), `pool.affinity` (cycles `true`/`false`), `accounts ›`, `scheduler ›`. |
| `accounts` submenu | `"<provider name> accounts"`, rows: `upstream ›` when the pool exists or an upstream credential is configured, one `stored.<account id> ›` per stored account, `accounts.none` when there are none, and `accounts.add ›` when the caller supplied login methods. |
| `upstream` submenu | `"<label>"` (`Pi default` when unset), rows `upstream.enabled` (cycle), `upstream.label` (input), `upstream.weight` (integer input, min 1), `upstream.priority` (integer input, min 0). |
| `stored.<id>` submenu | `"<label>"`, rows `account.<id>.label` (input), `.enabled` (cycle), `.weight` (integer input, min 1), `.priority` (integer input, min 0), `.reauth ›` when methods exist, `.remove ›`. |
| `scheduler` submenu | `"Scheduler cooldowns"`, the five cooldown rows and `scheduler.errorsBeforeSwitch`, each a select of `Default (<formatMs>)` plus the upstream option lists. |
| `account.<id>.reauth` | select titled `Reauthenticate <label>`; the highlighted option is `preferredAuthMethod` (`oauth` for an oauth account, `api_key` for an API-key account, `api_key_paste` when the account is an API-key account and only paste is available, otherwise the first method). The reply value is wrapped as `__reauth__:<value>` and ends the screen with `{type:'reauth', accountId, method}`. |
| `account.<id>.remove` | select titled `Remove <label>?` with `Cancel`/`Remove <label>`. `remove` answers `__remove__` and deletes the credential; `keep` closes the submenu without a change. |
| `accounts.add` | select titled `Add <provider name> account`; the reply value is wrapped as `__add__:<value>` and ends the screen with `{type:'add', method}`. |
| Row label suffix | Backend/upstream summaries are formatted exactly as upstream: `weight 3 · priority 0`, `disabled`, `1 upstream · 2 stored, 1 disabled`, `45s rate-limit · 15m quota · 5m auth · 3 errors/switch`. |
| Change dispatch | `pool.policy`/`pool.affinity` → `updatePool`; `upstream.*` → `updatePool` with `mergeUpstream` applied; `scheduler.<key>` (only the six known keys) → `updateSchedulerSettings` plus `service.updateSchedulerDefaults`; `account.<id>.*` → `updateAccount`/`removeAccount`. An empty account label notifies `Account label must not be empty.` (`error`) and stores nothing. |
| After a change | The state is reloaded (one `pool` read, one `getScheduler` read) and the **root** screen is rebuilt: exactly `component.refresh(selectAfterFor(id))`. `selected` is `selectAfterFor(id)` when that id is a root row, otherwise the first row. |
| After a submenu cancel | The parent screen is re-shown with the query it had and the opened row selected. |
| Buffer | While no pool is stored, `updatePool` accumulates the policy/affinity/includeUpstream/upstream in the caller's buffer; `loadState` overwrites the buffer from the stored pool once one exists. This is what `/multilogin` later writes into the first added account. |
| Cancel at the root | `{type:'closed'}`. |

Two Port additions are needed to drive this from the login flow: `Port` already
covers the store, `reconcile` and the runtime view; the pool manager callbacks
also call `service.updateSchedulerDefaults(effective)`, which this lane declares
as `commands::pool_manager::PoolDefaults::update_scheduler_defaults`.

## `/vprovider`

`pi.registerCommand('vprovider', …)` requires `hasUI` and `mode === 'tui'`,
otherwise it notifies `/vprovider requires Pi interactive mode.` (`warning`).
Every editor page is one native control:

| Upstream page | Projection |
| --- | --- |
| Root list | `settings` titled `Virtual providers`; rows `create`, `edit-<id>`, `delete-<id>`. |
| Editor menu | `settings` titled `Virtual provider "<id>"`; rows `model-id`, `add`, `sep-top`, one `backend-<n>` row per `draft.models[0].backends` entry (`1. <providerId> · <modelId> · enabled · w1`), `sep-bottom`, `save`, `discard`. |
| Provider picker | `settings` titled `Backing provider`; rows `<name> (<id>)` for each candidate (providers with models, excluding virtual providers, sorted by name with Node collation). |
| Model picker | `settings` titled `Backing model for <name>`; rows keyed by model id with the model name as the value. |
| Backend actions | `settings` titled `<providerId> · <modelId>`; rows `toggle` (`Enable`/`Disable`), `weight`, `remove`. |
| Provider id / model id / weight input | `input` titled `Virtual provider id`, `Virtual model id (shown in /model)` or `Set weight`, with `initialValue` and `trim: true` (`integerMin: 1` for weight). |
| Page error | The upstream dialog renders `pageError` as an extra warning line. Native controls have no inline error, so it is appended to the control's `description` after a newline. |
| Removal confirmation | `confirm` titled `Remove virtual provider?` with `Remove <id>? Backing providers and their pooled accounts are untouched.` |
| Saved/created | `info`: `<Saved|Created> virtual provider "<id>". Select "<modelId>" on provider "<id>" in /model.` (`Saved` when the id was already stored). |
| Removed | `info`: `Removed virtual provider "<id>".` |

Upstream quirks preserved:

- The backend **`Set weight` row is unreachable**: its branch opens the weight
  input and the trailing `goTo({kind:'menu'})` in the same handler immediately
  overrides it, so the editor returns to the menu. The port keeps the branch
  (`InputPurpose::Weight`, allowed dead code) and the observable behavior.
- The editor only ever touches `draft.models[0]`: additional stored models are
  carried through a save untouched and are not shown in the menu.
- Saving heals templates only for enabled backends that have none, resolving
  them from the candidate catalog with `captureVirtualModelTemplate`; the live
  resolution still wins at runtime.

Port additions for `/vprovider`: `Port` covers the store (`listVirtual`,
`saveVirtual`, `removeVirtual`), `reconcile` and the virtual-provider registry;
the editor also needs the provider model catalog (`uniqueProviders` /
`getModels`) and `ctx.modelRegistry.getProvider(id)`, declared here as
`commands::vprovider::VproviderSources::{providers_with_models, provider_registered}`.

## Fixture input

For the pool manager and `/vprovider` scenarios `input.steps` is the scripted
user interaction (one action per screen: `change`, `open`, `pick`, `submit`,
`cancel`, with an optional `search` that is typed into the host's search box
before the action). `input.answers` is the recorded native reply for every
control the flow shows, in order, followed by the scripted `ctx.ui.select` /
`confirm` / `input` answers; the Rust fake `Ui` replays them. A search text is
typed *after* the control is projected, so the control's `query` is what the
extension knew and the reply's `query` is what the host's search box held — the
extension re-sends the reply's query on the next screen, and the query is reset
by a change (upstream rebuilds the root list).
