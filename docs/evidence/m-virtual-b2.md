# M-VIRTUAL-B — native host-v2 virtual binding

Status: **provider build/test gates pass; inherited strict lint/format failures and M09 real post-turn compaction remain open**.
This report supersedes only the M06–M09 package-binding evidence; it does not
promote historical reports or edit the read-only host ledger. Machine-readable
hashes, SDK identities, counts and limitations: [`m-virtual-b2.json`](m-virtual-b2.json).

## Source and boundary

- Reviewed baseline commit `916fb385bf499be2607c133a4e382d634d648cac`,
  based on `10515f50bc9a9f7a5a958d00bfd166c1b8f0ff53`.
  Checkout location and branch are local provenance, not portable evidence.
- All three provider packages use host
  `8014e36cfc9923d404a76044ba0595dc4edc98e7`, unchanged Git URL. Cargo's existing
  URL cache was seeded from the local read-only host repository, not the network.
- Registry dependency versions/checksums are byte-for-byte equivalent to the base
  lockfile. The pin-only provider cohort compiled on Rust 1.78 before binding work.
- No host source changes; no old-lane changes, push, external requests, live
  credentials, or `~/.pi` / `~/.rpi` access. All test HOME/private TMP directories
  are isolated mode 0700. Fixture credentials are synthetic.

Review fixes:

- `4d2c0a8`: typed synthetic terminal identity and transport diagnostics (F1).
- `7d08414`: scope-local virtual pin restoration and missing-session rejection (F2/F3).
- F4 separates portable content from optional local provenance. Two original
  regenerations, a relocated detached checkout, and a differently named branch
  produced byte-identical JSON and Markdown.

## Binding decisions

`src/native/virtuals.rs` owns catalog effects, not a second retry scheduler. Each
execution scope has its own catalog/service. The existing `virtuals` and `lift`
state machines retain weighted/distribute/fallback selection, retry ordering,
initial buffering and terminal policy. Native vprovider commands use a narrow
port; unrelated login/switch flows are unchanged.

Virtual aliases have an owned streaming base that **fails closed**. They are
registered early enough for actual CLI model selection. Reconciliation requires
publication acknowledgement before trusting local metadata; lookup heals model
templates, updates backend metadata, and handles deletion/re-registration. A
removed backend or model retains the upstream unavailable error. An unready
session returns `multiprovider: session not ready` rather than the physical base.

The global wrapper recognizes an owned virtual alias and negotiates secure v2.
Each attempt resolves the registered target, mints its ambient receipt and
calls `delegate(target, Some(receipt))`. Source identity/auth, source `apiKey`
and `providerOptions` are not forwarded. Target wrappers run normally, including
physical target pool selection. Pool-account authentication remains inside that
target's wrapper, rather than manufacturing source credentials for a foreign
provider. Cross-provider delegation never falls back to v1.

Opaque target guards drain their producers and attempt receipt/target release on
success, retries, failed receipt/stream startup, cancellation and errors. Exact
upstream missing-terminal and exhaustion strings remain in the ported machines.
The native stream adapter now uses `Provider stream ended without a terminal event`
for a silent delegated stream as well. Synthetic terminals retain the resolved
model locally (never in the v2 envelope), deserialize as `rpi_ai::AssistantMessage`,
and preserve nonzero delegation diagnostics. Recorded and inherited virtual pins
use the current scope's catalog service; reconciliation and child shutdown retain
the parent's virtual pin. Missing session identity fails with the exact unready
session error before delegation.

The optional compaction adapter requires service `pi-fabric` version 1 **and**
explicit capability `multiprovider.failoverCompaction.v1`; a `fabric_exec` tool
name alone grants nothing. Calls use the selected invocation's native client.
Absent/incompatible service means ordinary inline failover. Per-scope workers
reuse the existing compaction policy, are cancelled on scope/session shutdown,
and are joined before runtime destruction. No root registration-client fallback
is used for delayed child work. This is a compatible fixture contract, not a
claim that a production Fabric extension is available.

## Gates (both package toolchains)

| Gate | Rust 1.78.0 (`-Dwarnings`) | Rust 1.95.0 |
|---|---:|---:|
| Offline provider cdylib build | pass | pass |
| Antigravity cohort | 97 passed / 0 failed / 5 inherited ignored | same |
| Cursor cohort | 62 / 0 / 17 inherited ignored | same |
| Multiprovider cohort | 92 / 0 / 2 inherited ignored | same |
| Total cohort | **251 / 0 / 24 inherited ignored** | **same** |
| Explicit MP native ABI tests | 24 / 0 / 0 | 24 / 0 / 0 |
| Actual-host MP DLL acceptance | 5 / 0 / 0 | 5 / 0 / 0 |
| Explicit Antigravity loading branch | 1 / 0 / 0 | 1 / 0 / 0 |

The explicit native tests are included in cohort counts, not additional tests.
All three doctest suites completed. No new ignores, passing artifact skips or
weakened assertions were introduced in the new acceptance suite. The existing
`r02_native_loading` harness explicitly executes the requested AG library branch;
its unrelated legacy-provider and Cursor-library branches still report their
pre-existing skips. Historical Cursor IPC evidence remains **8/8 on Node 24.22.0**
(not rerun for these native-only fixes).
Both MP cdylibs are loaded into the real host built from pinned 8014e36 with Rust
1.78 (same host executable for both package toolchains), using independently
compiled fixture target/service DLLs, isolated agent directories and local socket
synchronization. No sleeps are used as readiness synchronization.

Native virtual ABI regression names (9):

- `virtual_native_failover_releases_every_target_and_receipt`
- `virtual_native_unsupported_v2_never_downgrades`
- `virtual_native_missing_terminal_fails_and_releases`
- `virtual_native_publication_ack_healing_delete_and_reregister`
- `virtual_native_capable_service_compacts_and_shutdown_drains`
- `virtual_native_delayed_compaction_is_cancelled_and_drained_on_shutdown`
- `virtual_native_transport_failure_preserves_diagnostic_and_identity`
- `virtual_native_recorded_pin_is_scope_local_across_reconcile_and_shutdown`
- `virtual_native_missing_session_identity_fails_explicitly`

New actual-host acceptance names (5):

- `real_dll_cross_provider_dispatch_and_backend_failover`
- `real_dll_cross_provider_dispatch_reaches_target_pool`
- `real_dll_cancellation_attempts_release_after_host_revocation`
- `real_dll_virtual_cycle_rejection_surfaces`
- `real_dll_capable_fabric_shutdown_cancels_delayed_compaction`

The loader fixture audits actual continuation operations, opaque handle values,
target-auth requests and release results; the target fixture requires its own
synthetic credential and proves that no source identity/options crossed over.
The cancellation test requires one release attempt per actual handle and checks
the exact host revocation error; it does **not** claim the host accepted releases.

Existing upstream differential tests are preserved unchanged: 70 virtual and
96 lift scenarios cover backend/account traces, failover order, errors, terminal
boundaries and missing terminals. Both upstream extractors and the existing
command/Cursor asset fixture regenerators previously produced byte-identical outputs
twice. This review-fix pass reran the virtual/lift extractors twice, byte-identically.
No changes were made to the approved AG body-read timeout, Cursor snapshot,
presentation or import-storage exceptions.

### Mutation checks

The four original mutations below remain historical evidence. Three new review
mutations were applied individually, killed by assertions (exit 101), restored,
and followed by passing native/cohort tests on both toolchains:

| Guard mutated | Regression |
|---|---|
| Accept v1 instead of required v2 | `virtual_native_unsupported_v2_never_downgrades` |
| Omit receipt release in target guard | `virtual_native_failover_releases_every_target_and_receipt` |
| Forward source identity | `virtual_native_failover_releases_every_target_and_receipt` |
| Replace exact missing-terminal error (historical) | `virtual_native_missing_terminal_fails_and_releases` |
| F1: read fallback identity from the descriptor-free envelope | `virtual_native_missing_terminal_fails_and_releases` (typed deserialization fails) |
| F2: route virtual pins back to the ordinary service | `virtual_native_recorded_pin_is_scope_local_across_reconcile_and_shutdown` (selected backend differs) |
| F3: accept empty affinity | `virtual_native_missing_session_identity_fails_explicitly` (exact error absent) |

Portable JSON indexes logical log names. Optional local provenance records absolute
paths, artifact sizes and SHA-256 hashes separately; it is not committed or used
in portable freshness comparisons. Binary hashes and raw-log hashes are not portable
because compiler paths and log output vary across checkouts.

### Formatting and lint qualification

Rust 1.95 rustfmt passes for every changed Rust file, with `--edition 2021
--config skip_children=true` (including the `.rs.inc` regression file). Full
`cargo +1.95.0 fmt --all -- --check` already fails at the reviewed baseline due to
unrelated formatting debt; no unrelated files were reformatted. Rust 1.78 rustfmt
is not installed; no installation/network access was attempted.

Strict cohort `clippy --all-targets -- -D warnings` fails identically on Rust 1.95
at baseline and after fixes: Cursor 11, Antigravity 80 and Multiprovider 22 existing
library diagnostics. Rust 1.78 with `RUSTFLAGS=-Dwarnings` also fails on inherited
lints. Unqualified 1.95 clippy has the existing SDK `export_plugin!` macro's denied
`clippy::not_unsafe_ptr_arg_deref` in all three packages. Running 1.95 clippy with
only `-A clippy::not_unsafe_ptr_arg_deref` completes (existing warnings remain).
These failures are not represented as clean strict lint gates; no unrelated lint
fixes or source-level lint allowances were added.

## Ledger qualification and remaining host limitations

| Row | Package evidence now established | Not established |
|---|---|---|
| M06 | Owned registration, ack/healing/delete/re-register, native command binding, global wrapper and real DLL dispatch | No promotion of unrelated service-lane or typed-queue evidence |
| M07 | Template/model effects, backend failover, target wrappers and target pool selection; upstream differential traces | Exhaustive real-host weighted/nested catalog combinations are not separately replayed |
| M08 | Existing stream/retry machines reused; exact errors, native attempt/terminal effects, secure target/auth separation; successful exit/retry release and cycle rejection in real host | **Successful cancellation cleanup RPCs**: host revokes their continuation before package cleanup |
| M09 | Native explicit capability, absent-service inline fallback; successful compact/notice and cancel/drain at ABI seam; real-host scoped capability negotiation and delayed-work shutdown | **Successful real post-turn compaction**: ordinary invocation native lease expires after stream callback returns |

Two concrete host observations must not be mistaken for package-side proof:

1. Cancellation cleanup calls return exactly
   `inactive provider continuation` after host epoch revocation. The package
   attempts every release with a fresh cancellation token; there is no surviving
   target/receipt authority. The host may invalidate handles itself, but package
   tests do not inspect its internal maps or claim successful post-revocation RPCs.
2. A delayed service worker cannot keep using its invocation native client after
   the stream callback returns. The host `scope_lifetime` seam permits inspection
   and shutdown waiting, **not** service calls. The root registration lease has
   `identity: null`, so using it would launder selected-scope work; that approach
   was rejected. Production post-turn compaction requires an explicit durable
   scope-bound service/scheduling authority or compatible service-owned task API.
   No such host change was made or assumed here. M09 remains partial.

These are limitations of current lease/cancellation semantics, not demonstrated
host memory-safety bugs. No host-side fix is included. Linux acceptance only;
no real Fabric extension, live provider, typed bounded queue load or full-workspace
Rust-1.78 gate is claimed.

## Reproduce offline

Use the prepared local Cargo cache and the pinned host executable. Each cargo
command must run in the foreground with `timeout 270 ...; echo exit=$?`; retry an
interrupted compile until it completes, never treat a timeout as a successful gate.
Do not run the entire workspace on 1.78.

```bash
cd "${LANE_CHECKOUT:?set to this checkout}"
source "${PARITY_ENV:?set to the prepared offline parity-env.sh}"
mkdir -p /tmp/m-virtual-b2/{home,tmp,logs}
chmod 0700 /tmp/m-virtual-b2 /tmp/m-virtual-b2/{home,tmp}
export HOME=/tmp/m-virtual-b2/home TMPDIR=/tmp/m-virtual-b2/tmp
export RPI_PRIVATE_TMP=$TMPDIR
export RUSTUP_HOME="${PREPARED_RUSTUP_HOME:?}" CARGO_HOME="${PREPARED_CARGO_HOME:?}"
export RUSTUP_AUTO_INSTALL=0 CARGO_NET_OFFLINE=true CARGO_BUILD_JOBS=6
export RPI_TEST_HOST="${PARITY_TARGETS:?}/m-virtual-b2-178/debug/rpi"
# Run once with tc=178/version=1.78.0/RUSTFLAGS=-Dwarnings, then
# with tc=195/version=1.95.0 and RUSTFLAGS unset:
export CARGO_TARGET_DIR=$PARITY_TARGETS/m-virtual-b2-$tc
export RPI_TEST_RUSTC=$RUSTUP_HOME/toolchains/$version-x86_64-unknown-linux-gnu/bin/rustc
export RPI_TEST_MULTIPROVIDER_LIBRARY=$CARGO_TARGET_DIR/debug/librpi_multiprovider.so
export RPI_TEST_ANTIGRAVITY_LIBRARY=$CARGO_TARGET_DIR/debug/librpi_antigravity_auth.so
# Build with --message-format=json to artifacts-$tc.jsonl; stderr artifacts-$tc.log.
timeout 270 cargo +$version build --lib --locked --offline --message-format=json \
  -p rpi-antigravity-auth -p rpi-cursor-sdk -p rpi-multiprovider \
  > /tmp/m-virtual-b2/logs/artifacts-$tc.jsonl \
  2> /tmp/m-virtual-b2/logs/artifacts-$tc.log; echo exit=$?
timeout 270 cargo +$version test --locked --offline \
  -p rpi-antigravity-auth -p rpi-cursor-sdk -p rpi-multiprovider \
  > /tmp/m-virtual-b2/logs/test-$tc.log 2>&1; echo exit=$?
timeout 270 cargo +$version test --locked --offline -p rpi-multiprovider \
  --test m_native --test m_virtual_loaded \
  > /tmp/m-virtual-b2/logs/native-$tc.log 2>&1; echo exit=$?
timeout 270 cargo +$version test --locked --offline -p rpi-cursor-sdk \
  --test r02_native_loading -- --nocapture \
  > /tmp/m-virtual-b2/logs/r02-$tc.log 2>&1; echo exit=$?

# Consume completed gates, immutable mutation logs and two-pass fixture outputs.
python3 .github/scripts/refresh-m-virtual-b2-evidence.py --write
sha256sum docs/evidence/m-virtual-b2.json
python3 .github/scripts/refresh-m-virtual-b2-evidence.py --write
sha256sum docs/evidence/m-virtual-b2.json
python3 .github/scripts/refresh-m-virtual-b2-evidence.py
```

Portable content is deterministic for the same source and verified gate outcomes,
independent of checkout path, branch, raw log bytes and binary build paths. Use
`--logs <local-log-directory> --provenance <local-provenance.json>` to retain local
audit identities separately. The review-fix run used a fresh 0700 HOME/private TMP
under a 0700 temporary parent. Historical cohort JSONs remain historical.
