"""Real gate observation; native history stays on the isolated local transport.

This does not qualify server-to-browser encrypted pull or issue capability PUT.
"""
from __future__ import annotations

import json
import socket
import subprocess
import threading
import time
import urllib.request

import uvicorn
from sqlalchemy.orm import Session

from backend.app.cloud.models import EncryptedObject, SyncDevice
from test_cloud_auth import ROOT, cloud_client, create_user, migrated_database
from test_web_replay_native_content_conflict import native_content_resolution_history


def test_reader_gate_observation_real_http_with_native_browser_history(cloud_client, tmp_path):
    client, engine = cloud_client
    user = create_user(engine, username='ReaderQualification', email='qualification@example.test')
    fixture = native_content_resolution_history(tmp_path, str(user))
    api_socket = socket.socket()
    api_socket.bind(('127.0.0.1', 0))
    api_port = api_socket.getsockname()[1]
    server = uvicorn.Server(uvicorn.Config(client.app, lifespan='off', access_log=False, log_level='warning'))
    thread = threading.Thread(target=server.run, kwargs={'sockets': [api_socket]}, daemon=True)
    thread.start()
    with socket.socket() as port_socket:
        port_socket.bind(('127.0.0.1', 0))
        vite_port = port_socket.getsockname()[1]
    with (tmp_path / 'vite-reader-qualification.log').open('w+') as log:
        vite = subprocess.Popen(['node', 'node_modules/vite/bin/vite.js', '--host', '127.0.0.1', '--port', str(vite_port), '--strictPort'], cwd=ROOT / 'frontend', stdout=log, stderr=log)
        try:
            for _ in range(100):
                try:
                    urllib.request.urlopen(f'http://127.0.0.1:{vite_port}/scripts/browser-cloud-account-harness.html', timeout=1).close()
                    if server.started:
                        break
                except OSError:
                    pass
                time.sleep(.1)
            else:
                raise AssertionError('isolated backend/Vite did not start')
            fixture['backend'] = f'http://127.0.0.1:{api_port}'
            path = tmp_path / 'reader-gate-fixture.json'
            path.write_text(json.dumps(fixture), encoding='utf-8'); path.chmod(0o600)
            result = subprocess.run(['node', 'scripts/browser-replay-resolution-local.mjs', str(path)],
                cwd=ROOT / 'frontend', capture_output=True, text=True, timeout=300)
            assert result.returncode == 0, result.stdout + result.stderr
            evidence = json.loads(result.stdout.strip().splitlines()[-1])
            assert evidence['pass']
            with Session(engine) as session:
                devices = list(session.query(SyncDevice))
                assert len(devices) == 1
                assert all(device.last_ack_sequence == 0 for device in devices)
                assert all(device.note_ordinary_reader_version == 0 and device.note_resolution_reader_version == 0 for device in devices)
                # Native ciphertext is exercised locally; there is no fake server
                # capability or encrypted-feed eligibility in this observation test.
                assert session.query(EncryptedObject).count() == 0
        finally:
            vite.terminate()
            vite.wait(timeout=15)
            server.should_exit = True
            thread.join(timeout=15)
            api_socket.close()
