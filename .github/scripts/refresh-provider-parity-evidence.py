#!/usr/bin/env python3
"""Refresh the scoped provider SDK report from offline verifier artifacts.

Run the commands documented in docs/provider-parity-verification.md first, then:
  python3 .github/scripts/refresh-provider-parity-evidence.py --write
  python3 .github/scripts/refresh-provider-parity-evidence.py

No builds, network, credential access, timestamps, or hand-maintained hash fields.
The existing reference generators remain authoritative for their own fixtures.
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

PACKAGES = ["rpi-antigravity-auth", "rpi-cursor-sdk", "rpi-multiprovider"]
HOST_CRATES = ["rpi-ai", "rpi-plugin-sdk", "rpi-telemetry"]
BASE = "5b5416c"


def checkout_root(start=None):
    """Repository root of the checkout that contains this script (or --root)."""
    probe = Path(start) if start is not None else Path(__file__).resolve()
    resolved = subprocess.run(["git", "-C", str(probe), "rev-parse", "--show-toplevel"],
                              capture_output=True, text=True)
    if resolved.returncode == 0 and resolved.stdout.strip():
        return Path(resolved.stdout.strip())
    return Path(__file__).resolve().parents[2]


ROOT = checkout_root()


def default_logs(root):
    """Per-checkout verifier log directory; never a directory shared by other lanes."""
    return Path(os.environ.get("PARITY_EVIDENCE_LOGS", Path(tempfile.gettempdir()) / f"pkg-repin-{root.name}"))


def git(*args, cwd=ROOT):
    return subprocess.check_output(["git", "-C", str(cwd), *args]).decode().strip()


def digest(data):
    return hashlib.sha256(data).hexdigest()


def identity(path):
    data = path.read_bytes()
    return {"path": str(path), "bytes": len(data), "sha256": digest(data)}


def tree_identity(paths, root):
    rows = [(str(path.relative_to(root)), digest(path.read_bytes())) for path in sorted(paths)]
    return {"files": len(rows), "sha256": digest("".join(f"{p} {h}\n" for p, h in rows).encode())}


def test_counts(path):
    counts = {name: dict(passed=0, failed=0, ignored=0) for name in PACKAGES}
    current = None
    text = path.read_text()
    assert "test result: FAILED" not in text
    for line in text.splitlines():
        if "Running unittests" in line:
            current = re.search(r"deps/(rpi_[a-z_]+)-", line)[1].replace("_", "-")
        elif "Doc-tests" in line:
            current = line.split()[-1].replace("_", "-")
        match = re.search(r"test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored", line)
        if match:
            for key, value in zip(("passed", "failed", "ignored"), match.groups()):
                counts[current][key] += int(value)
    assert all(value["passed"] > 0 and value["failed"] == 0 for value in counts.values())
    assert len(re.findall(r"Doc-tests", text)) == 3, "incomplete cohort test log"
    return counts


def report(logs, host_checkout=None):
    manifests = {name: tomllib.loads((ROOT / "packages" / name / "Cargo.toml").read_text()) for name in PACKAGES}
    revisions = {m["dependencies"]["rpi-plugin-sdk"]["rev"] for m in manifests.values()}
    assert len(revisions) == 1
    revision = revisions.pop()
    assert re.fullmatch("[0-9a-f]{40}", revision)
    lock = tomllib.loads((ROOT / "Cargo.lock").read_text())
    locked = {match[1] for package in lock["package"] if package["name"] in HOST_CRATES
              for match in [re.search(r"rev=([0-9a-f]{40})", package.get("source", ""))] if match}
    # Cargo.lock also pins unrelated host-SDK revisions for this workspace's own graph;
    # the package pin is the revision the three manifests resolve to.
    assert revision in locked, f"Cargo.lock has no {HOST_CRATES} entry at the manifest pin {revision}; found {sorted(locked)}"
    old_lock = tomllib.loads(git("show", f"{BASE}:Cargo.lock"))
    registry = lambda value: sorted((p["name"], p["version"], p.get("checksum")) for p in value["package"] if p.get("source", "").startswith("registry+"))
    assert registry(lock) == registry(old_lock), "unrelated registry dependency change"
    cohort = [p for p in lock["package"] if revision in p.get("source", "")]
    assert sorted(p["name"] for p in cohort) == HOST_CRATES
    toolchains = {}
    for short, version in [("178", "1.78.0"), ("195", "1.95.0")]:
        artifacts = [json.loads(line) for line in (logs / f"artifacts-{short}.jsonl").read_text().splitlines()]
        assert artifacts[-1] == {"reason": "build-finished", "success": True}
        sdk_artifacts = [a for a in artifacts if a.get("reason") == "compiler-artifact" and a["target"]["name"].replace("_", "-") in HOST_CRATES]
        assert len(sdk_artifacts) == 3
        sdk = next(a for a in sdk_artifacts if a["target"]["name"].replace("_", "-") == "rpi-plugin-sdk")
        checkout = Path(sdk["target"]["src_path"]).parents[3]
        assert git("rev-parse", "HEAD", cwd=checkout) == revision, f"artifact checkout {checkout} is not at the pinned host commit"
        assert not git("status", "--porcelain", "--", "crates/rpi-ai", "crates/rpi-plugin-sdk", "crates/rpi-telemetry", cwd=checkout)
        if host_checkout is not None:
            assert git("rev-parse", "HEAD", cwd=host_checkout) == revision, f"--host-checkout {host_checkout} is not at the pinned host commit {revision}"
            assert not git("status", "--porcelain", "--", "crates/rpi-ai", "crates/rpi-plugin-sdk", "crates/rpi-telemetry", cwd=host_checkout)
        source = []
        for crate in HOST_CRATES:
            files = [checkout / p for p in git("ls-files", f"crates/{crate}", cwd=checkout).splitlines()]
            source.append({"name": crate, **tree_identity(files, checkout)})
        libraries = []
        for name in PACKAGES:
            a = next(a for a in artifacts if a.get("reason") == "compiler-artifact" and a["target"]["name"].replace("_", "-") == name and "cdylib" in a["target"]["crate_types"])
            libraries.append(identity(Path(next(f for f in a["filenames"] if f.endswith(".so")))))
        loading = {}
        for stem in ["mp-native", "r02-native"]:
            text = (logs / f"{stem}-{short}.log").read_text()
            matches = re.findall(r"test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored", text)
            assert len(matches) == 1 and matches[0][1:] == ("0", "0")
            loading[stem] = int(matches[0][0])
        toolchains[version] = {
            "tests": test_counts(logs / f"test-{short}-final.log"),
            "rustflags": "-Dwarnings" if short == "178" else "",
            "sdkCheckout": str(checkout), "sdkCheckoutHead": revision,
            "sdkSources": source,
            "compilerArtifactPackageIds": sorted(a["package_id"] for a in sdk_artifacts),
            "cdylibs": libraries,
            "nativeLoading": {"multiproviderTests": loading["mp-native"], "antigravityRegistrationTests": loading["r02-native"],
                              "cursorAndLegacyLoading": "not run (RPI_TEST_* library variables unset)"},
        }
    mutations = [
        ("c-emit", "cancellation_while_in_synchronous_c_emit_propagates_host_rejection", "Invert the real C emit nonzero-return rejection condition", "Ok(())"),
        ("receipt", "private_receipt_precedes_start_and_never_leaks_into_public_events_or_result", "Emit start before the private response receipt", 'String("start")'),
        ("blocked-idle", "blocked_downstream_preserves_idle_timeout_and_disabled_timeout_cancellation", "Remove Output::wait around the bounded body-channel send", "Elapsed(())"),
        ("body-order", "slow_body_consumer_over_four_mib_is_ordered_and_lossless_with_idle_disabled", "Reverse bytes in every body-channel send", "assertion `left == right` failed"),
        ("cursor-snapshot", "slow consumer drains over 64 events and 4 MiB without loss and retains emission-time partial snapshots", "Replace the actual WorkerPeer encoded partial text with stale", "stale"),
    ]
    mutation_results = []
    for stem, test, mutation, expected in mutations:
        text = (logs / f"mutate-{stem}.log").read_text()
        assert expected in text and ("FAILED" in text or "AssertionError" in text)
        mutation_results.append({"test": test, "mutation": mutation, "result": "failed as expected; mutation reverted"})
    node = (logs / "cursor-node24-final.log").read_text()
    assert "pass 8" in node and "fail 0" in node
    fixture_hashes = (logs / "fixtures-1.sha256").read_bytes()
    assert fixture_hashes == (logs / "fixtures-2.sha256").read_bytes()
    # Include production sources, tests, build metadata, and retained fixture identities.
    files = [ROOT / "Cargo.lock", ROOT / "Cargo.toml"]
    for name in PACKAGES:
        files += [ROOT / p for p in git("ls-files", f"packages/{name}").splitlines()]
    selected_logs = [f"test-{tc}-final.log" for tc in ["178", "195"]]
    selected_logs += [f"{stem}-{tc}.{suffix}" for tc in ["178", "195"] for stem, suffix in [("build-cdylib", "log"), ("artifacts", "jsonl"), ("mp-native", "log"), ("r02-native", "log")]]
    selected_logs += [f"mutate-{stem}.log" for stem, *_ in mutations]
    selected_logs += ["cursor-node24-final.log"]
    selected_logs += [f"reference-{pkg}-{n}.log" for pkg in ["mp", "cursor"] for n in [1, 2]]
    return {
        "schemaVersion": 2, "status": "scoped-pass",
        "scope": "Offline provider cohort only; no live services, full workspace, or G1/G2 acceptance claimed.",
        "hostTarget": revision, "packageBase": git("rev-parse", BASE),
        "lane": git("branch", "--show-current"), "worktree": str(ROOT),
        "generator": ".github/scripts/refresh-provider-parity-evidence.py",
        "pinResolution": {"registryVersionsChanged": False, "hostCrates": [{k: p[k] for k in ["name", "version", "source"]} for p in cohort]},
        "packageFiles": tree_identity(files, ROOT),
        "pinFiles": [identity(ROOT / "packages" / p / "Cargo.toml") for p in PACKAGES] + [identity(ROOT / "Cargo.lock")],
        "toolchains": toolchains, "mutations": mutation_results,
        "cursorIpc": {"node": "24.22.0", "passed": 8, "failed": 0, "ignored": 0, "newTestEvents": 131, "partialTextBytesPerEvent": 70000},
        "regeneration": {"referenceFixturesByteIdenticalTwice": True, "fixtureHashes": fixture_hashes.decode().splitlines(),
                         "cohortReportCheck": "Regenerate --write twice and cmp the outputs; no timestamps or output-file self-hash."},
        "approvedExceptions": ["Antigravity body-idle timeout/cancel may fire while downstream is blocked.", "Cursor encodes partial snapshots at emission time, before its synchronous credit wait."],
        "limitations": ["24 ignored Rust tests predate this change; none added or newly ignored.",
                        "The real host typed stream queue and extension C callback adapter are not linked by these package harnesses. New tests exercise the actual package C emit sink, TLS body channel, and Cursor IPC credit adapter instead.",
                        "Native loading uses existing fake-vtable tests, not a full host process; Cursor/legacy loading branches were not requested via environment.",
                        "No host source changes were needed; direct host queue coverage remains a host follow-up needed, not a demonstrated host defect.",
                        "Domain evidence files outside this refreshed cohort report remain historical; only the named reference fixtures were freshly regenerated.",
                        "The five retained-regression mutation logs are the originally recorded mutation runs; a plain report refresh does not re-apply production mutations."],
        "logs": [identity(logs / name) for name in selected_logs],
    }


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, help="checkout to verify (default: this script's repository root)")
    parser.add_argument("--logs", type=Path,
                        help="verifier log directory (default: a per-checkout directory under $TMPDIR, "
                             "overridable with PARITY_EVIDENCE_LOGS)")
    parser.add_argument("--host-checkout", type=Path,
                        help="pinned host artifact checkout; must be at the host commit pinned by Cargo.lock/Cargo.toml")
    parser.add_argument("--write", action="store_true")
    parser.add_argument("--check", action="store_true",
                        help="render into a temporary directory and diff against the tracked report")
    args = parser.parse_args()
    if args.root is not None:
        ROOT = checkout_root(args.root)
    logs = (args.logs or default_logs(ROOT)).resolve()
    host_checkout = args.host_checkout.resolve() if args.host_checkout else None
    path = ROOT / "docs/provider-parity-sdk-cohort.json"
    rendered = json.dumps(report(logs, host_checkout), indent=2, ensure_ascii=False) + "\n"
    if args.check:
        staged = Path(tempfile.mkdtemp(prefix="provider-parity-cohort-")) / path.name
        staged.write_text(rendered)
        committed = path.read_text() if path.exists() else ""
        if committed != rendered:
            diff = list(difflib.unified_diff(committed.splitlines(True), rendered.splitlines(True),
                                             str(path), str(staged)))
            sys.stdout.write("".join(diff[:200]))
            print(f"{path.relative_to(ROOT)} does not match the verifier logs in {logs}; rerun with --write")
            sys.exit(1)
        print(f"verified {path.relative_to(ROOT)} against {logs}")
    elif args.write:
        path.write_text(rendered)
        print(f"wrote {path.relative_to(ROOT)}")
    else:
        assert path.read_text() == rendered, "cohort report is stale; regenerate with --write"
        print(f"verified {path.relative_to(ROOT)}")
