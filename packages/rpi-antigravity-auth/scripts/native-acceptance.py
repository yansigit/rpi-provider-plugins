"""Offline acceptance through the actual rpi executable and installed native library."""
import json
import os
from pathlib import Path
import subprocess
import tempfile

host = Path(os.environ["RPI_TEST_HOST"]).resolve()
library = Path(os.environ["RPI_TEST_PROVIDER_LIBRARY"]).resolve()
assert host.is_file() and library.is_file()
package = Path(__file__).resolve().parents[1]
models = json.loads((package / "src/catalog.json").read_text())["catalog"]
with tempfile.TemporaryDirectory(prefix="rpi-antigravity-native-") as directory:
    root = Path(directory)
    agent = root / "agent"
    agent.mkdir(mode=0o700)
    env = dict(os.environ, RPI_CODING_AGENT_DIR=str(agent), RPI_OFFLINE="1")
    for name in list(env):
        if name.endswith(("_API_KEY", "_AUTH_TOKEN")):
            env.pop(name)
    def run(*args, input=None):
        result = subprocess.run([str(host), *args, "--extension", str(library)], cwd=root,
                                env=env, input=input, text=True, capture_output=True, timeout=20)
        assert "fixture-private-key" not in result.stdout + result.stderr
        return result
    methods = run("auth", "methods", "--provider", "google")
    assert methods.returncode == 0, methods.stderr
    assert json.loads(methods.stdout) == [
        {"id": "api_key", "name": "Gemini API key", "kind": "api_key"},
        {"id": "oauth", "name": "Google Antigravity (CortexKit)", "kind": "oauth"},
    ]
    listing = run("--list-models", "google")
    assert listing.returncode == 0, listing.stderr
    rows = [line.split() for line in listing.stdout.splitlines() if line.split()[:1] == ["google"]]
    # Native table presentation sorts IDs; the declaration order is checked in tests/catalog.rs.
    assert [row[1] for row in rows] == sorted(model["id"] for model in models), listing.stdout
    assert all(row[-1] == "yes" for row in rows)
    login = run("auth", "login", "--provider", "google", "--method", "api_key", input="fixture-private-key\n")
    assert login.returncode == 0, login.stderr
    credential = json.loads((agent / "auth.json").read_text())
    assert credential["google"]["type"] == "api_key"
    assert credential["google"]["key"] == "fixture-private-key"
    if os.name != "nt":
        assert (agent / "auth.json").stat().st_mode & 0o077 == 0
    check = run("auth", "check", "--provider", "google", "--json")
    assert check.returncode == 0, check.stderr
    assert json.loads(check.stdout)["ready"] is True
    logout = run("auth", "logout", "--provider", "google")
    assert logout.returncode == 0, logout.stderr
    assert "google" not in json.loads((agent / "auth.json").read_text())
print(json.dumps({"checks": ["native library registration", "offline catalog replacement", "auth discovery", "API-key login", "private durable credential", "local status", "logout"], "network": False}))
