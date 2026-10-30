# Third-party notices

This repository ports four Pi extensions to native Rust plugins and carries
vendored crates, data derived from Unicode and Node.js, and pinned dependencies.
Each component keeps its own license. The MIT notices below are reproduced in the
files named in the table; those files are authoritative.

## Upstream ports

| Component | Upstream and copyright holder | Revision | License | Notice file |
| --- | --- | --- | --- | --- |
| rpi-antigravity-auth | [cortexkit/antigravity-auth](https://github.com/cortexkit/antigravity-auth), Copyright (c) 2025 Jens | `a97f9055017f3c1546afbfe61a19d50b428be3d8` | MIT | `packages/rpi-antigravity-auth/LICENSE.upstream` |
| rpi-anthropic-auth | [leohenon/pi-anthropic-oauth](https://github.com/leohenon/pi-anthropic-oauth) (0.3.6), Copyright (c) 2026 Leo Henon | `3709dcb7bce5de10670f3a8912c4e14327f93594` | MIT | `packages/rpi-anthropic-auth/LICENSE.upstream` |
| rpi-multiprovider | [monotykamary/pi-multiprovider](https://github.com/monotykamary/pi-multiprovider) (0.11.3), Copyright (c) 2026 pi-multiprovider contributors | `bd97d65a36f541935dc91e33ba15d7910a30f9d7` | MIT | `packages/rpi-multiprovider/LICENSE` |
| rpi-cursor-sdk, worker adapter | [fitchmultz/pi-cursor-sdk](https://github.com/fitchmultz/pi-cursor-sdk), Copyright (c) 2026 Mitch Fultz | `f337dc4bb7cea936c52e69a7fe08e7e376c8467b` | MIT | `packages/rpi-cursor-sdk/worker/LICENSE` and `packages/rpi-cursor-sdk/runtime/LICENSE` |

The Rust ports of anthropic-auth, antigravity-auth and cursor-sdk are additionally
covered by the MIT license in their package directories
(`packages/rpi-anthropic-auth/LICENSE`, `packages/rpi-antigravity-auth/LICENSE`
and `packages/rpi-cursor-sdk/LICENSE`), copyright Aiden Yoon.

The behavioral reference for all of them is Pi 1.3.0 by Mario Zechner
([earendil-works/pi](https://github.com/earendil-works/pi)), MIT licensed. It is
not part of this repository; the parity tests read it from a local checkout.

## Vendored Rust crates

The crates under `vendor/` are private integration copies. They keep their
original MIT notices. `vendor/README.md` and each `UPSTREAM.json` record the
revisions and the local changes.

| Crate or component | Upstream | Revision | License | Notice file |
| --- | --- | --- | --- | --- |
| cortexkit-claustrum-client 0.5.0 | [cortexkit/claustrum](https://github.com/cortexkit/claustrum), Copyright (c) 2026 Ufuk Altinok | `64fe590ab5f81c43c801d70421111107ff8f1cf5` | MIT | `vendor/claustrum-client/LICENSE` |
| subc-client-rs 0.30.5 | [cortexkit/subconscious](https://github.com/cortexkit/subconscious), Copyright (c) 2026 Ufuk Altinok | `3f9b1083257a0a0e27f6ff47e5c624bda190fbfe` | MIT | `vendor/subc-client-rs/LICENSE` |
| subc-os 0.1.16 | [cortexkit/subconscious](https://github.com/cortexkit/subconscious), Copyright (c) 2026 Ufuk Altinok | `668dfbde461d30aa63580dd032c91cf3f1b75f5f` | MIT | `vendor/subc-os/LICENSE` |
| private-files native 0.2.1 | [cortexkit/subconscious](https://github.com/cortexkit/subconscious), Copyright (c) 2026 Ex Machina | `3f9b1083257a0a0e27f6ff47e5c624bda190fbfe` | MIT | `vendor/private-files/LICENSE` |
| private-files legacy native 0.1.0 | [cortexkit/subconscious](https://github.com/cortexkit/subconscious), Copyright (c) 2026 Ex Machina | `eeab1627627b8ce7498b0bc633bdba83ea343fa4` | MIT | `vendor/private-files/legacy/LICENSE` |

## Data files

| File or dependency | Origin | License | Notice file |
| --- | --- | --- | --- |
| `crates/rpi-node-collation/data/icu78-collation.postcard` | Generated from Unicode CLDR 48 and ICU 78.3 data, as used by Node.js 24.22.0 (see `crates/rpi-node-collation/data/README.md`) | Unicode License V3 | `crates/rpi-node-collation/data/UNICODE-LICENSE`, `crates/rpi-node-collation/data/CLDR-LICENSE` |
| ICU4X 1.5 crates (`icu_collator`, `icu_provider_blob`, `icu_provider_adapters`, `icu_normalizer`) | [unicode-org/icu4x](https://github.com/unicode-org/icu4x), from crates.io | Unicode-3.0 | Shipped with each crate |
| `packages/rpi-antigravity-auth/data/node24-roots.pem` | The certificate list bundled with Node.js 24.22.0. Node takes its root store from Mozilla's NSS program, which Mozilla distributes under the Mozilla Public License 2.0 | Node.js and Mozilla terms | `packages/rpi-antigravity-auth/data/NODE-LICENSE` |

## Pinned host SDK

The four plugins compile against the rpi plugin SDK, `rpi-plugin-sdk` (and
`rpi-ai` and `rpi-telemetry` through it), from the pi-rust fork at
[yansigit/pi-rust](https://github.com/yansigit/pi-rust) revision
`6064a7616217ebb2d90014787bf4f45ce9e597dc`. The SDK is a git dependency and is not
vendored. The fork's `LICENSE` is MIT, Copyright (c) 2025 Mario Zechner.

## Cursor SDK and npm dependencies (not redistributed)

| Package | License | Handling |
| --- | --- | --- |
| `@cursor/sdk` 1.0.37 and its platform packages | Anysphere terms, not MIT | Never copied into this repository or a release archive. Installed from npm by the user under their own Cursor terms. See `packages/rpi-cursor-sdk/docs/licensing.md`. |
| `@earendil-works/pi-*` 1.3.0 peers | MIT | Installed from npm through `packages/rpi-cursor-sdk/runtime/package-lock.json`. Not vendored. |
| Other npm dependencies of `worker/` and `runtime/` | Their own licenses, as published | Installed from npm from the lock files. Not vendored. |

## Rust dependencies

Every other Rust dependency is a crates.io or git crate under its own published
license. `Cargo.lock` records the exact versions. Their license texts ship with
the crate sources in the Cargo registry.
