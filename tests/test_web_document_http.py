"""Native Document genesis → production HTTP → authenticated Vue reader."""
import json
import os
import socket
import sqlite3
import subprocess
import threading
from uuid import uuid4

import uvicorn
from sqlalchemy import select
from sqlalchemy.orm import Session

from test_cloud_auth import ROOT, cloud_client, create_user, login, migrated_database
from test_cloud_c15_headless_cross_runtime import PROJECT_ID, _headers
from test_cloud_c18_authority_cross_runtime import provision, bootstrap, native, publish, sync
from test_cloud_c18_content_note_gate import capabilities
from test_cloud_c18_document_acceptance import document, content, emit, support
from nfprogress.core.sqlite.connection import register_remote_apply_authorization_guard


def test_native_document_recovery_get_vue_and_profile_restart(cloud_client, tmp_path):
    client, engine = cloud_client
    user = str(create_user(engine))
    token = login(client).json()['access_token']
    headers = _headers(token)
    db, identity = provision(tmp_path, 'document-http-native', user, True)
    document(tmp_path, db, identity, user, 'init')
    with sqlite3.connect(db) as connection:
        register_remote_apply_authorization_guard(connection)
        connection.execute("UPDATE projects SET status='активен',payload_json=json_set(payload_json,'$.work_method','app')")
    device = identity['device_id']
    assert client.put(f'/api/v1/sync/devices/{device}', headers=headers).status_code == 200
    boot = bootstrap(tmp_path, db, identity, 'bootstrap_prepare')['bootstrap_id']
    assert client.post(f'/api/v1/cloud/projects/{PROJECT_ID}/bootstrap', headers=headers,
                       json={'bootstrap_id': boot, 'device_id': device}).status_code == 200
    bootstrap(tmp_path, db, identity, 'bootstrap_prepare_capture', bootstrap_id=boot, remote_high_water=0)
    assert client.post(f'/api/v1/cloud/projects/{PROJECT_ID}/bootstrap/complete', headers=headers,
        json={'bootstrap_id': boot, 'device_id': device, 'initial_event_count': 0, 'initial_max_server_sequence': 0}).status_code == 200
    bootstrap(tmp_path, db, identity, 'bootstrap_confirm_active', bootstrap_id=boot, remote_high_water=0)
    assert client.post('/api/v2/sync/encrypted/cutover', headers=headers, json={'expected_cutover_epoch': 0}).status_code == 200
    assert client.post('/api/v3/sync/encrypted/reader-ready', headers=headers,
                       json={'device_id': device, 'reader_transport_version': 3}).status_code == 204
    assert client.post('/api/v3/sync/encrypted/cutover', headers=headers, json={'expected_cutover_epoch': 1}).status_code == 200
    key = list(os.urandom(32))
    native(tmp_path, db, identity, user, 'begin')
    publish(client, tmp_path, token, db, identity, user, key)
    assert sync(client, tmp_path, token, db, identity, user, key)['view']['state'] == 'active'
    # Only the isolated Native Desktop is declared, using its qualified codec8/
    # Resolution2 and codec10 reader contracts. Browser capabilities stay absent.
    assert client.put('/api/v3/sync/encrypted/note-reader-capabilities', headers=headers,
                      json=capabilities(device)).status_code == 204
    support(client, headers, device)
    text = 'Native manuscript through HTTP. <script>window.documentInjected=true</script>'
    saved = document(tmp_path, db, identity, user, 'save', content=content(text))
    assert 'error' not in saved, saved
    document(tmp_path, db, identity, user, 'begin')
    expected = document(tmp_path, db, identity, user, 'pending')[0]['event']['document']
    assert len(emit(client, tmp_path, token, db, identity, user, key)) == 1
    page = client.get('/api/v3/recovery/encrypted/history', headers=headers,
        params={'since': 0, 'limit': 100, 'expected_cursor': 2, 'expected_epoch': 2})
    assert page.status_code == 200, page.text
    items = page.json()['items']
    assert [item['event']['entity_type'] for item in items] == ['project_metadata', 'document']
    assert client.get('/api/v3/recovery/encrypted/history').status_code == 401
    create_user(engine, username='Foreign', email='foreign@example.test')
    foreign_token = login(client, username='Foreign').json()['access_token']
    foreign_page = client.get('/api/v3/recovery/encrypted/history', headers=_headers(foreign_token),
                              params={'since': 0, 'limit': 100, 'expected_cursor': 2, 'expected_epoch': 2})
    assert foreign_page.status_code == 409
    assert foreign_page.json()['detail']['code'] == 'recovery_snapshot_changed'
    fixture = tmp_path / 'document-http.json'
    fixture.write_text(json.dumps({'key': key, 'project': PROJECT_ID, 'expected': expected, 'text': text, 'items': items}))
    fixture.chmod(0o600)
    api_socket = socket.socket(); api_socket.bind(('127.0.0.1', 0))
    port = api_socket.getsockname()[1]
    server = uvicorn.Server(uvicorn.Config(client.app, lifespan='off', access_log=False, log_level='warning'))
    thread = threading.Thread(target=server.run, kwargs={'sockets': [api_socket]}, daemon=True)
    thread.start()
    try:
        result = subprocess.run(['node', 'scripts/browser-document-http.mjs', str(fixture), f'http://127.0.0.1:{port}', str(tmp_path / 'browser-profile')],
            cwd=ROOT / 'frontend', capture_output=True, text=True, timeout=180)
        assert result.returncode == 0, result.stdout + result.stderr
        assert json.loads(result.stdout.strip().splitlines()[-1])['pass']
        from backend.app.cloud.models import SyncDevice
        with Session(engine) as session:
            devices = session.scalars(select(SyncDevice).where(SyncDevice.user_id == user)).all()
            assert [str(row.device_id) for row in devices] == [device]
            assert all(row.last_ack_sequence == 0 for row in devices)
    finally:
        server.should_exit = True; thread.join(timeout=10); api_socket.close()
