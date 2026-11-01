"""Private encrypted Note source seed against disposable PostgreSQL16 and real Chromium."""
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


def test_browser_replay_v6_atomic_migration(tmp_path):
    with socket.socket() as port_socket:
        port_socket.bind(('127.0.0.1', 0))
        port = port_socket.getsockname()[1]
    with (tmp_path / 'vite-v6.log').open('w+') as log:
        vite = subprocess.Popen(['node', 'node_modules/vite/bin/vite.js', '--host', '127.0.0.1', '--port', str(port), '--strictPort'], cwd=ROOT / 'frontend', stdout=log, stderr=log)
        try:
            for _ in range(100):
                try:
                    urllib.request.urlopen(f'http://127.0.0.1:{port}/scripts/browser-cloud-account-harness.html', timeout=1).close()
                    break
                except OSError:
                    time.sleep(.1)
            else:
                raise AssertionError('isolated Vite did not start')
            result = subprocess.run(['node', 'scripts/browser-replay-v6-migration-acceptance.mjs', f'http://127.0.0.1:{port}'], cwd=ROOT / 'frontend', capture_output=True, text=True, timeout=120)
            assert result.returncode == 0, result.stdout + result.stderr
            assert json.loads(result.stdout.strip().splitlines()[-1])['pass']
        finally:
            vite.terminate()
            vite.wait(timeout=15)


def test_browser_replay_v6_owner_migration_real_api_postgresql(cloud_client, tmp_path):
    client, engine = cloud_client
    create_user(engine, username='SourceSeed', email='sourceseed@example.test')
    api_socket = socket.socket()
    api_socket.bind(('127.0.0.1', 0))
    api_port = api_socket.getsockname()[1]
    server = uvicorn.Server(uvicorn.Config(client.app, lifespan='off', access_log=False, log_level='warning'))
    thread = threading.Thread(target=server.run, kwargs={'sockets': [api_socket]}, daemon=True)
    thread.start()
    with socket.socket() as port_socket:
        port_socket.bind(('127.0.0.1', 0))
        vite_port = port_socket.getsockname()[1]
    with (tmp_path / 'vite.log').open('w+') as log:
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
            result = subprocess.run(['node', 'scripts/browser-replay-v6-owner-migration.mjs',
                f'http://127.0.0.1:{vite_port}', f'http://127.0.0.1:{api_port}'],
                cwd=ROOT / 'frontend', capture_output=True, text=True, timeout=300)
            assert result.returncode == 0, result.stdout + result.stderr
            evidence = json.loads(result.stdout.strip().splitlines()[-1])
            assert evidence['pass'] and evidence['ownerHeads'] > 0 and evidence['seeds'] > 0 and evidence['inboxProofs'] > 0
            with Session(engine) as session:
                for device in session.query(SyncDevice):
                    assert device.last_ack_sequence == 0
                    assert device.note_ordinary_reader_version == 0
                for row in session.query(EncryptedObject):
                    assert b'PRIVATE_SEED_CONTENT_' not in row.ciphertext
        finally:
            vite.terminate()
            vite.wait(timeout=15)
            server.should_exit = True
            thread.join(timeout=15)
            api_socket.close()
