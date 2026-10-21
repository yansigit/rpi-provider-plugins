"""Original factory cases through the installed native library and real host auth lock."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile

host = Path(os.environ["RPI_TEST_HOST"]).resolve()
library = Path(os.environ["RPI_TEST_PROVIDER_LIBRARY"]).resolve()
package = Path(__file__).resolve().parents[1]
cases = json.loads((package / "tests/fixtures/vault-startup.json").read_text())["cases"]
checked = refusals = 0
with tempfile.TemporaryDirectory(prefix="rpi-native-vault-") as directory:
    root = Path(directory)
    for index, case in enumerate(cases):
        spec = case["spec"]
        # Mid-callback fault injection remains in the source/slot differential suite.
        if "afterSwap" in spec:
            continue
        agent = root / str(index)
        vault = agent / "antigravity-auth-vault"
        vault.mkdir(parents=True, mode=0o700)
        agent.chmod(0o700)
        for key, name in [("state", "pi-enrollment-state.json"), ("token", "pi-enrollment.json"), ("mode", "pi-mode.json")]:
            if key in spec:
                path = vault / name
                path.write_text(json.dumps(spec[key], separators=(",", ":")))
                path.chmod(0o600)
        auth = agent / "auth.json"
        auth.write_text(spec["auth"])
        auth.chmod(0o600)
        env = {key: value for key, value in os.environ.items() if not key.endswith(("_API_KEY", "_AUTH_TOKEN"))}
        env.update(RPI_CODING_AGENT_DIR=str(agent), RPI_OFFLINE="1", SUBC_CONNECTION_FILE=str(root / "absent-connection.json"))
        def run(*args):
            result = subprocess.run([str(host), *args, "--extension", str(library)], cwd=root, env=env, text=True, capture_output=True, timeout=20)
            assert "fixture-original" not in result.stdout + result.stderr, index
            return result
        methods = run("auth", "methods", "--provider", "google")
        assert methods.returncode == 0, (index, methods.stderr)
        assert [item["id"] for item in json.loads(methods.stdout)] == (["api_key", "oauth"] if case["configuration"]["oauth"] else ["api_key"]), (index, methods.stdout)
        assert auth.read_text() == case["auth"], (index, "credential bytes")
        stash = vault / "google-login-stash.json"
        saved = stash.read_text() if stash.exists() else None
        if saved:
            identity = hashlib.sha256(json.dumps(str(auth), separators=(",", ":")).encode()).hexdigest()
            saved = saved.replace(identity, "<auth-identity>")
            if os.name != "nt":
                assert stash.stat().st_mode & 0o077 == 0
        assert saved == case["stash"], (index, "backup bytes")
        refusal = case["configuration"]["refusal"]
        if refusal:
            result = run("--print", "--no-session", "--provider", "google", "--model", "antigravity-gemini-3.8-flash", "--api-key", "antigravity-vault-custody", "fixture request")
            assert refusal in result.stdout + result.stderr, (index, result.returncode, result.stdout, result.stderr)
            assert auth.read_text() == case["auth"], (index, "refusal changed credential")
            refusals += 1
        checked += 1
print(json.dumps({"factoryCases": checked, "loadedRefusals": refusals, "liveNetwork": False, "midCallbackChanges": "component-tested; not injected into loaded library"}))
