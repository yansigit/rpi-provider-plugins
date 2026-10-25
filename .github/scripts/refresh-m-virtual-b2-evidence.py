#!/usr/bin/env python3
"""Deterministic, offline M-VIRTUAL-B verifier. See docs/evidence/m-virtual-b2.md.

Consumes completed foreground gate logs; never runs builds or accesses auth.
Historical evidence is not rewritten. Output has no timestamp or self-hash.
"""
import argparse
import difflib
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import tomllib

PACKAGES = ('rpi-antigravity-auth', 'rpi-cursor-sdk', 'rpi-multiprovider')
HOST_CRATES = ('rpi-ai', 'rpi-plugin-sdk', 'rpi-telemetry')
BASE = '10515f50bc9a9f7a5a958d00bfd166c1b8f0ff53'
# Counts as measured when this evidence was written. Test counts drift as tests are
# added or moved between toolchains, so they are recorded as the floor to beat, not
# an exact expectation a later run must reproduce.
COUNTS_AT_TIME_OF_WRITING = {'rpi-antigravity-auth': 97, 'rpi-cursor-sdk': 64, 'rpi-multiprovider': 97}


def checkout_root(start=None):
    """Repository root of the checkout that contains this script (or --root)."""
    probe = Path(start) if start is not None else Path(__file__).resolve()
    resolved = subprocess.run(['git', '-C', str(probe), 'rev-parse', '--show-toplevel'],
                              capture_output=True, text=True)
    if resolved.returncode == 0 and resolved.stdout.strip():
        return Path(resolved.stdout.strip())
    return Path(__file__).resolve().parents[2]


ROOT = checkout_root()


def default_logs(root):
    """Per-checkout verifier log directory; never a directory shared by other lanes."""
    return Path(os.environ.get('PARITY_EVIDENCE_LOGS', Path(tempfile.gettempdir()) / f'm-virtual-b2-{root.name}'))


def base_commit(reference, root):
    """The baseline commit, or a clear exit when this clone cannot reach it."""
    probe = subprocess.run(['git', '-C', str(root), 'rev-parse', '--verify', '--quiet', f'{reference}^{{commit}}'],
                           capture_output=True, text=True)
    if probe.returncode != 0 or not probe.stdout.strip():
        sys.exit(f'base commit {reference} is not in {root}; a shallow clone cannot read '
                 f'`git show {reference}:Cargo.lock` — fetch it or pass --base')
    return probe.stdout.strip()


def git(*args, cwd=None):
    return subprocess.check_output(['git', '-C', str(cwd or ROOT), *args]).decode().strip()


def identity(path):
    """Portable source identity; external artifact provenance is separate."""
    data = path.read_bytes()
    return {'path': path.relative_to(ROOT).as_posix(), 'bytes': len(data), 'sha256': hashlib.sha256(data).hexdigest()}


def provenance(logs):
    """Optional local audit data, never part of portable freshness checks."""
    paths = list(sorted(logs.iterdir()))
    for short in ('178', '195'):
        for line in (logs / f'artifacts-{short}.jsonl').read_text().splitlines():
            artifact = json.loads(line)
            paths.extend(Path(f) for f in artifact.get('filenames', []) if f.endswith('.so'))
    return {'checkout': str(ROOT), 'commit': git('rev-parse', 'HEAD'),
        'files': [{'path': str(path), 'bytes': path.stat().st_size,
            'sha256': hashlib.sha256(path.read_bytes()).hexdigest()} for path in paths if path.is_file()]}


def results(text):
    assert 'test result: FAILED' not in text
    matches = re.findall(r'test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored', text)
    assert matches, 'missing test results'
    assert all(failed == '0' for _, failed, _ in matches)
    return [tuple(map(int, match)) for match in matches]


def cohort(path):
    text = path.read_text()
    results(text)
    current = None
    counts = {name: dict(passed=0, failed=0, ignored=0) for name in PACKAGES}
    for line in text.splitlines():
        if 'Running unittests' in line:
            current = re.search(r'deps/(rpi_[a-z_]+)-', line)[1].replace('_', '-')
        elif 'Doc-tests' in line:
            current = line.split()[-1].replace('_', '-')
        match = re.search(r'test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored', line)
        if match:
            for key, value in zip(('passed', 'failed', 'ignored'), match.groups()):
                counts[current][key] += int(value)
    assert text.count('Doc-tests') == 3
    # The historical 62 cursor tests plus the two worker-bootstrap regressions of this lane
    # plus everything added later: a run may only gain tests, and none may fail.
    for name in PACKAGES:
        assert counts[name]['failed'] == 0, f'{name} recorded failed tests: {counts[name]}'
        assert counts[name]['passed'] >= COUNTS_AT_TIME_OF_WRITING[name], \
            f'{name} ran {counts[name]["passed"]} tests, fewer than the {COUNTS_AT_TIME_OF_WRITING[name]} recorded here: {counts}'
    return counts


def report(logs, host_checkout=None, base=BASE):
    lock = tomllib.loads((ROOT / 'Cargo.lock').read_text())
    previous = tomllib.loads(git('show', f'{base}:Cargo.lock'))
    pinned = {match[1] for package in lock['package'] if package['name'] in HOST_CRATES
              for match in [re.search(r'rev=([0-9a-f]{40})', package.get('source', ''))] if match}
    manifests_pin = {tomllib.loads((ROOT / 'packages' / name / 'Cargo.toml').read_text())['dependencies']['rpi-plugin-sdk']['rev'] for name in PACKAGES}
    assert len(manifests_pin) == 1, f'package manifests pin {sorted(manifests_pin)}'
    host = manifests_pin.pop()
    # Cargo.lock also pins unrelated host-SDK revisions for this workspace's own graph.
    assert host in pinned, f'Cargo.lock has no {HOST_CRATES} entry at the manifest pin {host}; found {sorted(pinned)}'
    for name in PACKAGES:
        manifest = tomllib.loads((ROOT / 'packages' / name / 'Cargo.toml').read_text())
        assert manifest['dependencies']['rpi-plugin-sdk']['rev'] == host, f'{name} manifest pin differs from Cargo.lock'
    if host_checkout is not None:
        assert git('rev-parse', 'HEAD', cwd=host_checkout) == host, f'--host-checkout {host_checkout} is not at the pinned host commit {host}'
    def registry(value):
        return sorted((p['name'], p['version'], p.get('checksum')) for p in value['package'] if p.get('source', '').startswith('registry+'))
    assert registry(lock) == registry(previous)
    host_crates = [p for p in lock['package'] if host in p.get('source', '')]
    assert sorted(p['name'] for p in host_crates) == list(HOST_CRATES)
    for name in PACKAGES:
        manifest = tomllib.loads((ROOT / 'packages' / name / 'Cargo.toml').read_text())
    toolchains = {}
    for short, version in [('178', '1.78.0'), ('195', '1.95.0')]:
        artifacts = [json.loads(line) for line in (logs / f'artifacts-{short}.jsonl').read_text().splitlines()]
        assert artifacts[-1] == {'reason': 'build-finished', 'success': True}
        sdk_artifacts = [a for a in artifacts if a.get('reason') == 'compiler-artifact' and a['target']['name'].replace('_', '-') in HOST_CRATES]
        assert len(sdk_artifacts) == 3
        stale = sorted({re.search(r'rev=([0-9a-f]{40})', a['package_id'])[1] if 'rev=' in a['package_id'] else a['package_id'] for a in sdk_artifacts if host not in a['package_id']})
        assert not stale, f'{logs}/artifacts-{short}.jsonl was built against {stale}, not the pinned host {host}; rerun the M-VIRTUAL-B gate sequence'
        libraries = []
        for name in PACKAGES:
            artifact = next(a for a in artifacts if a.get('reason') == 'compiler-artifact' and a['target']['name'].replace('_', '-') == name and 'cdylib' in a['target']['crate_types'])
            library = Path(next(f for f in artifact['filenames'] if f.endswith('.so')))
            assert library.is_file()
            libraries.append({'path': f'target/{version}/debug/{library.name}'})
        native = (logs / f'native-{short}.log').read_text()
        assert results(native) == [(25, 0, 0), (8, 0, 0)]
        assert results((logs / f'real-host-{short}.log').read_text()) == [(8, 0, 0)]
        assert results((logs / f'r02-{short}.log').read_text()) == [(1, 0, 0)]
        toolchains[version] = {'rustflags': '-Dwarnings' if short == '178' else '', 'cohort': cohort(logs / f'test-{short}.log'),
            'explicitNativeAbiTests': 25, 'realHostTests': 8, 'antigravityLoadingGate': 1,
            'sdkPackageIds': sorted(a['package_id'] for a in sdk_artifacts), 'cdylibs': libraries,
            'realHost': 'Pinned e9c0526 host built with Rust 1.95; same executable for both package toolchains. DLL rebuilt after the exact test executable; missing/older DLLs fail.'}
    mutations = []
    for stem, test, expected in [
        ('v2', 'virtual_native_unsupported_v2_never_downgrades', 'host lacks secure delegation v2'),
        ('release', 'virtual_native_failover_releases_every_target_and_receipt', 'receipt:plain'),
        ('identity', 'virtual_native_failover_releases_every_target_and_receipt', 'get("identity").is_none()'),
        ('terminal', 'virtual_native_missing_terminal_fails_and_releases', 'Provider stream ended without a terminal event'),
        ('f1', 'virtual_native_missing_terminal_fails_and_releases', 'invalid type: null, expected a string'),
        ('f2', 'virtual_native_recorded_pin_is_scope_local_across_reconcile_and_shutdown', 'assertion `left == right` failed'),
        ('f3', 'virtual_native_missing_session_identity_fails_explicitly', 'multiprovider: session not ready'),
        ('inherited', 'virtual_native_inherited_pins_restore_journal_and_preserve_parent', 'assertion `left == right` failed'),
    ]:
        text = (logs / f'mutate-{stem}.log').read_text()
        assert f'test {test} ... FAILED' in text and expected in text
        mutations.append({'guard': stem, 'test': test, 'result': 'killed (exit 101); mutation reverted'})
    for stem, test, expected in [
        ('lease-acquire', 'real_dll_post_turn_scope_lease_compacts', 'lease worker did not drain'),
        ('lease-signal', 'real_dll_post_turn_scope_lease_compacts', 'post-turn compact missing'),
        ('lease-busy', 'real_dll_post_turn_busy_is_skipped', 'busy is normal lost admission'),
    ]:
        text = (logs / f'mutate-{stem}.log').read_text()
        assert f'test {test} ... FAILED' in text and expected in text
        mutations.append({'guard': stem, 'test': test, 'result': 'killed (exit 101); mutation reverted'})
    cancellation_test = 'native::fabric::tests::cancellation_between_context_and_clock_releases_without_poisoning'
    text = (logs / 'mutate-cancel-clock.log').read_text()
    assert f'test {cancellation_test} ... FAILED' in text
    assert 'cancellation is not a timestamp' in text and 'Some(9223372036854775807)' in text
    mutations.append({'guard': 'cancel-clock', 'test': cancellation_test,
        'result': 'killed (exit 101); mutation reverted; saturating arithmetic retained'})
    for short in ('178', '195'):
        assert f'test {cancellation_test} ... ok' in (logs / f'test-{short}.log').read_text()
    for stem in ('reclaim', 'selected'):
        assert results((logs / f'host-{stem}.log').read_text()) == [(1, 0, 0)]
    assert 'cancelled_auth_and_stopped_consumers_release_the_lease_without_penalty ... ok' in (logs / 'test-195.log').read_text()
    for stem, expected in [('stale', 'stale DLL'), ('missing', 'required fresh DLL')]:
        text = (logs / f'{stem}-dll.log').read_text()
        assert 'test result: FAILED' in text and expected in text
    assert 'pass 8' in (logs / 'cursor-node24.log').read_text()
    assert 'fail 0' in (logs / 'cursor-node24.log').read_text()
    assert (logs / 'fixtures-1.sha256').read_bytes() == (logs / 'fixtures-2.sha256').read_bytes()
    fixtures = {}
    for name in ('virtual', 'lift'):
        fixture = ROOT / f'packages/rpi-multiprovider/tests/{name}-reference.json'
        assert fixture.read_bytes() == (logs / f'{name}-1.json').read_bytes() == (logs / f'{name}-2.json').read_bytes()
        fixtures[name] = {'cases': len(json.loads(fixture.read_text())['cases']), **identity(fixture)}
    source_paths = ['Cargo.lock'] + git('ls-files', *(f'packages/{name}' for name in PACKAGES)).splitlines()
    rows = [f'{path} {identity(ROOT / path)["sha256"]}\n' for path in sorted(source_paths)]
    log_names = [f'{kind}-{tc}.{suffix}' for tc in ('178', '195') for kind, suffix in [('artifacts', 'jsonl'), ('artifacts', 'log'), ('test', 'log'), ('native', 'log'), ('r02', 'log')]]
    log_names += [f'mutate-{stem}.log' for stem in ('v2', 'release', 'identity', 'terminal', 'f1', 'f2', 'f3', 'inherited')]
    log_names += ['cursor-node24.log', 'fixtures-1.sha256', 'fixtures-2.sha256',
        'real-host-178.log', 'real-host-195.log', 'fresh-dll-178.log', 'fresh-dll-195.log',
        'stale-dll.log', 'missing-dll.log']
    log_names += [f'mutate-lease-{stem}.log' for stem in ('acquire', 'signal', 'busy')]
    log_names += ['host-build.log', 'host-reclaim.log', 'host-selected.log', 'mutate-cancel-clock.log']
    return {'schemaVersion': 3, 'status': 'provider-gates-pass-scope-lease-and-layered-cancel-cleanup-proven', 'host': host, 'base': base,
        'reviewedCommit': '916fb385bf499be2607c133a4e382d634d648cac', 'registryVersionsChanged': False,
        'sourceSha256': hashlib.sha256(''.join(rows).encode()).hexdigest(), 'toolchains': toolchains,
        'mutations': mutations, 'differentialFixtures': fixtures, 'node24Ipc': {'passed': 8, 'failed': 0, 'evidence': 'historical; not rerun for native review fixes'},
        'formatAndLint': {
            'changedRustFiles': 'Rust 1.95 rustfmt --check --edition 2021 --config skip_children=true passes',
            'fullFmt': 'fails on inherited formatting debt at reviewed baseline; Rust 1.78 rustfmt unavailable',
            'strictClippy': 'inherited failures on 1.78 and 1.95; 1.95 baseline comparison has identical library diagnostic counts',
            'compatClippy': '1.95 all-targets passes with only -A clippy::not_unsafe_ptr_arg_deref; existing warnings remain'},

        'regeneration': 'Final e9c0526 pin: host rebuilt with Rust 1.95; both-toolchain provider builds, cohort, native ABI, fresh-DLL real-host and explicit AG loading gates rerun; virtual/lift fixture outputs and portable report regenerated byte-identically twice. All mutation, missing/stale negative, command/Cursor fixture, Node IPC and complementary host-test evidence historical.',
        'ledger': {
            'M06': 'Native owned catalog binding, publication acknowledgement, healing/deletion/re-registration, global wrapper and command binding proven by ABI tests; existing service lane unchanged.',
            'M07': 'Virtual template/backend effects bound; real host target pool and backend failover proven; 70 upstream differential scenarios retained.',
            'M08': 'Cancellation contract proven in layers: package DLL attempts exactly one release per handle and accepts only success/documented revocation errors; pinned host loaded_cancel_reclaims_tables_before_blocked_callback_and_library_drain proves table reclamation before producer drain and final ownership drain; package lift test compares every account inFlight with baseline after producer drain. Successful post-revocation release RPCs are neither required nor claimed.',
            'M09': 'POST-TURN real-host package compaction proven: pi-fabric v1 + Fabric capability + services.scopeLease.v1; narrow lease acquired before callback return, worker signal, actual host IsIdle/Compact after agent_end, exactly one persisted compaction in selected session, TS notice on diagnostics, busy skip, absent capability inline failover, shutdown release/drain. Pinned host test separately proves child/main isolation and real busy admission.'},
        'layeredHostProofs': {'cancelReclamation': 'loaded_cancel_reclaims_tables_before_blocked_callback_and_library_drain',
            'scopeIsolationAndBusy': 'loaded_scope_service_compaction_after_callback_is_selected_idle_only_and_revocable',
            'toolchain': '1.95.0', 'passed': 2, 'failed': 0},
        'limitations': ['24 inherited ignored cohort tests; no new ignores or assertion weakening.',
            'Strict clippy and whole-workspace fmt have inherited baseline failures; see formatAndLint and Markdown qualification.',
            'Cancellation release RPCs may succeed or return inactive provider continuation / delegation epoch revoked / expired provider continuation. Host tables are checked in the pinned host loaded test, not via an invented package introspection port.',
            'Package real-host transcript test selects main; child/main isolation and actual competing-run busy admission are complementary pinned-host loaded-library tests. Package busy handling injects the exact service error.',
            'ScopeServiceLease grants no UI authority. The exact TS success notice is emitted on stderr, not through an expired invocation UI client. No root fallback is implemented.',
            'No real Fabric extension, live auth/network, host typed-queue load, or full-workspace gate claimed. r02 runs the requested AG library branch; its pre-existing Cursor/legacy branches skip. Linux only.',
            'Older evidence JSON files remain historical and are not promoted by this report. Host ledger was not edited (read-only constraint).',
            "Cohort test counts here (97/62/97 = 256) are the counts recorded for the reviewed commit; docs/provider-parity-verification.md reports the current, larger cohort because regression tests were added after this report's gate run. This file was not regenerated for those additions: its inputs are the historical mutation, pinned-host, real-host and fixture-regeneration logs of reviewed commit 916fb38, which a plain refresh cannot reproduce."],
        'logs': log_names}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, help="checkout to verify (default: this script's repository root)")
    parser.add_argument('--logs', type=Path,
                        help='verifier log directory (default: a per-checkout directory under $TMPDIR, '
                             'overridable with PARITY_EVIDENCE_LOGS)')
    parser.add_argument('--host-checkout', type=Path,
                        help='pinned host artifact checkout; must be at the host commit pinned by Cargo.lock/Cargo.toml')
    parser.add_argument('--write', action='store_true')
    parser.add_argument('--check', action='store_true',
                        help='render into a temporary directory and diff against the tracked report')
    parser.add_argument('--base', default=BASE,
                        help=f'commit the registry dependency baseline is read from (default: {BASE})')
    parser.add_argument('--provenance', type=Path, help='optional nonportable local audit JSON')
    args = parser.parse_args()
    if args.root is not None:
        ROOT = checkout_root(args.root)
    logs = (args.logs or default_logs(ROOT)).resolve()
    base = base_commit(args.base, ROOT)
    host_checkout = args.host_checkout.resolve() if args.host_checkout else None
    if args.provenance:
        args.provenance.write_text(json.dumps(provenance(logs), indent=2) + '\n')
    output = ROOT / 'docs/evidence/m-virtual-b2.json'
    rendered = json.dumps(report(logs, host_checkout, base), indent=2, ensure_ascii=False) + '\n'
    if args.check:
        staged = Path(tempfile.mkdtemp(prefix='m-virtual-b2-')) / output.name
        staged.write_text(rendered)
        committed = output.read_text() if output.exists() else ''
        if committed != rendered:
            diff = list(difflib.unified_diff(committed.splitlines(True), rendered.splitlines(True),
                                             str(output), str(staged)))
            sys.stdout.write(''.join(diff[:200]))
            print(f'{output.relative_to(ROOT)} does not match the verifier logs in {logs}; rerun with --write')
            sys.exit(1)
        print(f'verified {output.relative_to(ROOT)} against {logs}')
    elif args.write:
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(rendered)
        print(f'wrote {output.relative_to(ROOT)}')
    else:
        assert output.read_text() == rendered, 'M-VIRTUAL-B evidence stale; regenerate --write'
        print(f'verified {output.relative_to(ROOT)}')
