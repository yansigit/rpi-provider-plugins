# Antigravity parity ledger

Reference: `cortexkit/antigravity-auth@a97f9055017f3c1546afbfe61a19d50b428be3d8`.
Provider identity is `google`. Source behavior takes precedence over README text.
The package is not installable or parity complete yet.

The complete source/test declaration inventory is maintained in the host fork at
`docs/provider-parity/inventory.jsonl`; its Antigravity shared-code reachability
review remains pending. This table assigns the Pi-facing behavior to concrete
port and acceptance files before implementation. Each row remains pending until
all its source behavior and tests have passing evidence.

| Row | Upstream source and associated tests | Rust port | Acceptance | Status |
| --- | --- | --- | --- | --- |
| A01 | Pi `index.ts`, core `model-registry.ts`, `transform/model-resolver.ts`, Pi `stream.ts` model resolution; each colocated test file | `src/catalog.rs` | `tests/catalog.rs`, generated pinned-source catalog/routing fixture, loaded-host explicit replacement | catalog and 20,376 routing cases passed on Rust 1.78; host replacement pending |
| A02 | Pi `stream.ts` retired-route preflight and admission order | `src/catalog.rs`, `src/lib.rs`, `src/stream.rs` | retired aliases through loaded provider with credential/network call counters | pending |
| A03 | Pi `index.ts` OAuth, `credential-cache.ts`; core `antigravity/oauth.ts`, `auth.ts`, `project.ts` and reachable helpers | `src/auth.rs`, `src/project.rs`, `src/cache.rs` | scripted OAuth/project responses, callback/code inputs, packed refresh preservation, cache and custody refusal | pending |
| A04 | Pi `convert.ts`; core generation/model/signature helpers reachable from Pi | `src/convert.rs`, `src/schema.rs`, `src/metadata.rs` | exact request/history/schema/signature fixtures including cross-model transitions | 855 request/schema reference traces passed, including field order, on Rust 1.78; generation/metadata and complete sends pending |
| A05 | Pi `stream.ts`; core `agy-transport.ts`, request metadata, deadlines and redaction | `src/stream.rs`, `src/transport.rs` | raw HTTP/1.1 bytes, SSE framing/late usage/partial output/cancellation/error comparisons | pending |
| A06 | Pi `vault-setup.ts`, `vault-source.ts`; core enrollment authority and embedded Claustrum/common-auth | `src/vault/` | producer enrollment/rotation/approval/roster fixtures and scoped credential requests | pending |
| A07 | Pi vault setup and core common-auth Pi credential slot | `src/vault/custody.rs` | backup/marker ordering, identity/refusal, concurrent credential replacement and conditional restore | pending |
| A08 | Pi vault send and core vault account source | `src/vault/send.rs` | fresh credential/project per physical send, receipt/version-aware 401 retry and failure reporting | pending |
| A09 | Pi `commands.ts`, `paths.ts`; embedded shared command menus | `src/commands.rs` | complete scripted menu choices, exact messages, configuration mutations and notifications | pending |

Existing Pi vault enrollments and source credentials remain untouched. Imports
require fresh enrollment and cannot turn a marker or dormant backup into a login.
Live acceptance is restricted to the user's authorized Gemini 3.8 Flash model.
