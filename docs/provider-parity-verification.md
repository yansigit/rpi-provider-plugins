# Reconciled-host provider cohort verification

**Host 229ce5a / M-VIRTUAL-B scope-lease update:** current scoped package-binding
results and both-toolchain gates (255 passed / 0 failed / 24 inherited ignored)
are in [`evidence/m-virtual-b2.md`](evidence/m-virtual-b2.md) and its deterministic
JSON report. M09 real post-turn compaction and layered M08 cancel cleanup are
proven there, with package/host test boundaries and notice-channel qualifications
explicit. The host ledger is read-only and was not edited.

The `provider-parity-sdk-cohort.json` report and the rest of this page record the
historical host-7dfadb0 cohort (commit `10515f5`); they have not been relabelled as
8014e36 results. Its predecessor (commit `9c59416`) remains historical evidence in
Git. Other packages' older domain-evidence reports are likewise unchanged.

Scope: `rpi-antigravity-auth`, `rpi-cursor-sdk`, `rpi-multiprovider`, pinned together
to host `7dfadb0720bed6d624ea39b383e64b3753469206` (SDK workspace 0.8.4).
No production-code migration was needed: this host preserves the parity SDK
surface, and the packages already use the synchronous C emission contract.

## Offline reproduction

Run only in `/home/user/Projects/rpi-package--stream-api-migration` on
`lane/stream-api-migration`. The local parity environment redirects the SDK Git
URL to the existing local repository. Do not fetch/install dependencies or call
live providers. All non-loopback network access is out of scope.

```bash
source /home/user/Projects/.parity-env.sh
export RUSTUP_HOME=/home/user/.rustup CARGO_HOME=/home/user/.cargo
export RUSTUP_AUTO_INSTALL=0 CARGO_NET_OFFLINE=true CARGO_BUILD_JOBS=8
mkdir -p /tmp/pkg-repin/{home,tmp}
export HOME=/tmp/pkg-repin/home TMPDIR=/tmp/pkg-repin/tmp
export RPI_PRIVATE_TMP=/tmp/pkg-repin/tmp

for tc in 178 195; do
  if [ "$tc" = 178 ]; then
    version=1.78.0; export RUSTFLAGS=-Dwarnings
  else
    version=1.95.0; unset RUSTFLAGS
  fi
  export CARGO_TARGET_DIR=/home/user/Projects/.targets/verify-integrated-package-$tc
  timeout 600 cargo +$version test --locked --offline \
    -p rpi-antigravity-auth -p rpi-cursor-sdk -p rpi-multiprovider \
    > /tmp/pkg-repin/test-$tc-final.log 2>&1 || exit $?
  timeout 180 cargo +$version build --lib --locked --offline \
    -p rpi-antigravity-auth -p rpi-cursor-sdk -p rpi-multiprovider \
    > /tmp/pkg-repin/build-cdylib-$tc.log 2>&1 || exit $?
  timeout 90 cargo +$version build --lib --locked --offline --message-format=json \
    -p rpi-antigravity-auth -p rpi-cursor-sdk -p rpi-multiprovider \
    > /tmp/pkg-repin/artifacts-$tc.jsonl \
    2> /tmp/pkg-repin/artifacts-$tc.log || exit $?
  export RPI_TEST_MULTIPROVIDER_LIBRARY=$CARGO_TARGET_DIR/debug/librpi_multiprovider.so
  export RPI_TEST_ANTIGRAVITY_LIBRARY=$CARGO_TARGET_DIR/debug/librpi_antigravity_auth.so
  timeout 60 cargo +$version test -p rpi-multiprovider --test m_native \
    --locked --offline > /tmp/pkg-repin/mp-native-$tc.log 2>&1 || exit $?
  timeout 60 cargo +$version test -p rpi-cursor-sdk --test r02_native_loading \
    --locked --offline -- --nocapture > /tmp/pkg-repin/r02-native-$tc.log 2>&1 || exit $?
done
```

These explicit library paths exercise the multiprovider native-registration tests
and the Antigravity branch of `r02_native_loading`. The Cursor and legacy-library
branches require separate environment variables and are **not** claimed here.
The test harness uses fake host vtables, not a running host process.

For the Cursor worker, reuse the existing installed dependencies without running
an installer. This verification temporarily linked `worker/node_modules` to
`/home/user/Projects/rpi-package/packages/rpi-cursor-sdk/worker/node_modules`,
then removed the link. With that link in place:

```bash
(cd packages/rpi-cursor-sdk/worker &&
  timeout 90 node scripts/build.mjs &&
  timeout 45 node --test test/rpi/transport.test.mjs
) > /tmp/pkg-repin/cursor-node24-final.log 2>&1
```

The parity environment selects Node 24.22.0. Its eight IPC tests pass, including
the new slow-consumer test. Worker build outputs are ignored and not committed.

## Reference regenerators and deterministic report

The existing local-only generators below were run twice. Both passes matched each
other and the checked-in fixtures; no fixture was edited by hand.

```bash
for pass in 1 2; do
  (cd packages/rpi-multiprovider &&
    timeout 90 node --experimental-transform-types tests/commands-reference.ts --write &&
    timeout 90 node --experimental-transform-types tests/commands-a2-reference.ts --write
  ) > /tmp/pkg-repin/reference-mp-$pass.log 2>&1 || exit $?
  (cd packages/rpi-cursor-sdk &&
    timeout 60 node tests/r01-package-reference.mjs --write
  ) > /tmp/pkg-repin/reference-cursor-$pass.log 2>&1 || exit $?
  sha256sum packages/rpi-multiprovider/tests/commands{,-a2}-reference.json \
    packages/rpi-cursor-sdk/tests/r01-package-assets.json \
    > /tmp/pkg-repin/fixtures-$pass.sha256
done
cmp /tmp/pkg-repin/fixtures-1.sha256 /tmp/pkg-repin/fixtures-2.sha256
```

There was no checked-in generator for the previous static cohort report. The new
`.github/scripts/refresh-provider-parity-evidence.py` derives its replacement from
these logs, Cargo's compiler-artifact identities, the actual SDK source checkouts,
locked dependencies, tracked package files, and rebuilt cdylibs. It checks that
registry dependency versions/checksums did not change relative to `5b5416c`.
It hashes all tracked files in the three resolved host crates, verifies their
checkout HEAD and cleanliness, and records every binary's SHA-256. It never
runs a build, reads credentials, or accesses the network.

After preserving the mutation-check logs described below:

```bash
python3 .github/scripts/refresh-provider-parity-evidence.py --write
cp docs/provider-parity-sdk-cohort.json /tmp/pkg-repin/cohort-pass1.json
python3 .github/scripts/refresh-provider-parity-evidence.py --write
cmp /tmp/pkg-repin/cohort-pass1.json docs/provider-parity-sdk-cohort.json
python3 .github/scripts/refresh-provider-parity-evidence.py
```

Report regeneration is byte-identical for fixed verifier inputs. Rerunning tests
can change log durations/hashes; that intentionally changes the report. The report
has no timestamp, self-hash, or final commit SHA to create a generation cycle.

## Results and mutation checks

Each Rust toolchain passes the same cohort:

| Package | Passed | Failed | Ignored (pre-existing) |
| --- | ---: | ---: | ---: |
| rpi-antigravity-auth | 97 | 0 | 5 |
| rpi-cursor-sdk | 62 | 0 | 17 |
| rpi-multiprovider | 78 | 0 | 2 |
| **Total** | **237** | **0** | **24** |

Additional explicit-library gates pass 15 multiprovider tests and one Antigravity
native-loading test per toolchain (these overlap cohort test names and are not
added to the totals). No new ignored test or weakened assertion was introduced.

Every new regression was individually mutation-checked, then the mutation was
reverted before both final cohort runs and cdylib builds:

| New regression | Temporary production mutation | Observed failure / log in `/tmp/pkg-repin/` |
| --- | --- | --- |
| `cancellation_while_in_synchronous_c_emit_propagates_host_rejection` | Invert the real C emit nonzero-return rejection condition | Returned `Ok(())` instead of abort error; `mutate-c-emit.log` |
| `private_receipt_precedes_start_and_never_leaks_into_public_events_or_result` | Emit start before the private response receipt | Observed `start` before receipt; `mutate-receipt.log` |
| `blocked_downstream_preserves_idle_timeout_and_disabled_timeout_cancellation` | Bypass `Output::wait` around the bounded body-channel send | Stalled producer did not close on deadline, `Elapsed(())`; `mutate-blocked-idle.log` |
| `slow_body_consumer_over_four_mib_is_ordered_and_lossless_with_idle_disabled` | Reverse each body-channel chunk before sending | Byte-order equality failed; `mutate-body-order.log` |
| `slow consumer drains over 64 events and 4 MiB without loss and retains emission-time partial snapshots` | Replace `WorkerPeer`'s encoded partial text with `stale` | Partial-text equality failed; `mutate-cursor-snapshot.log` |

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
