# M-VIRTUAL-B — native host-v2 virtual binding

Status: **provider build/test gates pass; M09 post-turn compaction and layered M08 cancel cleanup proven. Inherited strict lint/format debt remains**.
This report supersedes only the M06–M09 package-binding evidence; it does not
promote historical reports or edit the read-only host ledger. Machine-readable
hashes, SDK identities, counts and limitations: [`m-virtual-b2.json`](m-virtual-b2.json).
The cohort counts in that JSON (97 / 62 / 97 = 256) are as measured for the
reviewed commit below; [`../provider-parity-verification.md`](../provider-parity-verification.md)
reports 258 because two worker-bootstrap regression tests were added after this
report's gate run, and this file was not regenerated for that addition (its
inputs are the historical mutation, pinned-host, real-host and
fixture-regeneration logs of the reviewed commit, not a plain refresh).

## Source and boundary

- Reviewed baseline commit `916fb385bf499be2607c133a4e382d634d648cac`,
  based on `10515f50bc9a9f7a5a958d00bfd166c1b8f0ff53`.
  Checkout location and branch are local provenance, not portable evidence.
- All three provider packages use host
  `e9c052653975c1d648092ab52e817a0b2f4b575c`, unchanged Git URL. Cargo's existing
  URL cache was seeded from the local read-only host repository, not the network.
- Registry dependency versions/checksums are byte-for-byte equivalent to the base
  lockfile. The re-pinned complete provider cohort compiled on Rust 1.78.0 and 1.95.0.
- No host source changes; no old-lane changes, push, external requests, live
  credentials, or `~/.pi` / `~/.rpi` access. All test HOME/private TMP directories
  are isolated mode 0700. Fixture credentials are synthetic.

Review fixes:

- `4d2c0a8`: typed synthetic terminal identity and transport diagnostics (F1).
- `7d08414`: scope-local virtual pin restoration and missing-session rejection (F2/F3).
- F4 separates portable content from optional local provenance. Two original
  regenerations, a relocated detached checkout, and a differently named branch
  produced byte-identical JSON and Markdown.

Latest re-review fixes (baseline `7f2c169`):

- `80091ac`: seed scope-local pending inherited decisions from the filtered capture.
  The ABI regression supplies both virtual-backend and ordinary pool pins through
  `PI_MULTIPROVIDER_SESSION_PINS`, verifies journal appends, recorded precedence,
  deferred identity/scheduler availability, and parent isolation after child shutdown.
- `67cbf22`: real-host tests default to the DLL in their own executable's `deps`
  directory, not a potentially stale top-level artifact. Explicit paths are still
  supported but missing DLLs or DLLs older than the test executable fail loudly.
  Negative missing/stale checks both exited 101; both toolchains' five host cases
  then passed against freshly rebuilt DLLs. No artifact-skip path exists.

Scope-lease adoption (starting at `6fbaecd`):

- `6756edd`: pin all three providers to local host 229ce5a; only host-source
  lock entries changed (`rpi-ai`, `rpi-plugin-sdk`, `rpi-telemetry`).
- `6f3448b`: acquire the narrow grant before callback completion, scope-own and
  drain delayed work, cancel invocation-local tokens at completion, and strengthen
  package-DLL and pool-drain regressions.
- Latest cancellation follow-up gates and optional binary provenance:
  `/tmp/m-virtual-b2-cancel/`; earlier lease evidence: `/tmp/m-virtual-b2-lease/`.
  The Markdown/JSON pair and both differential fixtures were checked byte-identical
  across two regenerations. Prior mutation/IPC/missing-stale evidence is historical,
  explicitly distinguished from the three new lease mutations.

## Final host re-pin

- `daad9a9`: re-pin all three providers from 229ce5a to final host
  `e9c052653975c1d648092ab52e817a0b2f4b575c` using local-only Cargo Git cache
  imports. Only the three host-source lock entries changed; registry entries and
  every other lockfile field are unchanged.
- Built the host with Rust 1.95 in a new detached checkout
  `/home/user/Projects/pi-rust--host-e9c0526`, target
  `/home/user/Projects/.targets/host-e9c0526-195`. No host-source edits.
- Rebuilt all provider cdylibs on 1.78.0 (`-Dwarnings`) and 1.95.0. Per toolchain:
  **256 passed / 0 failed / 24 ignored**, including **8 real-host DLL tests**
  against that new host with DLLs rebuilt after the exact test executables;
  **25 native ABI tests** and **1 explicit AG loading test** passed.
- Regenerated the **70 virtual / 96 lift** fixtures twice byte-identically,
  unchanged from the tracked fixtures. Regenerated portable evidence twice and
  verified freshness after commit. Logs and separate local provenance:
  `/tmp/m-virtual-b2-final/`; isolated HOME/TMP and parent are mode 0700.
- Earlier mutation, missing/stale negative, Node IPC and complementary host-test
  logs remain historical, copied from `/tmp/m-virtual-b2-cancel/logs`; they were
  not rerun for this pin-only change. No new package behavior or coverage claims.

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
name alone grants nothing. The host must also advertise `services.scopeLease.v1`.
The selected invocation acquires `scope_service_lease("pi-fabric", 1,
&["isIdle", "compact"], invocation_signal)` before returning. Workers keep only
this narrow lease, a non-authority account-count snapshot and their scope-owned
cancellation signal, never an invocation client. Invocation signals are cancelled
on callback completion; completed invocation signals cannot drive delayed calls.
Absent/incompatible service or lease capability means ordinary inline failover.
Quota/auth non-host-retryable failures still rotate inline. The existing 50ms idle
poll, 15s deadline and 30s debounce remain. Busy is normal lost admission and emits
neither error nor success notice. Workers release on completion/cancel and are
cancelled/joined on shutdown. No root registration-client fallback is used.
The exact TS success notice is emitted on stderr: the narrow lease grants no UI
authority, so retaining a UI client would violate the contract. This is a compatible
`pi-fabric` v1 fixture service, not a claim that a production Fabric extension is available.

## Gates (both package toolchains)

| Gate | Rust 1.78.0 (`-Dwarnings`) | Rust 1.95.0 |
|---|---:|---:|
| Offline provider cdylib build | pass | pass |
| Antigravity cohort | 97 passed / 0 failed / 5 inherited ignored | same |
| Cursor cohort | 62 / 0 / 17 inherited ignored | same |
| Multiprovider cohort | 97 / 0 / 2 inherited ignored | same |
| Total cohort | **256 / 0 / 24 inherited ignored** | **same** |
| Explicit MP native ABI tests | 25 / 0 / 0 | 25 / 0 / 0 |
| Actual-host MP DLL acceptance | 8 / 0 / 0 | 8 / 0 / 0 |
| Explicit Antigravity loading branch | 1 / 0 / 0 | 1 / 0 / 0 |

The explicit native tests are included in cohort counts, not additional tests.

### Cancellation clock follow-up (no host re-pin)

`FabricHost::now_ms` now returns `Option<i64>`: cancellation is `None`, never
`i64::MAX`. Compaction returns `Cancelled` at clock reads and uses saturating
deadline/debounce arithmetic as defense in depth. The worker drops the state
lock before explicitly releasing its lease, including this cancellation path.

The deterministic native unit test
`cancellation_between_context_and_clock_releases_without_poisoning` cancels
immediately after the successful context check, before the initial clock read.
It joins the real worker without panic, checks the mutex is unpoisoned and the
debounce state unchanged, observes explicit lease release while retaining a
lease clone (excluding last-clone RAII cleanup), then compacts successfully with
a fresh signal/lease using the same state. It passes on both toolchains.
Restoring the old cancellation sentinel as `Some(i64::MAX)` fails the regression
with `cancellation is not a timestamp` (exit 101), even with saturating arithmetic
retained; the mutation was reverted before the final gates.

Both package cohorts, native ABI gates, eight real-host tests per toolchain
with freshly rebuilt DLLs, and 70/96 differential fixtures were rerun offline.
The differential outputs were regenerated twice byte-identically. Earlier
mutation, Node IPC and complementary host-source test logs remain historical;
no host pin, host source or registry dependency was changed. Verify this follow-up
with `python3 .github/scripts/refresh-m-virtual-b2-evidence.py --logs /tmp/m-virtual-b2-cancel/logs`.
All three doctest suites completed. No new ignores, passing artifact skips or
weakened assertions were introduced in the new acceptance suite. The existing
`r02_native_loading` harness explicitly executes the requested AG library branch;
its unrelated legacy-provider and Cursor-library branches still report their
pre-existing skips. Historical Cursor IPC evidence remains **8/8 on Node 24.22.0**
(not rerun for these native-only fixes).
Both MP cdylibs are loaded into the real host built from pinned e9c0526 with Rust
1.95 in the read-only host worktree, using its separate target directory
`/home/user/Projects/.targets/h-scope-lease-195`. The same host executable serves
both package toolchains. Fixture target/service DLLs are independently compiled;
isolated agent directories, local sockets and bounded marker waits synchronize
post-turn work. All Cargo commands ran in the foreground with `timeout 270`.
Two complementary pinned-host loaded-library tests also passed (Rust 1.95):
`loaded_cancel_reclaims_tables_before_blocked_callback_and_library_drain` and
`loaded_scope_service_compaction_after_callback_is_selected_idle_only_and_revocable`.

Native virtual ABI regression names (10):

- `virtual_native_failover_releases_every_target_and_receipt`
- `virtual_native_unsupported_v2_never_downgrades`
- `virtual_native_missing_terminal_fails_and_releases`
- `virtual_native_publication_ack_healing_delete_and_reregister`
- `virtual_native_capable_service_compacts_and_shutdown_drains`
- `virtual_native_delayed_compaction_is_cancelled_and_drained_on_shutdown`
- `virtual_native_transport_failure_preserves_diagnostic_and_identity`
- `virtual_native_recorded_pin_is_scope_local_across_reconcile_and_shutdown`
- `virtual_native_missing_session_identity_fails_explicitly`
- `virtual_native_inherited_pins_restore_journal_and_preserve_parent`

Actual-host acceptance names (8):

- `real_dll_cross_provider_dispatch_and_backend_failover`
- `real_dll_cross_provider_dispatch_reaches_target_pool`
- `real_dll_cancellation_attempts_release_after_host_revocation`
- `real_dll_virtual_cycle_rejection_surfaces`
- `real_dll_capable_fabric_shutdown_cancels_delayed_compaction`
- `real_dll_post_turn_scope_lease_compacts`
- `real_dll_post_turn_busy_is_skipped`
- `real_dll_absent_scope_lease_rotates_inline`

The success case seeds two real turns, surfaces the third turn's network error,
then opens the service barrier only after `agent_end`. The service forwards to
actual host IsIdle/Compact. The test requires a completed host compaction, one
persisted compaction entry in the selected session file, the TS notice, and exactly
one lease release. Busy is injected at the package service seam; the complementary
host test exercises real competing-run admission and child/main transcript isolation.
The package transcript case selects main, not a child; these evidence layers are
kept distinct rather than claiming a package child transcript replay.

The loader fixture audits actual continuation operations, opaque handle values,
target-auth requests and release results; the target fixture requires its own
synthetic credential and proves that no source identity/options crossed over.
The cancellation test requires one release attempt per actual handle and accepts
success or only `inactive provider continuation`, `delegation epoch revoked`, or
`expired provider continuation`. It does **not** require successful post-revocation
RPCs. Host-table reclamation is independently asserted in the pinned-host test;
the package lift regression now compares every pool/account's `inFlight` with its
pre-run baseline after awaiting producer drain (not just cancellation acknowledgement).

Existing upstream differential tests are preserved unchanged: 70 virtual and
96 lift scenarios cover backend/account traces, failover order, errors, terminal
boundaries and missing terminals. Both upstream extractors and the existing
command/Cursor asset fixture regenerators previously produced byte-identical outputs
twice. This review-fix pass reran the virtual/lift extractors twice, byte-identically.
No changes were made to the approved AG body-read timeout, Cursor snapshot,
presentation or import-storage exceptions.

### Mutation checks

The eight prior mutations below remain historical evidence. This lease-adoption
pass additionally killed all three requested real-DLL mutations (exit 101 each),
restored them, and reran both complete toolchain gates. The invocation-signal
mutation initially survived because callback completion did not cancel the local
token; callback teardown now explicitly cancels it, and the restored mutation was
then killed by missing post-turn compact. This teardown fix does not cancel the
independent scope worker.

| Guard mutated | Regression |
|---|---|
| Accept v1 instead of required v2 | `virtual_native_unsupported_v2_never_downgrades` |
| Omit receipt release in target guard | `virtual_native_failover_releases_every_target_and_receipt` |
| Forward source identity | `virtual_native_failover_releases_every_target_and_receipt` |
| Replace exact missing-terminal error (historical) | `virtual_native_missing_terminal_fails_and_releases` |
| F1: read fallback identity from the descriptor-free envelope | `virtual_native_missing_terminal_fails_and_releases` (typed deserialization fails) |
| F2: route virtual pins back to the ordinary service | `virtual_native_recorded_pin_is_scope_local_across_reconcile_and_shutdown` (selected backend differs) |
| F3: accept empty affinity | `virtual_native_missing_session_identity_fails_explicitly` (exact error absent) |
| Revert inherited pending seeding (historical) | `virtual_native_inherited_pins_restore_journal_and_preserve_parent` (backend/account differ) |
| Omit lease acquisition and fall back inline | `real_dll_post_turn_scope_lease_compacts` (worker lease/release absent) |
| Retain completed invocation signal for delayed calls | `real_dll_post_turn_scope_lease_compacts` (post-turn compact missing) |
| Treat busy as a reported compaction error | `real_dll_post_turn_busy_is_skipped` (busy diagnostic surfaced) |

Portable JSON indexes logical log names. Optional local provenance records absolute
paths, artifact sizes and SHA-256 hashes separately; it is not committed or used
in portable freshness comparisons. Binary hashes and raw-log hashes are not portable
because compiler paths and log output vary across checkouts.

### Formatting and lint qualification

Lint comparisons below are historical from the preceding review-fix pass; this
narrow re-review reran changed-file formatting and the requested build/test gates,
not clippy or full-workspace formatting.

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

## Ledger qualification and remaining gaps

| Row | Package evidence now established | Not established |
|---|---|---|
| M06 | Owned registration, ack/healing/delete/re-register, native command binding, global wrapper and real DLL dispatch | No promotion of unrelated service-lane or typed-queue evidence |
| M07 | Template/model effects, backend failover, target wrappers and target pool selection; upstream differential traces | Exhaustive real-host weighted/nested catalog combinations are not separately replayed |
| M08 | **Cancel contract proven in layers**: real package DLL release attempts accept only documented revocation outcomes; pinned-host loaded test asserts table reclamation before blocked callback drain and final continuation/library drain; package lift asserts all account `inFlight` values return to baseline after producer drain | Successful RPC release after revocation is not required or claimed; package DLL has no host-table introspection port |
| M09 | **Post-turn compaction proven**: real package DLL acquires narrow scope lease, surfaces failure, performs actual host compaction after callback return, persists selected-session compaction and emits notice; busy skip, absent-capability inline rotation, shutdown cancel/release/drain; host loaded test separately proves child/main isolation and actual busy admission | No production Fabric extension or package child-transcript replay; notice is diagnostic output, not UI |

Host e9c0526 includes cancellation reclamation f5002d7 and scope-service leases.
Host tables are not inferred from rejected cleanup RPCs: the complementary loaded
host test explicitly checks `(false, 0, 1, 1)` → `(true, 0, 0, 0)` while the foreign
callback is blocked, then final disappearance after producer/library drain.
Ordinary invocation clients and `scope_lifetime` remain unsuitable for delayed
service authority. Only the new grant is retained; no root fallback was added.
No host source was edited. Linux acceptance only; no real Fabric extension, live
provider, typed bounded queue load or full-workspace Rust-1.78 gate is claimed.

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
export RPI_TEST_HOST="${PARITY_TARGETS:?}/host-e9c0526-195/debug/rpi"
# Run once with tc=178/version=1.78.0/RUSTFLAGS=-Dwarnings, then
# with tc=195/version=1.95.0 and RUSTFLAGS unset:
export CARGO_TARGET_DIR=$PARITY_TARGETS/m-virtual-b2-$tc
export RPI_TEST_RUSTC=$RUSTUP_HOME/toolchains/$version-x86_64-unknown-linux-gnu/bin/rustc
export RPI_TEST_MULTIPROVIDER_LIBRARY=$CARGO_TARGET_DIR/debug/deps/librpi_multiprovider.so
export RPI_TEST_ANTIGRAVITY_LIBRARY=$CARGO_TARGET_DIR/debug/librpi_antigravity_auth.so
# Build with --message-format=json to artifacts-$tc.jsonl; stderr artifacts-$tc.log.
timeout 270 cargo +$version build --lib --locked --offline --message-format=json \
  -p rpi-antigravity-auth -p rpi-cursor-sdk -p rpi-multiprovider \
  > /tmp/m-virtual-b2/logs/artifacts-$tc.jsonl \
  2> /tmp/m-virtual-b2/logs/artifacts-$tc.log; echo exit=$?
# Compile all test executables; run real-host cases separately after a fresh DLL build.
timeout 270 cargo +$version test --locked --offline \
  -p rpi-antigravity-auth -p rpi-cursor-sdk -p rpi-multiprovider -- --skip real_dll \
  > /tmp/m-virtual-b2/logs/test-$tc.log 2>&1; echo exit=$?
timeout 270 cargo +$version test --locked --offline -p rpi-multiprovider \
  --test m_native \
  > /tmp/m-virtual-b2/logs/native-$tc.log 2>&1; echo exit=$?
timeout 270 cargo +$version test --locked --offline -p rpi-cursor-sdk \
  --test r02_native_loading -- --nocapture \
  > /tmp/m-virtual-b2/logs/r02-$tc.log 2>&1; echo exit=$?

# Force a fresh DLL build after the test executable, without relinking that test.
# The metadata argument changes only build identity, not provider behavior.
timeout 270 cargo +$version rustc --locked --offline -p rpi-multiprovider --lib \
  -- -C metadata=m_virtual_b2_fresh_dll \
  > /tmp/m-virtual-b2/logs/fresh-dll-$tc.log 2>&1; echo exit=$?
# Extract the EXACT executable Cargo just ran; never glob an older test executable.
exe=$(python3 -c 'import re,sys; print(re.search(r"Running tests/m_virtual_loaded.rs \\(([^)]+)\\)",open(sys.argv[1]).read())[1])' /tmp/m-virtual-b2/logs/test-$tc.log)
timeout 270 "$exe" --nocapture > /tmp/m-virtual-b2/logs/real-host-$tc.log 2>&1; echo exit=$?
# Include these eight actual results in cohort/native totals (not skipped coverage).
for kind in test native; do
  python3 -c 'import pathlib,sys; p=pathlib.Path(sys.argv[1]); p.write_text(p.read_text()+pathlib.Path(sys.argv[2]).read_text())' \
    /tmp/m-virtual-b2/logs/$kind-$tc.log /tmp/m-virtual-b2/logs/real-host-$tc.log
done

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
