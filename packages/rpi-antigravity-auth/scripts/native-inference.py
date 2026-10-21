"""Production native-library requests through a local TLS-terminating test proxy."""
import gzip
import json
import os
from pathlib import Path
import socketserver
import ssl
import subprocess
import tempfile
import threading

package = Path(__file__).resolve().parents[1]
host = Path(os.environ['RPI_TEST_HOST']).resolve()
library = Path(os.environ['RPI_TEST_PROVIDER_LIBRARY']).resolve()
certificate = package / 'tests/fixtures/tls/agy-server-cert.pem'
key = package / 'tests/fixtures/tls/agy-server-key.pem'
access = 'fixture-private-access'
project = 'fixture-private-managed'


def frame(value):
    return ('data: ' + json.dumps(value, separators=(',', ':')) + '\n\n').encode()


class Proxy(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


def scenario(root, name, response, status=200, compressed=False, packed=True, vault_case=None):
    requests, errors = [], []
    tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    tls.load_cert_chain(certificate, key)

    class Handler(socketserver.StreamRequestHandler):
        def handle(self):
            try:
                self.connection.settimeout(5)
                connect = self.rfile.readline().decode().strip()
                assert connect.startswith('CONNECT '), connect
                authority = connect.split(' ')[1]
                assert authority in ('daily-cloudcode-pa.sandbox.googleapis.com:443',
                                     'daily-cloudcode-pa.googleapis.com:443',
                                     'cloudcode-pa.googleapis.com:443'), authority
                while self.rfile.readline() != b'\r\n':
                    pass
                self.wfile.write(b'HTTP/1.1 200 Connection established\r\n\r\n')
                self.wfile.flush()
                with tls.wrap_socket(self.connection, server_side=True) as stream:
                    incoming = stream.makefile('rb')
                    line = incoming.readline().decode().strip()
                    headers = []
                    while (header := incoming.readline()) != b'\r\n':
                        assert header, 'truncated request'
                        headers.append(header.decode().rstrip('\r\n').split(': ', 1))
                    values = {k.lower(): v for k, v in headers}
                    if values.get('transfer-encoding') == 'chunked':
                        body = bytearray()
                        while (size := int(incoming.readline().strip(), 16)):
                            body.extend(incoming.read(size))
                            assert incoming.read(2) == b'\r\n'
                        assert incoming.read(2) == b'\r\n'
                        body = bytes(body)
                    else:
                        body = incoming.read(int(values['content-length']))
                    requests.append({'authority': authority, 'line': line,
                                     'headers': headers, 'body': json.loads(body)})
                    if ':loadCodeAssist' in line:
                        payload = json.dumps({'cloudaicompanionProject': project}).encode()
                        result_status, encoding = 200, b''
                    else:
                        payload = gzip.compress(response) if compressed else response
                        result_status = (401 if vault_case in ('same-version', 'terminal-401') or (vault_case == 'rotated' and len(requests) == 1) else status)
                        encoding = b'Content-Encoding: gzip\r\n' if compressed else b''
                    stream.sendall(f'HTTP/1.1 {result_status} Fixture\r\nContent-Length: {len(payload)}\r\n'.encode()
                                   + encoding + b'Content-Type: text/event-stream\r\nConnection: close\r\n\r\n')
                    # Exercise framing across socket reads without changing SSE boundaries.
                    for offset in range(0, len(payload), 7):
                        stream.sendall(payload[offset:offset + 7])
            except Exception as error:
                errors.append(repr(error))

    agent = root / name
    agent.mkdir(mode=0o700)
    credentials = {'google': {'type': 'oauth', 'access': access,
                              'refresh': 'fixture-private-refresh' + ('|fixture-private-project|' + project if packed else ''),
                              'expires': 4102444800},
                   'other': {'type': 'api_key', 'key': 'untouched-fixture-key'}}
    if vault_case:
        vault = agent / 'antigravity-auth-vault'
        vault.mkdir(mode=0o700)
        for filename, value in [('pi-enrollment-state.json', {'version': 1, 'phase': 'approved', 'proposedName': 'antigravity-auth-pi', 'tokenGeneration': 1, 'updatedAt': 1000}),
                                ('pi-enrollment.json', {'token': 'a' * 64, 'token_generation': 1})]:
            file = vault / filename
            file.write_text(json.dumps(value))
            file.chmod(0o600)
    original = json.dumps(credentials)
    (agent / 'auth.json').write_text(original)
    (agent / 'settings.json').write_text(json.dumps({'retry': {'enabled': False}, 'compaction': {'enabled': False}}))
    (agent / 'system.md').write_text('Be concise.')
    env = dict(os.environ, RPI_CODING_AGENT_DIR=str(agent), RPI_OFFLINE='1', NODE_EXTRA_CA_CERTS=str(certificate))
    for key_name in list(env):
        if key_name.endswith(('_API_KEY', '_AUTH_TOKEN')) or key_name in ('NODE_OPTIONS', 'NODE_USE_SYSTEM_CA'):
            env.pop(key_name)
    for key_name in ['HTTPS_PROXY', 'https_proxy', 'ALL_PROXY', 'all_proxy', 'NO_PROXY', 'no_proxy']:
        env.pop(key_name, None)
    with Proxy(('127.0.0.1', 0), Handler) as proxy:
        thread = threading.Thread(target=proxy.serve_forever, daemon=True)
        thread.start()
        env['HTTPS_PROXY'] = f'http://127.0.0.1:{proxy.server_address[1]}'
        try:
            output = subprocess.run([str(host), '--extension', str(library), '--provider', 'google',
                                     '--model', 'antigravity-gemini-3.8-flash', '--thinking', 'low',
                                     '--mode', 'json', '--print', '--no-session', '--no-tools', 'Say hello.'],
                                    cwd=agent, env=env, text=True, capture_output=True, timeout=25)
        finally:
            proxy.shutdown()
            thread.join()
    assert not errors, errors
    if vault_case:
        after = json.loads((agent / 'auth.json').read_text())
        assert after['google'] == {'type': 'api_key', 'key': 'antigravity-vault-custody'}
        assert after['other'] == credentials['other']
        assert json.loads((agent / 'antigravity-auth-vault/google-login-stash.json').read_text())['entry'] == credentials['google']
    else:
        assert (agent / 'auth.json').read_text() == original
    for secret in [access, project, 'fixture-private-refresh', 'fixture-private-project', 'untouched-fixture-key']:
        assert secret not in output.stdout + output.stderr, f'{name}: private credential in output'
    expected_sends = (2 if vault_case in ('rotated', 'terminal-401') else 0 if vault_case in ('identity', 'project-missing') else 1) if vault_case else (1 if packed else 2)
    assert len(requests) == expected_sends, (name, requests, output.stdout, output.stderr)
    for index, request in enumerate(requests):
        expected_access = access + f'-{index + 1}' if vault_case else access
        assert dict(request['headers'])['Authorization'] == f'Bearer {expected_access}'
        if vault_case:
            assert request['body']['project'] == project + f'-{index + 1}'
        assert [pair[0] for pair in request['headers']] == ['Host', 'User-Agent',
                    'Transfer-Encoding' if ':streamGenerateContent' in request['line'] else 'Content-Length',
                    'Authorization', 'Content-Type', 'Accept-Encoding']
    if requests:
        request = requests[-1]
        assert request['line'] == 'POST /v1internal:streamGenerateContent?alt=sse HTTP/1.1'
        if not vault_case:
            assert request['body']['project'] == project
        assert request['body']['requestType'] == 'agent'
        assert request['body']['request']['generationConfig']['thinkingConfig'] == {'includeThoughts': True, 'thinkingBudget': 1000}, request['body']['request']['generationConfig']
    records = [json.loads(line) for line in output.stdout.splitlines() if line.startswith('{')]
    assistants = [entry['message'] for entry in records if entry.get('type') == 'message_end'
                  and entry.get('message', {}).get('role') == 'assistant']
    assert len(assistants) == 1, (name, records, output.stderr)
    return assistants[0], records


if __name__ == '__main__':
    with tempfile.TemporaryDirectory(prefix='rpi-native-inference-', dir=os.environ.get('RPI_PRIVATE_TMP')) as directory:
        root = Path(directory)
        terminal = frame({'candidates': [{'content': {'parts': [{'text': 'hello', 'thoughtSignature': 'fixture-text-signature'}]},
                                         'finishReason': 'STOP'}],
                          'usageMetadata': {'promptTokenCount': 20, 'cachedContentTokenCount': 5,
                                            'candidatesTokenCount': 8, 'thoughtsTokenCount': 3, 'totalTokenCount': 999}})
        for name, compressed, packed in [('plain', False, True), ('gzip', True, True), ('discovery', False, False)]:
            message, records = scenario(root, name, terminal, compressed=compressed, packed=packed)
            assert message['stopReason'] == 'stop', message
            assert message['content'] == [{'type': 'text', 'text': 'hello', 'textSignature': 'fixture-text-signature'}], message
            assert message['usage']['input'] == 15 and message['usage']['cacheRead'] == 5
            assert message['usage']['output'] == 11 and message['usage']['totalTokens'] == 31
            assert message['durationMs'] >= 0
        message, records = scenario(root, 'refusal', f'{access} at {project}'.encode(), status=403)
        assert message['stopReason'] == 'error' and 'HTTP 403' in message['errorMessage'], message
    print(json.dumps({'scenarios': 4, 'physicalRequests': 5, 'credentialsUnchanged': True,
                      'network': 'loopback authenticated TLS proxy only', 'liveInference': False}))
