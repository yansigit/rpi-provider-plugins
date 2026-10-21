"""Original menu traces through a real loaded library; no live vault or inference."""
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
fixtures = json.loads((package / "tests/fixtures/vault-menu.json").read_text())["cases"]
slots = json.loads((package / "tests/fixtures/vault-setup.json").read_text())["slots"]
with tempfile.TemporaryDirectory(prefix="rpi-menu-", dir=os.environ.get("RPI_PRIVATE_TMP")) as directory:
    root = Path(directory)
    agent = root / "agent"
    agent.mkdir(mode=0o700)
    auth = agent / "auth.json"
    auth.write_text("{}")
    auth.chmod(0o600)
    env = {key: value for key, value in os.environ.items() if not key.endswith(("_API_KEY", "_AUTH_TOKEN"))}
    env.update(RPI_CODING_AGENT_DIR=str(agent), RPI_OFFLINE="1", SUBC_CONNECTION_FILE=str(root / "absent.json"))
    process = subprocess.Popen([str(host), "--mode", "rpc", "--no-session", "--no-tools", "--provider", "google", "--model", "antigravity-gemini-3.8-flash", "--extension", str(library)], cwd=root, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    incoming, errors, messages = queue.Queue(), [], []
    reader = threading.Thread(target=lambda: [incoming.put(line) for line in process.stdout], daemon=True)
    error_reader = threading.Thread(target=lambda: errors.extend(process.stderr), daemon=True)
    reader.start()
    error_reader.start()
    def send(value):
        process.stdin.write(json.dumps(value) + "\n")
        process.stdin.flush()
    def until(predicate):
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            try:
                raw = incoming.get(timeout=0.2)
            except queue.Empty:
                assert process.poll() is None, (process.returncode, errors)
                continue
            value = json.loads(raw)
            messages.append(value)
            assert value.get("type") != "agent_start", "menu started an agent turn"
            if predicate(value):
                return value
        raise AssertionError(("RPC timeout", messages[-8:], errors))
    def response(identity):
        return until(lambda v: v.get("type") == "response" and v.get("id") == identity)
    def dialog():
        return until(lambda v: v.get("type") == "ui_dialog_request")
    def answer(value, choice):
        ui = value["ui"]["nativeDialog"]
        send({"type": "ui_dialog_response", "requestId": value["requestId"], "cancelled": choice is None, "answer": {"type": "select", "values": [ui["choices"][choice]["value"]] if choice is not None else []}})
    walks = inspected = 0
    try:
        until(lambda v: v.get("type") == "ready")
        send({"type": "get_commands", "id": "commands"})
        commands = response("commands")
        assert any(c["name"] == "antigravity" and c["native"] for c in commands["data"])
        for case in fixtures:
            if "uiTrace" not in case or case["spec"]["status"]["mode"] != "local" or any(t[0] != "select" for t in case["uiTrace"]):
                continue
            # The request-access walk mutates setup and has its own framed-vault acceptance.
            if any("Request vault access" in t[2] and a == 0 for t, a in zip(case["uiTrace"], case["answers"])):
                continue
            send({"type": "extension_command", "id": "walk", "name": "antigravity", "args": "ignored like Pi"})
            for i, trace in enumerate(case["uiTrace"]):
                value = dialog()
                ui = value["ui"]["nativeDialog"]
                assert ["select", ui["title"], [c["label"] for c in ui["choices"]]] == trace
                send({"type": "ping", "id": "responsive"})
                assert response("responsive")["success"]
                answer(value, case["answers"][i] if i < len(case["answers"]) else None)
            assert response("walk")["success"]
            walks += 1
        for case in slots:
            credential = case["hostSlot"]
            auth.write_text(json.dumps({"google": credential}))
            original = auth.read_bytes()
            send({"type": "extension_command", "id": "inspect", "name": "antigravity"})
            first = dialog()
            answer(first, 0)
            accounts = dialog()
            expected = next(c["result"]["menu"]["sections"][0] for c in fixtures if "request" not in c and "answers" not in c and c["spec"]["status"] == {"mode": "local", "enrollment": {"state": "idle"}, "hostSlot": case["result"]})
            assert accounts["ui"]["nativeDialog"]["title"] == "Accounts\n" + "\n".join(expected["lines"])
            answer(accounts, None)
            answer(dialog(), None)
            assert response("inspect")["success"]
            assert auth.read_bytes() == original, "inspection rewrote credentials"
            inspected += 1
        record = b'{"google":{"type":"oauth","refresh":"fixture-encoding-refresh"}}'
        for raw, expected in [(b"{", "empty"), (b"\xef\xbb\xbf" + record, "login"), (b"\xef\xbb\xbf\xef\xbb\xbf" + record, "empty"), (record[:-1] + b',"other":"\xff"}', "login")]:
            auth.write_bytes(raw)
            send({"type": "extension_command", "id": "encoding", "name": "antigravity"})
            first = dialog()
            answer(first, 0)
            accounts = dialog()
            line = "Signed in with Pi's own login." if expected == "login" else "Not signed in. Run Pi's /login to sign in."
            assert line in accounts["ui"]["nativeDialog"]["title"]
            answer(accounts, None)
            answer(dialog(), None)
            assert response("encoding")["success"]
            assert auth.read_bytes() == raw
        # EOF while awaiting a dialog must drain the owned callback and worker runtime.
        send({"type": "extension_command", "id": "eof", "name": "antigravity"})
        dialog()
    finally:
        process.stdin.close()
        try:
            process.wait(timeout=15)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()
            raise
        reader.join(timeout=1)
        error_reader.join(timeout=1)
    assert process.returncode == 0, errors
    text = json.dumps(messages) + "".join(errors)
    for secret in ["claustrum-tombstone:v1", "requestSecret", "refreshToken", "fixture-encoding-refresh"]:
        assert secret not in text, "private material in command output"
    print(json.dumps({"loadedMenuWalks": walks, "hostSlotInspections": inspected, "rawStoreEncodingCases": 4, "eof": "passed", "liveNetwork": False}))
