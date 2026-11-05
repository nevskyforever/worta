"""Native Document genesis → production HTTP → authenticated Vue reader."""
import json
import os
import socket
import sqlite3
import subprocess
import threading
from queue import Queue
from uuid import uuid4

import uvicorn
from sqlalchemy import select
from sqlalchemy.orm import Session

from test_cloud_auth import ROOT, cloud_client, create_user, login, migrated_database
from test_cloud_c15_headless_cross_runtime import PROJECT_ID, _headers
from test_cloud_c18_authority_cross_runtime import provision, bootstrap, native, publish, sync
from test_cloud_c18_content_note_gate import capabilities
from test_cloud_c18_document_acceptance import document, content, emit, support, receive, heads
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
    text = 'Первая редакция'
    saved = document(tmp_path, db, identity, user, 'save', content=content(text))
    assert 'error' not in saved, saved
    document(tmp_path, db, identity, user, 'begin')
    expected = document(tmp_path, db, identity, user, 'pending')[0]['event']['document']
    published = emit(client, tmp_path, token, db, identity, user, key)
    assert len(published) == 1
    page = client.get('/api/v3/recovery/encrypted/history', headers=headers,
        params={'since': 0, 'limit': 100, 'expected_cursor': 2, 'expected_epoch': 2})
    assert page.status_code == 200, page.text
    items = page.json()['items']
    assert [item['event']['entity_type'] for item in items] == ['project_metadata', 'document']
    assert items[1]['object'] == published[0]['items'][0]['object']
    assert client.get('/api/v3/recovery/encrypted/history').status_code == 401
    create_user(engine, username='Foreign', email='foreign@example.test')
    foreign_token = login(client, username='Foreign').json()['access_token']
    foreign_page = client.get('/api/v3/recovery/encrypted/history', headers=_headers(foreign_token),
                              params={'since': 0, 'limit': 100, 'expected_cursor': 2, 'expected_epoch': 2})
    assert foreign_page.status_code == 409
    assert foreign_page.json()['detail']['code'] == 'recovery_snapshot_changed'
    fixture = tmp_path / 'document-http.json'
    fixture.write_text(json.dumps({'key': key, 'project': PROJECT_ID, 'expected': expected, 'text': text, 'items': items, 'updateRequest': True}))
    fixture.chmod(0o600)
    api_socket = socket.socket(); api_socket.bind(('127.0.0.1', 0))
    port = api_socket.getsockname()[1]
    server = uvicorn.Server(uvicorn.Config(client.app, lifespan='off', access_log=False, log_level='warning'))
    thread = threading.Thread(target=server.run, kwargs={'sockets': [api_socket]}, daemon=True)
    thread.start()
    try:
        process = subprocess.Popen(['node', 'scripts/browser-document-http.mjs', str(fixture), f'http://127.0.0.1:{port}', str(tmp_path / 'browser-profile')],
            cwd=ROOT / 'frontend', stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            ready = Queue()
            threading.Thread(target=lambda: ready.put(process.stdout.readline()), daemon=True).start()
            line = ready.get(timeout=120)
            assert line and json.loads(line)['phase'] == 'ready_for_update', line
            # The real Desktop consumes its published event before authoring a
            # successor, so expected tips and revision come from its own owner.
            assert receive(client, tmp_path, token, db, identity, user, key) == ['applied']
            updated = document(tmp_path, db, identity, user, 'save', content=content('Вторая редакция'),
                               expected=heads(tmp_path, db, identity, user, expected['id']))
            assert 'error' not in updated, updated
            document(tmp_path, db, identity, user, 'begin')
            pending = document(tmp_path, db, identity, user, 'pending')
            assert len(pending) == 1
            event = pending[0]['event']
            assert event['header']['operation'] == 'update' and event['header']['revision'] == 2
            assert event['document']['id'] == expected['id'] and event['document']['stage_id'] is None
            assert event['header']['parents'] == [items[1]['event']['event_id']]
            published_update = emit(client, tmp_path, token, db, identity, user, key)
            assert len(published_update) == 1
            latest = client.get('/api/v3/recovery/encrypted/history', headers=headers,
                params={'since': 0, 'limit': 100, 'expected_cursor': 3, 'expected_epoch': 2})
            assert latest.status_code == 200, latest.text
            latest_items = latest.json()['items']
            assert latest_items[:2] == items
            assert latest_items[2]['object'] == published_update[0]['items'][0]['object']
            stdout, stderr = process.communicate(json.dumps({'text': 'Вторая редакция', 'expected': event['document'], 'items': latest_items}) + '\n', timeout=120)
            assert process.returncode == 0, stdout + stderr
            assert json.loads(stdout.strip().splitlines()[-1])['pass']
        finally:
            if process.poll() is None:
                process.kill(); process.communicate()
        from backend.app.cloud.models import SyncDevice
        with Session(engine) as session:
            devices = session.scalars(select(SyncDevice).where(SyncDevice.user_id == user)).all()
            assert [str(row.device_id) for row in devices] == [device]
            assert all(row.last_ack_sequence == 0 for row in devices)
    finally:
        server.should_exit = True; thread.join(timeout=10); api_socket.close()
