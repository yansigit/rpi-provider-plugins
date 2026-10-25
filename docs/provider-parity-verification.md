# Reconciled-host provider cohort verification

**Current state (host `e9c0526`, refreshed at `lane/g1-evidence-fix`):** the scoped
package-binding results and the both-toolchain gates below were rerun from this
checkout against host `e9c052653975c1d648092ab52e817a0b2f4b575c` (SDK workspace
0.8.4): **262 cohort passed / 0 failed / 24 inherited ignored, plus 8 real-host
passed**. The M-VIRTUAL-B scope-lease
evidence (that the same host binds per-package leases and releases them on turn
boundaries), including the eight real-host `m_virtual_loaded` cases, is in
[`evidence/m-virtual-b2.md`](evidence/m-virtual-b2.md) and its deterministic JSON
report. The host ledger is read-only and was not edited.

`provider-parity-sdk-cohort.json` is regenerated from this checkout by
`.github/scripts/refresh-provider-parity-evidence.py` and is byte-identical for
fixed verifier inputs. Earlier reports (host `7dfadb0` at `10515f5`, `9c59416`
before that) remain historical evidence in Git; other packages' domain-evidence
reports are unchanged.

Scope: `rpi-antigravity-auth`, `rpi-cursor-sdk`, `rpi-multiprovider`, pinned together
to host `e9c052653975c1d648092ab52e817a0b2f4b575c` (SDK workspace 0.8.4).
No production-code migration was needed: this host preserves the parity SDK
surface, and the packages already use the synchronous C emission contract.

## Offline reproduction

Run only in this worktree. The local parity environment redirects the SDK Git URL
to the existing local repository, so the pinned revision must already be present
there. Do not fetch/install dependencies from the network or call live providers;
all non-loopback network access is out of scope. Host source lives at
`/home/user/Projects/pi-rust` (and its pinned checkout
`/home/user/Projects/pi-rust--host-e9c0526`).

The Cursor runtime's pinned Node dependencies are installed, never fetched:

```bash
packages/rpi-cursor-sdk/scripts/ensure-runtime-deps.sh            # runtime/
packages/rpi-cursor-sdk/scripts/ensure-runtime-deps.sh --worker   # + worker/, for its IPC tests
```

The script runs `npm ci --offline --ignore-scripts --no-audit --no-fund` against
the local npm cache and fails loudly when the cache cannot satisfy
`runtime/package-lock.json`. A fresh worktree has no `runtime/node_modules`, so
both the packaged worker and `r02_native_loading`'s `stage()` precondition fail
with `ERR_MODULE_NOT_FOUND` in about 50 ms without it.

```bash
source /home/user/Projects/.parity-env.sh
export RUSTUP_HOME=/home/user/.rustup CARGO_HOME=/home/user/.cargo
export RUSTUP_AUTO_INSTALL=0 CARGO_NET_OFFLINE=true CARGO_BUILD_JOBS=6
LOGS=/tmp/pkg-repin-$(basename "$PWD")          # per-checkout, never shared between lanes
mkdir -p "$LOGS" /tmp/g1-evidence-run/{home,tmp}
export HOME=/tmp/g1-evidence-run/home TMPDIR=/tmp/g1-evidence-run/tmp
export RPI_PRIVATE_TMP=/tmp/g1-evidence-run/tmp
```

The Cursor worker build and staging run before the toolchain loop: together they
write the untracked `runtime/dist` and `runtime/shared` trees that
`r02_native_loading` copies beside the Cursor library, and a fresh worktree has
neither (the test refuses with `… runtime/dist is missing; stage the worker
first …`). The installed dependencies are enough:

```bash
(cd packages/rpi-cursor-sdk/worker &&
  timeout 90 node scripts/build.mjs &&
  timeout 45 node --test test/rpi/transport.test.mjs
) > "$LOGS/cursor-node24-final.log" 2>&1
(cd packages/rpi-cursor-sdk && timeout 30 node tests/stage-runtime.mjs) \
  > "$LOGS/cursor-stage.log" 2>&1
(cd packages/rpi-cursor-sdk/worker &&
  timeout 120 node node_modules/vitest/vitest.mjs run test/rpi/protocol.test.ts
) > "$LOGS/cursor-vitest.log" 2>&1
```

The parity environment selects Node 24.22.0. All eight IPC tests pass, including
the slow-consumer test, and the four Vitest protocol tests pass. Worker build
outputs are ignored and not committed.

Then, per toolchain:

```bash
for tc in 178 195; do
  if [ "$tc" = 178 ]; then
    version=1.78.0; export RUSTFLAGS=-Dwarnings
  else
    version=1.95.0; unset RUSTFLAGS
  fi
  export CARGO_TARGET_DIR=/home/user/Projects/.targets/g1-evidence-$tc
  timeout 600 cargo +$version test --locked --offline \
    -p rpi-antigravity-auth -p rpi-cursor-sdk -p rpi-multiprovider \
    -- --skip real_dll > "$LOGS/test-$tc.log" 2>&1 || exit $?
  cp "$LOGS/test-$tc.log" "$LOGS/test-$tc-final.log"
  timeout 180 cargo +$version build --lib --locked --offline \
    -p rpi-antigravity-auth -p rpi-cursor-sdk -p rpi-multiprovider \
    > "$LOGS/build-cdylib-$tc.log" 2>&1 || exit $?
  timeout 90 cargo +$version build --lib --locked --offline --message-format=json \
    -p rpi-antigravity-auth -p rpi-cursor-sdk -p rpi-multiprovider \
    > "$LOGS/artifacts-$tc.jsonl" 2> "$LOGS/artifacts-$tc.log" || exit $?
  export RPI_TEST_MULTIPROVIDER_LIBRARY=$CARGO_TARGET_DIR/debug/librpi_multiprovider.so
  export RPI_TEST_ANTIGRAVITY_LIBRARY=$CARGO_TARGET_DIR/debug/librpi_antigravity_auth.so
  export RPI_TEST_CURSOR_LIBRARY=$CARGO_TARGET_DIR/debug/librpi_cursor_sdk.so
  timeout 60 cargo +$version test -p rpi-multiprovider --test m_native \
    --locked --offline > "$LOGS/mp-native-$tc.log" 2>&1 || exit $?
  timeout 60 cargo +$version test -p rpi-cursor-sdk --test r02_native_loading \
    --locked --offline -- --nocapture > "$LOGS/r02-native-$tc.log" 2>&1 || exit $?
done
```

The cohort run skips the eight `real_dll_*` cases in `m_virtual_loaded`: they are
mandatory failures without a real host binary, and a plain `cargo test` cannot
order the fresh-DLL rebuild they check for. They run separately (see *Results*),
before the cdylib builds above, because their `-C metadata=m_virtual_b2_fresh_dll`
rebuild replaces the library files whose hashes the report records.

These explicit library paths exercise the multiprovider native-registration tests,
the Antigravity branch of `r02_native_loading`, and (with
`RPI_TEST_CURSOR_LIBRARY`) its Cursor branch, which additionally loads the packaged
`runtime/` worker, so `ensure-runtime-deps.sh` must have run first. The legacy-library
branch and the real host binary beyond the M-VIRTUAL-B scope below are **not**
claimed here. The test harness uses fake host vtables, not a running host process.

## Reference regenerators and deterministic report

The existing local-only generators below were run twice. Both passes matched each
other and the checked-in fixtures; no fixture was edited by hand.

```bash
for pass in 1 2; do
  (cd packages/rpi-multiprovider &&
    timeout 90 node --experimental-transform-types tests/commands-reference.ts --write &&
    timeout 90 node --experimental-transform-types tests/commands-a2-reference.ts --write
  ) > "$LOGS/reference-mp-$pass.log" 2>&1 || exit $?
  (cd packages/rpi-cursor-sdk &&
    timeout 60 node tests/r01-package-reference.mjs --write
  ) > "$LOGS/reference-cursor-$pass.log" 2>&1 || exit $?
  sha256sum packages/rpi-multiprovider/tests/commands{,-a2}-reference.json \
    packages/rpi-cursor-sdk/tests/r01-package-assets.json \
    > "$LOGS/fixtures-$pass.sha256"
done
cmp "$LOGS/fixtures-1.sha256" "$LOGS/fixtures-2.sha256"
```

There is no checked-in generator for the static cohort report; the report is the
generator. `.github/scripts/refresh-provider-parity-evidence.py` derives it from
these logs, Cargo's compiler-artifact identities, the actual SDK source checkouts,
locked dependencies, tracked package files, and rebuilt cdylibs. It operates on the
current checkout by default (`--root`, defaulting to the repository root, and the
host pin read from `Cargo.lock`/the package manifests) and takes the artifact
checkout with `--host-checkout`, which it asserts is at that pinned SHA, clean, and
unchanged from the layered pins. Logs default to a per-checkout temporary directory
(`PARITY_EVIDENCE_LOGS` overrides it) so lanes never read each other's evidence. It
checks that registry dependency versions/checksums did not change relative to
`5b5416c`, hashes all tracked files in the three resolved host crates, verifies their
checkout HEAD and cleanliness, and records every binary's SHA-256. It never runs a
build, reads credentials, or accesses the network.

After preserving the mutation-check logs described below (pass `--logs "$LOGS"`:
the default resolves under the current `TMPDIR`, which the sequence above isolates):

```bash
python3 .github/scripts/refresh-provider-parity-evidence.py \
  --host-checkout /home/user/Projects/pi-rust--host-e9c0526 --logs "$LOGS" --write
cp docs/provider-parity-sdk-cohort.json "$LOGS/cohort-pass1.json"
python3 .github/scripts/refresh-provider-parity-evidence.py \
  --host-checkout /home/user/Projects/pi-rust--host-e9c0526 --logs "$LOGS" --write
cmp "$LOGS/cohort-pass1.json" docs/provider-parity-sdk-cohort.json
python3 .github/scripts/refresh-provider-parity-evidence.py \
  --host-checkout /home/user/Projects/pi-rust--host-e9c0526 --logs "$LOGS" --check   # regenerates into a temp dir and diffs
```

Report regeneration is byte-identical for fixed verifier inputs. Rerunning tests
can change log durations/hashes; that intentionally changes the report. The report
has no timestamp, self-hash, or final commit SHA to create a generation cycle.

## Results and mutation checks

Each Rust toolchain passes the same cohort:

| Package | Passed | Failed | Ignored (pre-existing) |
| --- | ---: | ---: | ---: |
| rpi-antigravity-auth | 97 | 0 | 5 |
| rpi-cursor-sdk | 76 | 0 | 17 |
| rpi-multiprovider | 89 | 0 | 2 |
| **Cohort total** | **262** | **0** | **24** |
| `m_virtual_loaded` real-host (separate run) | 8 | 0 | 0 |

The 14 Cursor tests beyond the previously recorded 62 pin the worker bootstrap
failure path: an exited, timed-out, or pipe-closed worker reports its status and
bounded stderr (`worker_that_dies_during_bootstrap_reports_its_stderr_fast`,
`an_exited_worker_failure_carries_its_status_and_stderr`,
`a_handshake_timeout_carries_the_worker_stderr`, the closed-pipe and live-worker
cases), and the stderr ring redacts secrets before trimming, across chunk
boundaries, and keeps its tail valid UTF-8 (`bootstrap_secrets_are_redacted_from_worker_stderr`
and the `secrets_*`/ring cases). They are part of the cohort command. The 8
`rpi-multiprovider` real-host cases are the documented M-VIRTUAL-B sequence (build
the test with `--no-run`, `cargo rustc -p rpi-multiprovider --lib --locked --
-C metadata=m_virtual_b2_fresh_dll`, then run the test executable directly with
`RPI_TEST_HOST`, `RPI_TEST_RUSTC`, `RPI_TEST_MULTIPROVIDER_LIBRARY`).

Additional explicit-library gates pass 25 multiprovider tests, one Antigravity
native-loading test, and one Cursor native-loading test per toolchain (these
overlap cohort test names and are not added to the totals); the Cursor case SKIPs
its worker assertions when its library is absent and reports that SKIP. No new
ignored test or weakened assertion was introduced.

Every regression recorded here was individually mutation-checked, then the mutation
was reverted before both final cohort runs and cdylib builds:

| New regression | Temporary production mutation | Observed failure / log |
| --- | --- | --- |
| `cancellation_while_in_synchronous_c_emit_propagates_host_rejection` | Invert the real C emit nonzero-return rejection condition | Returned `Ok(())` instead of abort error; `mutate-c-emit.log` |
| `private_receipt_precedes_start_and_never_leaks_into_public_events_or_result` | Emit start before the private response receipt | Observed `start` before receipt; `mutate-receipt.log` |
| `blocked_downstream_preserves_idle_timeout_and_disabled_timeout_cancellation` | Bypass `Output::wait` around the bounded body-channel send | Stalled producer did not close on deadline, `Elapsed(())`; `mutate-blocked-idle.log` |
| `slow_body_consumer_over_four_mib_is_ordered_and_lossless_with_idle_disabled` | Reverse each body-channel chunk before sending | Byte-order equality failed; `mutate-body-order.log` |
| `slow consumer drains over 64 events and 4 MiB without loss and retains emission-time partial snapshots` | Replace `WorkerPeer`'s encoded partial text with `stale` | Partial-text equality failed; `mutate-cursor-snapshot.log` |
| `worker_that_dies_during_bootstrap_reports_its_stderr_fast` | Remove the child-exit race from the handshake `select!` | Both bootstrap tests waited the whole 10 s handshake timeout instead of failing fast; `mutate-exit-race.log` |
| `r02_native_loading::stage` precondition | Point the staged runtime at a missing `node_modules` | Failed in 0.13 s with `… is missing; run npm ci --offline … in packages/rpi-cursor-sdk/runtime first`; `mutate-stage-precondition.log` |

The first five logs were recorded by the run that introduced those regressions; a
plain report refresh re-reads them rather than re-applying production mutations, and
the report says so. The two bootstrap-precondition mutations above were applied and
reverted inside this lane.

The first test uses the actual package C emit sink and an intentionally blocked
host callback, then cancels and releases it with a nonzero return. It does not
claim that a plugin cancellation token can preempt synchronous foreign code.
The TLS tests use only a local fake server. They exercise real body buffering,
slow consumption of over 4 MiB, idle-timeout closure while downstream is blocked,
and explicit cancellation with idle timeout disabled. The receipt test separately
asserts private headers never appear in public events or the public result.
The Cursor test withholds credits, then drains 131 large snapshots with ordered
sequences, unchanged emission-time partials, and a terminal fence.

**Approved exceptions preserved:** Antigravity body-read idle timeout/cancel may
fire while downstream is blocked; Cursor encodes partial snapshots at emission
time, before its synchronous credit wait.

**Host follow-up needed (coverage only):** the package harnesses do not link the
host's typed bounded event queue or real extension C callback adapter. Direct
host-queue/backpressure coverage must run in the host repo. The new tests cover
the actual package adapters instead; they do not stand in for a full-host queue
test. No compilation breakage or host source change was needed. No full-workspace
Rust 1.78 gate, live-provider parity, or new G1/G2 acceptance is claimed.
