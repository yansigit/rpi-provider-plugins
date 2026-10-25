#!/usr/bin/env python3
"""Refresh/check I02 production evidence from local, completed foreground gates.

No builds, credentials, network access, timestamps, or self-hashing output.
Usage: python3 .github/scripts/refresh-i02-evidence.py [--logs DIR] [--write]
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess

ROOT = Path(__file__).resolve().parents[2]
OUTPUT = ROOT / "docs/evidence/i02-production.json"
PIN = "e9c052653975c1d648092ab52e817a0b2f4b575c"
INPUTS = [
    ".github/scripts/refresh-i02-evidence.py",
    "docs/evidence/i02-production.md",
    "Cargo.lock",
    "packages/rpi-antigravity-auth/src/native/vault.rs",
    "packages/rpi-antigravity-auth/src/vault/file_store.rs",
    "packages/rpi-antigravity-auth/src/vault/import_guard.rs",
    "packages/rpi-antigravity-auth/src/vault/source.rs",
    "packages/rpi-antigravity-auth/scripts/native-import-authority.py",
    "packages/rpi-antigravity-auth/tests/i02_production_authority.rs",
    "packages/rpi-cursor-sdk/src/import_guard.rs",
    "packages/rpi-cursor-sdk/src/journal/port.rs",
    "packages/rpi-cursor-sdk/src/native_host.rs",
    "packages/rpi-cursor-sdk/src/native_host/import_authority_tests.rs",
    "packages/rpi-cursor-sdk/worker/src/cursor-cloud-lifecycle.ts",
    "packages/rpi-cursor-sdk/worker/test/rpi/ledgers.test.ts",
    "packages/rpi-cursor-sdk/tests/r01-package-assets.json",
    "packages/rpi-cursor-sdk/tests/r01-package-evidence.json",
]


def sha(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def counts(text):
    rows = re.findall(r"test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored;.*? (\d+) filtered out", text)
    assert rows and "test result: FAILED" not in text, "incomplete/failing gate"
    return dict(zip(["passed", "failed", "ignored", "filtered"], [sum(int(row[i]) for row in rows) for i in range(4)]))


def render(logs):
    sources = {name: sha(ROOT / name) for name in INPUTS}
    manifest = ROOT / "packages/rpi-cursor-sdk/tests/r01-package-assets.json"
    evidence = json.loads((ROOT / "packages/rpi-cursor-sdk/tests/r01-package-evidence.json").read_text())
    assert evidence["assetManifest"]["sha256"] == sha(manifest)
    assert (logs / "fixtures-1.sha256").read_bytes() == (logs / "fixtures-2.sha256").read_bytes()
    gates = {}
    for tc in ["178", "195"]:
        name = f"cohort-{tc}.log"
        result = counts((logs / name).read_text())
        assert result == {"passed": 259, "failed": 0, "ignored": 24, "filtered": 8}, result
        gates[name] = {**result, "sha256": sha(logs / name)}
        name = f"native-{tc}.log"
        result = counts((logs / name).read_text())
        assert result["passed"] == 1
        text = (logs / name).read_text()
        assert "SKIP: set the RPI_TEST_*" not in text and "SKIP: build rpi-cursor-sdk" not in text
        gates[name] = {**result, "sha256": sha(logs / name)}
    for name, expected in [("real-host-178.log", 8), ("real-host-195.log", 8), ("worker-rust-195.log", 12)]:
        result = counts((logs / name).read_text())
        assert result["passed"] == expected
        gates[name] = {**result, "sha256": sha(logs / name)}
    worker = (logs / "worker-vitest.log").read_text()
    assert re.search(r"Tests\s+75 passed \(75\)", worker)
    gates["worker-vitest.log"] = {"passed": 75, "sha256": sha(logs / "worker-vitest.log")}
    ipc = (logs / "ipc.log").read_text()
    assert re.search(r"(?m)^(?:#|ℹ) pass 8$", ipc) and re.search(r"(?m)^(?:#|ℹ) fail 0$", ipc)
    gates["ipc.log"] = {"passed": 8, "sha256": sha(logs / "ipc.log")}
    mutations = {}
    for name, finding in [
        ("mutation-view.log", "left: true"),
        ("mutation-cloud.log", "called `Result::unwrap_err()` on an `Ok` value"),
        ("mutation-h1.log", "antigravity-vault-custody"),
        ("mutation-h1-rollback.log", "rollback_restores_the_login_and_drops_the_stash"),
        ("mutation-h2.log", "active-backup"),
        ("mutation-h2-path.log", "called `Result::unwrap()` on an `Err` value: Storage("),
        ("mutation-m3.log", "packages/rpi-cursor-sdk/src/import_guard.rs:1178"),
        ("mutation-m4.log", "packages/rpi-cursor-sdk/src/journal/tests.rs:301"),
        ("mutation-l6.log", "expected a typed provenance refusal"),
    ]:
        text = (logs / name).read_text()
        assert "test result: FAILED" in text and finding in text
        mutations[name] = sha(logs / name)
    libraries = {}
    for tc in ["178", "195"]:
        directory = Path(f"/home/user/Projects/.targets/i02-guards-{tc}/debug")
        for package in ["cursor_sdk", "antigravity_auth", "multiprovider"]:
            path = directory / f"librpi_{package}.so"
            libraries[str(path)] = sha(path)
    host = Path("/home/user/Projects/.targets/host-e9c0526-195/debug/rpi")
    checkout = Path("/home/user/Projects/pi-rust--host-e9c0526")
    assert subprocess.check_output(["git", "-C", str(checkout), "rev-parse", "HEAD"], text=True).strip() == PIN
    return json.dumps({"ledger": "I02", "hostPin": PIN, "hostBinarySha256": sha(host), "baseline": {"passed": 256, "ignored": 24},
                       "cohortAccounting": "259 + 8 separate fresh-DLL real-host cases = 267; three original lane tests plus eight review-fix tests; no new ignores",
                       "loadedVaultMaliciousMarkersPerToolchain": 15,
                       "sources": sources, "gates": gates, "mutationsReverted": mutations,
                       "fixtures": (logs / "fixtures-1.sha256").read_text(), "libraries": libraries,
                       "limitations": ["No live provider calls; Cursor malicious fixtures use the production Router and worker ledger path, not a real-host Cursor model turn.",
                                       "Both DLL toolchains run the eight real-host cases against the existing Rust 1.95 pinned host; no separate Rust 1.78 host is built.",
                                       "Existing unmarked, byte-identical files cannot prove cross-agent provenance; host import does not copy vault storage."]}, indent=2) + "\n"


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--logs", type=Path, default=Path("/tmp/i02-guards-run/logs"))
    parser.add_argument("--write", action="store_true")
    args = parser.parse_args()
    rendered = render(args.logs)
    if args.write:
        OUTPUT.write_text(rendered)
    else:
        assert OUTPUT.read_text() == rendered, "I02 evidence is stale; refresh with --write after rerunning the gates"
    print("I02 evidence refreshed" if args.write else "I02 evidence fresh")
