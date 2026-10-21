# Antigravity parity ledger

Reference: `cortexkit/antigravity-auth@a97f9055017f3c1546afbfe61a19d50b428be3d8`.
Provider identity is `google`. Source behavior takes precedence over README text.
The library loads on Linux; the package is not parity complete or ready for release.

The complete source/test declaration inventory is maintained in the host fork at
`docs/provider-parity/inventory.jsonl`; its conservative Antigravity import closure is recorded there; generated test
declarations and complete acceptance coverage still require review. This table assigns the Pi-facing behavior to concrete
port and acceptance files before implementation. Each row remains pending until
all its source behavior and tests have passing evidence.

| Row | Upstream source and associated tests | Rust port | Acceptance | Status |
| --- | --- | --- | --- | --- |
| A01 | Pi `index.ts`, core `model-registry.ts`, `transform/model-resolver.ts`, Pi `stream.ts` model resolution; each colocated test file | `src/catalog.rs` | `tests/catalog.rs`, generated pinned-source catalog/routing fixture, loaded-host explicit replacement | catalog and 20,376 routing cases passed on Rust 1.78; loaded CLI replacement passed on Linux |
| A02 | Pi `stream.ts` retired-route preflight and admission order | `src/catalog.rs`, `src/lib.rs`, `src/stream.rs` | retired aliases through loaded provider with credential/network call counters | pure source admission ordering and loaded retired preflight passed; complete credential/network counter acceptance pending |
| A03 | Pi `index.ts` OAuth, `credential-cache.ts`; core `antigravity/oauth.ts`, `auth.ts`, `project.ts` and reachable helpers | `src/auth.rs`, `src/project.rs` | scripted OAuth/project responses, callback/code inputs, packed refresh preservation, cache and custody refusal | 56 exchange/refresh cases, four Pi 1.3.0 login sequences and 30 project onboarding/cache scenarios match; admission/cancellation checkpoints, PKCE, expiry and four-entry cache passed; loaded method discovery/API-key login/logout and explicit refresh refusal passed; loaded OAuth and malformed JSON diagnostics pending |
| A04 | Pi `convert.ts`; core generation/model/signature helpers reachable from Pi | `src/convert.rs`, `src/schema.rs`, `src/metadata.rs`, `src/request.rs` | exact request/history/schema/signature fixtures including cross-model transitions | 855 conversion traces; 540 metadata, 240 generation and 90 finalization cases; session identity/expiry/eviction traces; 59 complete request bodies matched on Rust 1.78; local driver and native ownership connected; successful loaded HTTP acceptance pending |
| A05 | Pi `stream.ts`; core `agy-transport.ts`, request metadata, deadlines and redaction | `src/stream.rs`, `src/sse.rs`, `src/redaction.rs`, `src/transport.rs`, `src/transport/` | raw HTTP/1.1 bytes, SSE framing/late usage/partial output/cancellation/error comparisons | 65 provider cases/303 emitted events, 25 byte-boundary cases, 24 usage updates and 240 redaction cases passed on Rust 1.78; 66 serialized requests/proxy choices, 64 user-agent variants, 16 response/body cases and 15 response errors match Node 24; real TLS/proxy, bounded gzip and cancellation tests pass; pending-read exception, trust/OS diagnostics, custody response wrappers pending; timed driver, 30 ordinary fetch cases and seven local source cases passed |
| A06 | Pi `vault-setup.ts`, `vault-source.ts`; core enrollment authority and embedded Claustrum/common-auth | `src/vault/` | producer enrollment/rotation/approval/roster fixtures and scoped credential requests | enrolled wire/connection and pure state/authority boundaries pass; authenticated TCP, 168 real-filesystem ceremony scenarios, 38 lease scenarios, private-file and fsync-failure comparisons are recorded in tests/evidence.json; Windows cross-check passes on Rust 1.78, but platform execution, full storage fault qualification, 1,247 roster projections, 458 decodes and 19 real storage/discovery traces match; enrollment UI and watch/consumer integration remain pending |
| A07 | Pi vault setup and core common-auth Pi credential slot | `src/vault/slot.rs`, host credential transition port | `tests/vault_slot.rs`, `tests/vault_fsync.rs`, loaded host callback tests | 66 byte-preservation/refusal/crash scenarios and five actual file/directory fsync failure boundaries match; native custody startup, full lock-boundary qualification and platform execution remain pending |
| A08 | Pi vault send and core vault account source | `src/vault/custody.rs`, `src/vault/send.rs` | `tests/vault_custody.rs`; fresh credential/project per physical send, receipt/version-aware 401 retry and failure reporting | 40 custody state-machine traces match original results/calls/logs/timers, including cancellation and concurrent reporting; roster projection/persistence fixtures pass; consumer/watch, physical-send retry and native production wiring pending |
| A09 | Pi `commands.ts`, `paths.ts`; embedded shared command menus | `src/commands.rs` | complete scripted menu choices, exact messages, configuration mutations and notifications | pending |

Existing Pi vault enrollments and source credentials remain untouched. Imports
require fresh enrollment and cannot turn a marker or dormant backup into a login.
Live acceptance is restricted to the user's authorized Gemini 3.8 Flash model.

Proposed exception (not agreed): `tests/fixtures/network.json` records the original
Node 24 body read remaining pending after an idle timeout or cancellation. The
candidate returns the timeout/cancellation error and releases its request. This
row cannot pass until the behavior decision is resolved and integration is tested.
