"""Keep a loaded session alive while its stored credential changes; no live service calls."""
import json
import os
from pathlib import Path
import queue
import subprocess
import tempfile
import threading
import time

host = Path(os.environ["RPI_TEST_HOST"]).resolve()
library = Path(os.environ["RPI_TEST_PROVIDER_LIBRARY"]).resolve()
package = Path(__file__).resolve().parents[1]
cases = json.loads((package / "tests/fixtures/vault-host-auth.json").read_text())["cases"]
marker = "antigravity-vault-custody"
checked = 0
with tempfile.TemporaryDirectory(prefix="rpi-native-auth-") as directory:
    root = Path(directory)
    for index, case in enumerate(cases):
        if case["mode"] == "local":
            continue
        agent = root / str(index)
        vault = agent / "antigravity-auth-vault"
        vault.mkdir(parents=True, mode=0o700)
        agent.chmod(0o700)
        (agent / "settings.json").write_text(json.dumps({"retry": {"enabled": False}}))
        auth = agent / "auth.json"
        auth.write_text("{}")
        auth.chmod(0o600)
        if case["mode"] == "custody":
            state = {"version": 1, "phase": "approved", "proposedName": "antigravity-auth-pi", "tokenGeneration": 1, "updatedAt": 1000}
            for name, value in [("pi-enrollment-state.json", state), ("pi-enrollment.json", {"token": "a" * 64, "token_generation": 1})]:
                path = vault / name
                path.write_text(json.dumps(value))
                path.chmod(0o600)
        else:
            (vault / "pi-mode.json").write_text("{")
        env = {key: value for key, value in os.environ.items() if not key.endswith(("_API_KEY", "_AUTH_TOKEN"))}
        env.update(RPI_CODING_AGENT_DIR=str(agent), RPI_OFFLINE="1", GEMINI_API_KEY="ambient-key", SUBC_CONNECTION_FILE=str(root / "absent-connection.json"))
        args = [str(host), "--mode", "rpc", "--no-session", "--no-tools", "--provider", "google", "--model", "antigravity-gemini-3.8-flash", "--extension", str(library)]
        if "apiKey" in case["override"]:
            args += ["--api-key", case["override"]["apiKey"]]
        process = subprocess.Popen(args, cwd=root, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        incoming = queue.Queue()
        errors = []
        reader = threading.Thread(target=lambda: [incoming.put(line) for line in process.stdout], daemon=True)
        error_reader = threading.Thread(target=lambda: errors.extend(process.stderr), daemon=True)
        reader.start()
        error_reader.start()
        messages = []
        def send(value):
            process.stdin.write(json.dumps(value) + "\n")
            process.stdin.flush()
        def until(predicate):
            deadline = time.monotonic() + 15
            while time.monotonic() < deadline:
                try:
                    raw = incoming.get(timeout=0.2)
                except queue.Empty:
                    assert process.poll() is None, (index, process.returncode, errors)
                    continue
                value = json.loads(raw)
                messages.append(value)
                if predicate(value):
                    return value
            raise AssertionError((index, "RPC timeout", messages, errors))
        try:
            send({"type": "get_state", "id": "startup"})
            until(lambda v: v.get("type") == "response" and v.get("id") == "startup")
            credential = case["credential"]
            if credential and credential["type"] == "oauth":
                credential = dict(credential, expires=credential["expires"] // 1000)
            auth.write_text(json.dumps({"google": credential} if credential else {}))
            original = auth.read_bytes()
            send({"type": "prompt", "id": "request", "message": "fixture request"})
            until(lambda v: v.get("type") == "agent_settled")
            text = json.dumps(messages)
            # Both CLIs ignore --api-key ""; the request-level empty option has its own host unit check.
            reference = next(c for c in cases if c["mode"] == case["mode"] and c["credential"] == case["credential"] and not c["override"]) if case["override"].get("apiKey") == "" else case
            value = reference["result"].get("value")
            key = reference["override"].get("apiKey", value["auth"]["apiKey"] if value else None)
            expected = ("provider authentication failed" if value is None else
                        "The native Google credential slot is not synchronized for vault mode. No request was sent." if key != marker else
                        "Vault enrollment or source settings cannot be read. Local credentials remain unavailable." if case["mode"] == "refusing" else
                        "Claustrum scoped operation unavailable")
            assert expected in text, (index, expected, messages, errors)
            assert auth.read_bytes() == original, (index, "request changed stored credential")
            for secret in ["changed-native-key", "changed-access", "changed-refresh", "ambient-key", "explicit-key"]:
                assert secret not in text + "".join(errors), (index, "secret in output")
            checked += 1
        finally:
            process.stdin.close()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
                raise
            reader.join(timeout=1)
            error_reader.join(timeout=1)
print(json.dumps({"integratedReferenceCases": checked, "network": False, "checks": ["explicit-key precedence", "fresh stored credentials", "disabled OAuth", "source marker admission", "no credential mutations"]}))
