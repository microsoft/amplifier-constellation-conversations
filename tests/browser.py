#!/usr/bin/env python3
"""Run the real HTTPS server and browser against a model-free ACP fixture.

Seeds an expiring browser session only in this disposable test database.
PAM credential validation is covered separately; this never touches real users.
"""
import hashlib
import json
import os
from pathlib import Path
import secrets
import sqlite3
import ssl
import subprocess
import tempfile
import time
import urllib.request

ROOT=Path(__file__).resolve().parents[1]
BIN=ROOT/'target/debug/agent-conversations'
(ROOT/'private').mkdir(exist_ok=True)
with tempfile.TemporaryDirectory(prefix='browser-',dir=ROOT/'private') as temporary:
    home=Path(temporary)
    subprocess.run([str(BIN),'--home',str(home),'init','--agent-command','python3','--agent-arg',str(ROOT/'tests/fixture_agent.py'),'--owner','fixture-owner','--agent-label','Helper'],check=True,capture_output=True)
    config=json.loads((home/'config.json').read_text())
    config['allowed_users']=['robin']
    config['agent']['env']['FIXTURE_LOG']=str(home/'frames.jsonl')
    (home/'config.json').write_text(json.dumps(config))
    cookie=secrets.token_hex(32)
    sam_cookie=secrets.token_hex(32)
    with sqlite3.connect(home/'state.sqlite3') as db:
        db.execute('INSERT INTO browser_sessions(hash,owner,csrf,expires_at) VALUES(?,?,?,?)',(hashlib.sha256(cookie.encode()).hexdigest(),'fixture-owner',secrets.token_hex(32),int(time.time()*1000)+3600000))
        db.execute('INSERT INTO browser_sessions(hash,owner,csrf,expires_at) VALUES(?,?,?,?)',(hashlib.sha256(sam_cookie.encode()).hexdigest(),'robin',secrets.token_hex(32),int(time.time()*1000)+3600000))
    with (home/'server.log').open('w') as log:
        process=subprocess.Popen([str(BIN),'--home',str(home),'serve'],stdout=log,stderr=log)
        try:
            for _ in range(100):
                if (home/'runtime.json').exists():
                    port=json.loads((home/'runtime.json').read_text())['port']
                    try:
                        with urllib.request.urlopen(f'https://127.0.0.1:{port}/health',context=ssl.create_default_context(cafile=str(home/'tls/root-ca.crt')),timeout=1) as response:
                            assert response.status==200
                        break
                    except (OSError,TimeoutError): pass
                if process.poll() is not None: raise RuntimeError((home/'server.log').read_text())
                time.sleep(.1)
            else: raise RuntimeError('HTTPS fixture did not start')
            # Verify the command-line client validates TLS using the local CA.
            subprocess.run([str(BIN),'--home',str(home),'api','GET','/api/v1/conversations'],capture_output=True,check=True)
            # Source credentials use the same HTTPS API and never print their secret.
            source_file=home/'secrets/default-context-test'
            issued=subprocess.run([str(BIN),'--home',str(home),'context-source','issue','default','--token-out',str(source_file)],capture_output=True,text=True,check=True)
            source_secret=source_file.read_text()
            assert len(source_secret)==64 and source_secret not in issued.stdout
            assert source_file.stat().st_mode & 0o777 == 0o600
            assert json.loads(issued.stdout)['scope']=='context:default'
            shown=subprocess.run([str(BIN),'--home',str(home),'context-source','show','default'],capture_output=True,text=True,check=True)
            assert json.loads(shown.stdout)['enabled'] is True and source_secret not in shown.stdout
            revoked=subprocess.run([str(BIN),'--home',str(home),'context-source','revoke','default'],capture_output=True,text=True,check=True)
            assert json.loads(revoked.stdout)['enabled'] is False
            payload={'url':f'https://127.0.0.1:{port}','cookie_name':'acv_'+config['id'].replace('-',''),'cookie':cookie,'sam_cookie':sam_cookie,'control':(home/'secrets/control-token').read_text(),'context_token':(home/'secrets/context-token').read_text(),'import_token':(home/'secrets/import-token').read_text(),'screenshot':str(ROOT/'private/browser-desktop.png')}
            node=os.environ.get('NODE_BIN','node')
            subprocess.run([node,str(ROOT/'tests/browser.mjs')],input=json.dumps(payload),text=True,check=True)
        finally:
            process.terminate()
            try:process.wait(timeout=5)
            except subprocess.TimeoutExpired:process.kill();process.wait()
