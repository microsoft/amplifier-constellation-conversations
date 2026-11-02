#!/usr/bin/env python3
"""Exercise source-management CLI/API parity against a disposable HTTPS server.

No model calls, real user authentication, or installed-service changes.
"""
import json
import os
from pathlib import Path
import ssl
import subprocess
import tempfile
import time
import urllib.error
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get('AGENT_CONVERSATIONS_BINARY', ROOT / 'target/debug/agent-conversations'))


def run():
    with tempfile.TemporaryDirectory(prefix='import-source-cli-') as folder:
        home = Path(folder)

        def cli(*args):
            result = subprocess.run([str(BINARY), '--home', str(home), *args],
                                    capture_output=True, text=True, check=True)
            return json.loads(result.stdout), result.stdout + result.stderr

        cli('init', '--agent-command', 'python3', '--agent-arg',
            str(ROOT / 'tests/fixture_agent.py'), '--owner', 'fixture-owner')
        config = json.loads((home / 'config.json').read_text())
        context = ssl.create_default_context(cafile=config['tls_ca'])
        server = None

        def start():
            nonlocal server
            server = subprocess.Popen([str(BINARY), '--home', str(home), 'serve'],
                                      stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            for _ in range(100):
                assert server.poll() is None, 'HTTPS test server exited'
                try:
                    url = json.loads((home / 'runtime.json').read_text())['url']
                    with urllib.request.urlopen(url + '/health', context=context, timeout=1):
                        return url
                except (OSError, ValueError):
                    time.sleep(.05)
            raise AssertionError('HTTPS test server did not start')

        def import_with(token):
            body = {'label': 'CLI fixture', 'surface': 'zed', 'messages': [
                {'id': '1', 'role': 'user', 'text': 'Fixture import', 'created_at': 1}]}
            req = urllib.request.Request(url + '/api/v1/imports/zed/native', method='PUT',
                data=json.dumps(body).encode(), headers={
                    'Authorization': 'Bearer ' + token, 'Content-Type': 'application/json'})
            try:
                with urllib.request.urlopen(req, context=context, timeout=5) as response:
                    return response.status, json.load(response)
            except urllib.error.HTTPError as error:
                return error.code, json.load(error)

        try:
            url = start()
            token_file = home / 'zed-token'
            created, output = cli('import-source', 'create', 'zed', '--surface', 'zed',
                                  '--token-out', str(token_file))
            original = token_file.read_text()
            assert 'token' not in created and original not in output
            assert token_file.stat().st_mode & 0o777 == 0o600
            code, imported = import_with(original)
            assert code == 200 and imported['appended'] == 1
            listing, output = cli('import-source', 'list')
            assert [v['source'] for v in listing['items']] == ['toad', 'zed']
            assert original not in output
            rotated, output = cli('import-source', 'rotate', 'zed', '--token-out', str(token_file))
            current = token_file.read_text()
            assert current != original and current not in output and 'token' not in rotated
            assert import_with(original)[0] == 401
            code, replay = import_with(current)
            assert code == 200 and replay['conversation_id'] == imported['conversation_id']
            assert replay['appended'] == 0
            cli('import-source', 'revoke', 'zed')
            server.terminate(); server.wait(timeout=10)
            url = start()
            assert import_with(current)[0] == 401
            listing, _ = cli('import-source', 'list')
            assert next(v for v in listing['items'] if v['source'] == 'zed')['enabled'] is False
            print(json.dumps({'passed': True, 'checks': ['HTTPS CLI/API parity',
                'private credential output', 'scoped import', 'credential rotation',
                'idempotent replay', 'revocation survives restart'], 'model_prompts': 0}))
        finally:
            if server is not None and server.poll() is None:
                server.terminate(); server.wait(timeout=10)


if __name__ == '__main__':
    os.umask(0o077)
    run()
