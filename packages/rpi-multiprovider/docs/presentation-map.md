# Presentation map: `/multilogin`, `/multilogout`, `/accounts`, `/switch-account`

How each Pi UI call made by the upstream command flows is projected to a native
dialog control. The native contract is `pi-rust/docs/provider-parity/native-dialogs.md`;
the projection of `select`, `confirm`, `input` and `notify` is the one used by
`rpi-antigravity-auth/scripts/native-menu-reference.ts`. The differential
reference is `tests/commands-reference.ts`; its trace is the source of truth.

## Primitive projections

| Pi call | Native control or notification | Reply handling |
| --- | --- | --- |
| `ctx.ui.select(title, labels)` | `{"kind":"select","title":T,"choices":[{"label":L,"value":"<index>"}]}` | `values[0]` is the index. The flow takes `labels[index]` and resolves it to the first position with that label (`labels.indexOf`). A `null` reply is a cancel. |
| `ctx.ui.confirm(title, message)` | `{"kind":"confirm","title":T,"description":M}` | `confirmed === true` confirms; a `null` reply is a decline. |
| `ctx.ui.input(title, placeholder)` | `{"kind":"input","title":T,"placeholder":P,"masked":false}` | `value` is the text; a `null` reply is a cancel. |
| `ctx.ui.notify(text, kind)` | `notify` with the same `text` and `kind` (`info`, `warning`, `error`) | none |

Without interactive UI (`hasUI: false`) Pi's UI is a no-op: `select`, `confirm`
and `input` return nothing without showing a control, and `notify` is dropped.
The helpers in `src/commands/port.rs` follow that: they skip the control and
return the empty answer.

`/multilogin` and `/multilogout` reject non-interactive use with a warning
before anything is shown; both require `ctx.mode === "tui"`. `/switch-account`
requires only `hasUI`. `/accounts` has no up-front check and only calls `select`
for its listing.

## `/multilogin`

| Upstream step | Projection |
| --- | --- |
| `selectLogin`, several options | `ctx.ui.custom(OAuthSelectorComponent)` becomes one select control: title `Select provider to configure:`, `searchable: true`, and `query` set to the lowercased provider reference when one was given. Each choice is one provider and auth type, labelled `<name>[ [<type>]]<status>`. `[<type>]` appears only when the options mix subscription and API-key methods. The status suffix is ` ✓ configured`, ` ✓ env: NAME`, ` ✓ <source>`, ` • not configured` or ` • <type> configured`. The reply is an index into the option list. |
| `selectLogin`, one option | No control; the option is used directly. |
| `selectLogin`, no option | `warning`: `No providers with API-key or OAuth authentication are available.` or `No API-key or OAuth authentication method is available for "<ref>".` |
| Pool manager (`openPoolManager`) | Owned by the pool manager lane through `Port::pool_manager`. Its methods are `{label, value}` pairs: `oauth` (label `loginLabel` or name), `api_key` (interactive) or `api_key_paste` (label suffixed ` (paste)`). |
| `showLoginDialog` | Custom login dialog; see the next section. |
| `promptApiKeyCredential` | `input` titled `Enter <key name>:` with an empty placeholder. A blank reply is the error `API key is required`. |
| Account label | `input` titled `Account label:` with placeholder `<provider name> <N>`, where N is the stored account count plus one. A blank reply uses the placeholder. |
| Save or reauthenticate result | `info`: `Added <label> to <name>. Credentials saved to <path>` (or `Reauthenticated ...`). Failures are `error` notifications. |

### Login dialog (`LoginDialogHostComponent`, `showLoginDialog`)

Pi shows one custom component whose title is `Add <name> account`, or the
caller's title such as `Reauthenticate <label>`. The native equivalent is a
sequence of controls and notifications, each carrying that title as a prefix:

| Pi interaction | Native projection |
| --- | --- |
| Dialog open | No control; the title is carried on every following item. |
| `auth_url` event | `info`: `<title> · Open <url>`, plus ` · <instructions>` when given. |
| `device_code` event | `info`: `<title> · Open <uri> and enter code <code>`, then `info`: `<title> · Waiting for authentication...`. |
| `info` event | `info`: `<title> · <message>`, with ` · <label>: <url>` (or ` · <url>`) for each link. |
| `progress` event | `info`: `<title> · <message>`. |
| `select` prompt | Select control titled `<title> · <message>`; choices are the option labels. The reply resolves to the id of the first option with the chosen label. |
| `manual_code` prompt | Input control titled `<title> · <message>` with an empty placeholder. |
| `text` prompt | Input control titled `<title> · <message>` with the prompt placeholder. |
| `secret` prompt | Input control titled `<title> · <message>` with `masked: true`. |
| Cancel on any prompt, or dialog close | A `null` reply. The login fails with `Login cancelled`, which the flow turns into "no result" with no notification. |

Pi keeps progress and waiting text in a live dialog. A native client has no
equivalent persistent panel, so each status change is a separate `info`
notification. This is a presentation difference, not a change of outcome.

When `selectLogin` finds exactly one option, no selector is shown. A provider
argument matches by id or display name, case-insensitively.

## `/multilogout`

| Upstream step | Projection |
| --- | --- |
| No pools | `info`: `No multilogin accounts are stored.` |
| Provider from argument | Matched case-insensitively by provider id; falls back to the selector when nothing matches. |
| Provider selector | Select titled `Select provider to remove an account from:`; labels `<name> (<account count>)`. |
| Account selector | Select titled `Select account to remove:`; labels `<account label> · <auth kind>`. |
| Confirmation | Confirm titled `Remove pooled account?`, description `Remove <label> from <provider id>? Pi's normal /login credential is unchanged.` |
| Removed | `info`: `Removed <label> from <provider id>.` |

A selected label resolves to the first position with that label, so repeated
labels remove the first matching account, as the upstream `indexOf` does.

## `/accounts`

One select titled `Provider Accounts` lists the status lines. Each provider
gives a header `<provider> (<id>) · <policy>[ · main-first] · affinity on|off`,
followed by one line per account, `  <label> (<auth kind>) · <status> · w<weight> · p<priority> · <n> in flight · <n> failures[ · cooldown until <time>]`,
or `  no accounts`. The reply is ignored. With no pools, one `info`:
`No account pools are configured. Use /multilogin to add one.`

## `/switch-account`

| Upstream step | Projection |
| --- | --- |
| Argument matches a label exactly, or uniquely by prefix | Chosen directly. |
| Ambiguous prefix | `warning`: `Multiple accounts match "<ref>". Pick one below.`, then the picker. |
| `auto` or `automatic` with no match | Automatic; no picker. |
| No match | `warning`: `No pooled account for <provider> matches "<ref>". Pick one below.`, then the picker. |
| Picker | Select titled `Switch <provider> account:`. The first choice is `Automatic · let the <policy> strategy pick the next account`; the rest are `<label> (<kind>) · <status> · w<weight> · p<priority>[ · current]`. Repeated labels gain ` · <first 8 characters of the account id>`. |
| Automatic result | `info`: `Cleared this session's pinned account. ...` when the pool keeps affinity, otherwise `Selection for this session is already automatic.` |
| Pin result | `info`: `Switched to <label> for this session. ...`, with a cooldown sentence when the account is cooling down. |
| Disabled account | `error`: `Account "<label>" is disabled. Enable it in /multilogin first.` |

Virtual pools are labelled `<virtual provider label> · <model name>`.

## Not expressible through `Ui` + port

- Pi's `ctx.ui.custom` components are not native controls. Each is projected as
  the control sequence above. The login dialog's live status panel becomes
  notifications, and a cancel is a `null` reply rather than a keypress.
- The pool manager screen (`openPoolManager`) is outside this lane. Its result
  is the `PoolAction` returned by `Port::pool_manager`.
