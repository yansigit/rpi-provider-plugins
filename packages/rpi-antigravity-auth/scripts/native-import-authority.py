"""I02 malicious provenance through a fresh DLL and the pinned real host; offline."""
import copy
import json
import os
from pathlib import Path
import subprocess
import tempfile

host = Path(os.environ["RPI_TEST_HOST"]).resolve()
library = Path(os.environ["RPI_TEST_ANTIGRAVITY_LIBRARY"]).resolve()
notice = "Vault state that was not enrolled for this agent cannot serve Antigravity. Request vault access again; no imported custody, backup, token or local login is activated."
base = {
    "pi-mode.json": {"version": 1, "mode": "custody"},
    "pi-enrollment-state.json": {"version": 1, "phase": "approved", "proposedName": "antigravity-auth-pi", "approvedName": "antigravity-auth-pi", "tokenGeneration": 2, "updatedAt": 1729000000000},
    "pi-enrollment.json": {"token": "a" * 64, "token_generation": 2},
}
checked = 0
with tempfile.TemporaryDirectory(prefix="i02-loaded-") as directory:
    root = Path(directory)
    for target in [*base, "google-login-stash.json", "pi-enrollment.json.bak"]:
        for marker in [{"imported": False}, {"imported": {"source": "pi", "sessionVersion": 999}}, {"importedFrom": "pi"}]:
            agent = root / str(checked)
            vault = agent / "antigravity-auth-vault"
            vault.mkdir(parents=True, mode=0o700)
            agent.chmod(0o700)
            files = copy.deepcopy(base)
            files.setdefault(target, {}).update(marker)
            for name, value in files.items():
                path = vault / name
                path.write_text(json.dumps(value))
                path.chmod(0o600)
            # The imported directory must never replace or stash an existing Pi login.
            auth = agent / "auth.json"
            original = '{"google":{"type":"oauth","refresh":"fixture-original","access":"fixture-access","expires":0}}'
            auth.write_text(original)
            auth.chmod(0o600)
            before = {path.name: path.read_bytes() for path in vault.iterdir()}
            env = {key: value for key, value in os.environ.items() if not key.endswith(("_API_KEY", "_AUTH_TOKEN"))}
            env.update(RPI_CODING_AGENT_DIR=str(agent), RPI_OFFLINE="1", SUBC_CONNECTION_FILE=str(root / "absent.json"))
            result = subprocess.run([str(host), "--print", "--no-session", "--no-tools", "--provider", "google", "--model", "antigravity-gemini-3.8-flash", "--api-key", "antigravity-vault-custody", "--extension", str(library), "fixture request"], cwd=root, env=env, text=True, capture_output=True, timeout=25)
            output = result.stdout + result.stderr
            assert notice in output, (target, marker, result.returncode, output)
            assert "fixture-original" not in output, (target, "credential disclosed")
            assert auth.read_text() == original, (target, "credential changed")
            assert all((vault / name).read_bytes() == value for name, value in before.items()), (target, "imported storage changed")
            assert not (vault / "google-login-stash.json").exists() or target == "google-login-stash.json", target
            checked += 1
print(json.dumps({"loadedMaliciousMarkers": checked, "liveNetwork": False, "existingLoginUnchanged": True}))
