# C06 cloud differential

Offline differential evidence for ledger C06: Cursor Cloud runtime selection,
first-use acknowledgement, repository/ref and environment restrictions, the
cloud lifecycle ledger (intent before mutation), explicit archive/delete with
confirmation, identifier persistence, failed sync / uncertain outcomes and the
no-automatic-replay rule.

## Sides

| Side | Script | What runs |
| --- | --- | --- |
| reference | `run-reference.mjs` | Unchanged Pi 1.3.0 CLI (`provider-references/integrated`) with the pinned `pi-cursor-sdk@0.5.3` adapter and the unchanged fake `@cursor/sdk`. |
| candidate | `run-rpi.mjs` | The retained rpi worker (`runtime/main.mjs`) over its private peer protocol, with an emulated native host that implements the durable journal port. |

Both sides consume the same `scenarios/*.json` and the same copied fake SDK in
`fake-sdk/` (see provenance below). Neither side touches the network.

`c_cloud.rs` loads `reference-evidence.json` and `evidence.json` with
`include_str!` and compares the COMPLETE `observed` object of every scenario:
`cloudCalls` (whole fake-SDK cloud records), `lifecycle` (ordered session
entries), `journal` (whole durable entries with only per-run session identities
placeholdered), `sdkCalls` and the refused-send `sendError`.

## Scenario coverage

| Scenario | Covers |
| --- | --- |
| `cloud-create-archive` | Cloud selection, repo + `startingRef`, pool environment, one turn, run report/PR branch, explicit archive, explicit `delete --yes`. |
| `cloud-ack-missing` | First-use acknowledgement gate; no SDK agent is created. |
| `cloud-repo-invalid` | HTTPS-only repository URL. |
| `cloud-branch-no-repo` | A starting ref requires an explicit repository. |
| `cloud-branch-invalid` | `refs/tags/*` and other unsupported refs are refused. |
| `cloud-env-forwarding` | Reserved env forwarding is refused. |
| `cloud-env-type-invalid` | Environment type must be `cloud`, `pool` or `machine`. |
| `cloud-env-type-required` | An environment name requires a type. |
| `cloud-env-repo-conflict` | A named `cloud` environment cannot take a repo. |
| `cloud-delete-without-yes` | Delete requires `--yes`; no mutation occurs. |
| `cloud-archive-unknown` | A syntactically valid but unrecorded agent is refused. |
| `cloud-archive-uncertain` | A failed archive sync leaves the durable intent pending and a retry is refused (no replay). |
| `cloud-list-no-replay` | Repeated list/archive never replays the archive/delete mutations. |

## Determinism

`RPI_CURSOR_FAKE_CLOCK=2026-01-01T00:00:00.000Z` freezes `Date` for both
processes, and the fake SDK generates agent ids from a counter
(`bc-00000000-0000-4000-8000-000000000001`, ...). Each script runs every
scenario twice and records `determinism.reference` / `determinism.candidate`.

## Commands

```sh
source /home/user/Projects/.parity-env.sh && lane_env c-cloud
node tests/c-cloud/run-reference.mjs --write     # then check mode: omit --write
node tests/c-cloud/run-rpi.mjs --write           # then check mode: omit --write
```

`run-rpi.mjs` needs the staged worker:

```sh
cd worker && npm ci --offline --ignore-scripts && npm run build
node tests/stage-runtime.mjs
(cd runtime && npm ci --offline --ignore-scripts)
```

## Fake SDK provenance and local extensions

`fake-sdk/` is a copy of `tests/fake-sdk` from the C-FAKE lane (`lane/c-fake`,
commit `2067a25`), read-only upstream. Two documented local extensions make C06
observable:

- `register.mjs`: optional `RPI_CURSOR_FAKE_CLOCK` freezes the wall clock.
- `sdk.mjs`: optional `cloudErrors.<archive|delete>` makes that SDK mutation log
  and count the call, then throw before mutating fake state (failed sync).

No other fake-SDK behaviour changed.

## Not tested

- Real Cursor cloud resources, billing or any network call.
- Interactive/TUI acknowledgement dialogs (only the non-interactive flag path).
- Prompt/image assembly (covered by the C01 prompt lane); `agent.send`
  conversation text and image bytes are projected out of `cloudCalls`.
- The compiled rpi host binary/cdylib; the candidate drives the packaged worker
  over its real peer protocol with an emulated host, including the native
  journal port. The Rust `JournalPort` byte-level contract is covered by
  `tests/journal_reference.rs`.
