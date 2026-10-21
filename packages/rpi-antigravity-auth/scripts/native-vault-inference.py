"""The Rust test supplies the authenticated Subc producer; all HTTP stays local."""
import importlib.util
import json
import os
from pathlib import Path
import tempfile

file = Path(__file__).with_name('native-inference.py')
spec = importlib.util.spec_from_file_location('native_inference', file)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
case = os.environ['RPI_VAULT_SEND_CASE']
with tempfile.TemporaryDirectory(prefix='rpi-native-vault-inference-', dir=os.environ['RPI_PRIVATE_TMP']) as root:
    message, _ = module.scenario(Path(root), case, module.frame({'candidates': [{'content': {'parts': [{'text': 'vault hello'}]}, 'finishReason': 'STOP'}]}), vault_case=case)
    if case in ('success', 'rotated'):
        assert message['content'] == [{'type': 'text', 'text': 'vault hello'}], message
        assert message['stopReason'] == 'stop', message
    else:
        assert message['stopReason'] == 'error', message
        if case in ('same-version', 'terminal-401'):
            assert 'HTTP 401' in message['errorMessage'], message
print(json.dumps({'case': case, 'result': 'passed', 'liveInference': False}))
