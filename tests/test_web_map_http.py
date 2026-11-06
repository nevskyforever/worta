"""Native Map owner → production HTTP → authenticated Vue reader."""
import json
import os
import socket
import sqlite3
import subprocess
import threading
from queue import Queue
from uuid import uuid4

import uvicorn
import pytest
from sqlalchemy import select
from sqlalchemy.orm import Session

from test_cloud_auth import ROOT, cloud_client, create_user, login, migrated_database
from test_cloud_c15_headless_cross_runtime import PROJECT_ID, _headers
from test_cloud_c18_authority_cross_runtime import provision, bootstrap, native, publish, sync
from test_cloud_c18_content_note_gate import capabilities
from test_cloud_c18_structural_cross_runtime import structural
from test_cloud_c18_structural_integration import publish_pending, pull_structure
from test_cloud_c18_document_acceptance import document, content, emit, support, receive, heads
from nfprogress.core.sqlite.connection import register_remote_apply_authorization_guard


from test_cloud_c18_map_acceptance import maps, change, emit as emit_map, receive as receive_map
from test_cloud_c18_map_gate import map_capabilities

@pytest.mark.parametrize("stage_scoped", [False, True], ids=["project", "stage"])
def test_native_project_map_http_read_update_restart(cloud_client, tmp_path, stage_scoped):
    client, engine = cloud_client
    user = str(create_user(engine))
    token = login(client).json()['access_token']
    headers = _headers(token)
    db, identity = provision(tmp_path, 'map-http-native', user, True)
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
    # Resolution2 and codec9 reader contracts. Browser capabilities stay absent.
    assert client.put('/api/v3/sync/encrypted/note-reader-capabilities', headers=headers,
                      json=capabilities(device)).status_code == 204
    assert client.put('/api/v3/sync/encrypted/map-reader-capabilities', headers=headers, json=map_capabilities(device)).status_code == 204
    stage_id = 'S1' if stage_scoped else None
    cursor = 5 if stage_scoped else 2
    if stage_scoped:
        from test_cloud_c18_structural_cross_runtime import structural
        from test_cloud_c18_structural_integration import publish_pending, pull_structure
        stage = json.loads((ROOT / 'frontend/src/cloud/__fixtures__/stageCodecV1.json').read_text())['event']['stage']
        with sqlite3.connect(db) as connection:
            register_remote_apply_authorization_guard(connection)
            for position, sid in enumerate(('S2', 'S1')):
                details = dict(stage, name=f'Stage {sid}', work_method='app')
                connection.execute('INSERT INTO stages(id,project_id,name,goal,infinite,unit,status,created_at,payload_json) VALUES(?,?,?,?,?,?,?,?,?)', (sid, PROJECT_ID, details['name'], details['goal'], int(details['infinite']), details['unit'], details['status'], details['created_at'], json.dumps(details)))
                connection.execute('INSERT INTO stage_order VALUES(?,?,?)', (sid, PROJECT_ID, position))
        structural(tmp_path, db, identity, user, 'begin')
        assert len(publish_pending(client, tmp_path, token, db, identity, user, key)) == 2
        pull_structure(client, tmp_path, token, db, identity, user, key)
        assert len(publish_pending(client, tmp_path, token, db, identity, user, key)) == 1
        pull_structure(client, tmp_path, token, db, identity, user, key)
    data = json.loads((ROOT / 'frontend/src/cloud/__fixtures__/mapCodecV1.json').read_text())['examples'][2]['event']['map']['data']
    data['nodeData']['topic'] = 'Проектная карта Native'
    assert change(tmp_path, db, identity, user, stage_id, data) is True
    with sqlite3.connect(db) as connection:
        note_id = connection.execute("SELECT id FROM notes WHERE stage_id IS ? AND json_extract(payload_json,'$.source_type')='mindmap'", (stage_id,)).fetchone()[0]
    maps(tmp_path, db, identity, user, 'note_edit', note_id=note_id, patch={'title': 'Проверенная аннотация карты', 'tags': ['map-tag'], 'checklist': [{'id': 'check', 'text': 'Проверенная задача', 'checked': True}]})
    maps(tmp_path, db, identity, user, 'begin')
    pending = maps(tmp_path, db, identity, user, 'pending')
    assert len(pending) == 1 and pending[0]['event']['header']['operation'] == 'create'
    expected = pending[0]['event']['map']
    assert expected['annotations']  # Native-created Map-owned Note identities, never fake proofs.
    sent = emit_map(client, tmp_path, token, db, identity, user, key)
    assert receive_map(client, tmp_path, token, db, identity, user, key) == ['applied']
    with sqlite3.connect(db) as connection:
        assert json.loads(connection.execute("SELECT snapshot_json FROM cloud_map_projection WHERE project_id='project-1'").fetchone()[0]) == expected
    def history(cursor):
        response = client.get('/api/v3/recovery/encrypted/history', headers=headers,
            params={'since': 0, 'limit': 100, 'expected_cursor': cursor, 'expected_epoch': 2})
        assert response.status_code == 200, response.text
        return response.json()['items']
    items = history(cursor)
    assert items[-1]['object'] == sent[0]['items'][0]['object']
    assert client.get('/api/v3/recovery/encrypted/history').status_code == 401
    create_user(engine, username='Foreign', email='foreign-map@example.test')
    foreign = login(client, username='Foreign').json()['access_token']
    denied = client.get('/api/v3/recovery/encrypted/history', headers=_headers(foreign), params={'since': 0, 'limit': 100, 'expected_cursor': cursor, 'expected_epoch': 2})
    assert denied.status_code == 409
    fixture = tmp_path / 'map-http.json'
    fixture.write_text(json.dumps({'key': key, 'project': PROJECT_ID, 'map': expected, 'items': items, 'stageId': stage_id}))
    fixture.chmod(0o600)
    api_socket = socket.socket(); api_socket.bind(('127.0.0.1', 0)); port = api_socket.getsockname()[1]
    server = uvicorn.Server(uvicorn.Config(client.app, lifespan='off', access_log=False, log_level='warning'))
    thread = threading.Thread(target=server.run, kwargs={'sockets': [api_socket]}, daemon=True); thread.start()
    try:
        process = subprocess.Popen(['node', 'scripts/browser-map-http.mjs', str(fixture), f'http://127.0.0.1:{port}', str(tmp_path / 'browser-profile')],
            cwd=ROOT / 'frontend', stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            ready = Queue(); threading.Thread(target=lambda: ready.put(process.stdout.readline()), daemon=True).start()
            line = ready.get(timeout=120)
            if not line:
                stdout, stderr = process.communicate(timeout=10); raise AssertionError(stdout + stderr)
            assert json.loads(line)['phase'] == 'ready_for_update', line
            updated = json.loads(json.dumps(data)); updated['nodeData']['topic'] = 'Обновлённая карта Desktop'
            assert change(tmp_path, db, identity, user, stage_id, updated) is True
            pending = maps(tmp_path, db, identity, user, 'pending')
            assert len(pending) == 1 and pending[0]['event']['header']['revision'] == 2
            assert pending[0]['event']['header']['parents'] == [items[-1]['event']['event_id']]
            expected_update = pending[0]['event']['map']
            sent_update = emit_map(client, tmp_path, token, db, identity, user, key)
            assert receive_map(client, tmp_path, token, db, identity, user, key) == ['applied']
            with sqlite3.connect(db) as connection:
                assert json.loads(connection.execute("SELECT snapshot_json FROM cloud_map_projection WHERE project_id='project-1'").fetchone()[0]) == expected_update
            latest = history(cursor + 1); assert latest[:cursor] == items
            assert latest[-1]['object'] == sent_update[0]['items'][0]['object']
            stdout, stderr = process.communicate(json.dumps({'items': latest, 'map': expected_update}) + '\n', timeout=180)
            assert process.returncode == 0, stdout + stderr
            assert json.loads(stdout.strip().splitlines()[-1])['pass']
        finally:
            if process.poll() is None: process.kill(); process.communicate()
        from backend.app.cloud.models import SyncDevice
        with Session(engine) as session:
            devices = session.scalars(select(SyncDevice).where(SyncDevice.user_id == user)).all()
            assert [str(value.device_id) for value in devices] == [device]
            assert devices[0].last_ack_sequence == 0
    finally:
        server.should_exit = True; thread.join(timeout=10); api_socket.close()
