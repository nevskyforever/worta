"""Real native Content publication respects the server gate without capability PUT."""
import json
import os
import socket
import sqlite3
import subprocess
import threading
from uuid import uuid4

import uvicorn
import pytest

from test_cloud_auth import cloud_client, create_user, login, migrated_database
from test_cloud_c15_headless_cross_runtime import PROJECT_ID, _crypto_bridge, _headers
from test_cloud_c18_authority_cross_runtime import provision, bootstrap, native, publish, sync
from test_cloud_c18_content_note_acceptance import content, emit, receive
from test_cloud_c18_content_note_gate import capabilities
from test_cloud_c18_structural_cross_runtime import structural
from test_cloud_c18_structural_integration import publish_pending, pull_structure
from test_cloud_auth import ROOT
from nfprogress.core.sqlite.connection import register_remote_apply_authorization_guard


@pytest.mark.parametrize("stage_format", [None, "plain", "html"], ids=["project-plain", "stage-plain", "stage-html"])
def test_native_content_publication_fails_closed_without_reader_capability(cloud_client, tmp_path, stage_format):
    client, engine = cloud_client
    user = str(create_user(engine))
    token = login(client).json()['access_token']
    headers = _headers(token)
    db, identity = provision(tmp_path, 'native-recovery', user, True)
    with sqlite3.connect(db) as connection:
        register_remote_apply_authorization_guard(connection)
        connection.execute("UPDATE projects SET status='активен' WHERE id=?", (PROJECT_ID,))
        if stage_format:
            connection.execute("UPDATE projects SET payload_json=json_set(payload_json,'$.stages_enabled',json('true')) WHERE id=?", (PROJECT_ID,))
    assert client.put(f"/api/v1/sync/devices/{identity['device_id']}", headers=headers).status_code == 200
    boot = bootstrap(tmp_path, db, identity, 'bootstrap_prepare')['bootstrap_id']
    assert client.post(f'/api/v1/cloud/projects/{PROJECT_ID}/bootstrap', headers=headers,
        json={'bootstrap_id': boot, 'device_id': identity['device_id']}).status_code == 200
    bootstrap(tmp_path, db, identity, 'bootstrap_prepare_capture', bootstrap_id=boot, remote_high_water=0)
    assert client.post(f'/api/v1/cloud/projects/{PROJECT_ID}/bootstrap/complete', headers=headers,
        json={'bootstrap_id': boot, 'device_id': identity['device_id'], 'initial_event_count': 0, 'initial_max_server_sequence': 0}).status_code == 200
    bootstrap(tmp_path, db, identity, 'bootstrap_confirm_active', bootstrap_id=boot, remote_high_water=0)
    assert client.post('/api/v2/sync/encrypted/cutover', headers=headers, json={'expected_cutover_epoch': 0}).status_code == 200
    assert client.post('/api/v3/sync/encrypted/reader-ready', headers=headers,
        json={'device_id': identity['device_id'], 'reader_transport_version': 3}).status_code == 204
    assert client.post('/api/v3/sync/encrypted/cutover', headers=headers, json={'expected_cutover_epoch': 1}).status_code == 200
    amk = list(os.urandom(32))
    native(tmp_path, db, identity, user, 'begin')
    publish(client, tmp_path, token, db, identity, user, amk)
    assert sync(client, tmp_path, token, db, identity, user, amk)['view']['state'] == 'active'
    if stage_format:
        stage = json.loads((ROOT / 'frontend/src/cloud/__fixtures__/stageCodecV1.json').read_text())['event']['stage']
        with sqlite3.connect(db) as connection:
            for position, stage_id in enumerate(('S1', 'S2')):
                details = dict(stage, name=f'Stage {stage_id}')
                connection.execute('INSERT INTO stages(id,project_id,name,goal,infinite,unit,status,created_at,payload_json) VALUES(?,?,?,?,?,?,?,?,?)',
                    (stage_id, PROJECT_ID, details['name'], details['goal'], int(details['infinite']), details['unit'], details['status'], details['created_at'], json.dumps(details)))
                connection.execute('INSERT INTO stage_order VALUES(?,?,?)', (stage_id, PROJECT_ID, position))
        structural(tmp_path, db, identity, user, 'begin')
        assert len(publish_pending(client, tmp_path, token, db, identity, user, amk)) == 2
        pull_structure(client, tmp_path, token, db, identity, user, amk)
        assert len(publish_pending(client, tmp_path, token, db, identity, user, amk)) == 1
        pull_structure(client, tmp_path, token, db, identity, user, amk)
    content(tmp_path, db, identity, user, 'create', note_id='native-note', stage_id='S1' if stage_format else None, content_format=stage_format or 'plain')
    content(tmp_path, db, identity, user, 'begin')
    pending = content(tmp_path, db, identity, user, 'pending')
    assert len(pending) == 1
    item = pending[0]
    header = json.loads(bytes(item['frame'][20:]))['event']['header']
    sealed = _crypto_bridge(tmp_path, {'action': 'content_note_seal', 'frame': item['frame'], 'amk': amk})
    wire = {key: header[key] for key in ('event_id', 'project_id', 'entity_id', 'revision', 'updated_at')}
    wire.update(entity_type='note', operation='event', deleted_at=None)
    response = client.post('/api/v3/sync/encrypted/push', headers=headers, json={
        'protocol_version': 3, 'encrypted_sync_version': 3, 'device_id': identity['device_id'],
        'items': [{'event': wire, 'object': sealed['object']}],
    })
    assert response.status_code == 409
    assert response.json()['detail']['code'] == 'content_note_readers_not_ready'
    assert len(content(tmp_path, db, identity, user, 'pending')) == 1
    # Desktop's production codec8 reader and Resolution2 are qualified by the
    # native and codec tests; only this isolated synthetic Desktop is declared.
    response = client.put('/api/v3/sync/encrypted/note-reader-capabilities', headers=headers,
                          json=capabilities(identity['device_id']))
    assert response.status_code == 204, response.text
    response = client.post('/api/v3/sync/encrypted/push', headers=headers, json={
        'protocol_version': 3, 'encrypted_sync_version': 3, 'device_id': identity['device_id'],
        'items': [{'event': wire, 'object': sealed['object']}],
    })
    assert response.status_code == 200, response.text
    sequence = response.json()['results'][0]['server_sequence']
    content(tmp_path, db, identity, user, 'seal', event_id=item['event_id'], frame=item['frame'], envelope=sealed['object'])
    content(tmp_path, db, identity, user, 'receipt', event_id=item['event_id'], server_sequence=sequence, duplicate=False)
    receive(client, tmp_path, token, db, identity, user, amk)
    if stage_format:
        content(tmp_path, db, identity, user, 'create', note_id='stage-two-note', stage_id='S2', content_format='plain')
        assert len(emit(client, tmp_path, token, db, identity, user, amk)) == 1
        receive(client, tmp_path, token, db, identity, user, amk)
        second_text = 'Text belonging to Stage S2'
        content(tmp_path, db, identity, user, 'edit', note_id='stage-two-note', stage_id='S2', patch={'content': second_text})
        assert len(emit(client, tmp_path, token, db, identity, user, amk)) == 1
        receive(client, tmp_path, token, db, identity, user, amk)
        sequence += 2
    page = client.get('/api/v3/recovery/encrypted/history', headers=headers,
        params={'since': 0, 'limit': 100, 'expected_cursor': sequence, 'expected_epoch': 2})
    assert page.status_code == 200, page.text
    assert next(row for row in page.json()['items'] if row['event']['event_id'] == item['event_id'])['object'] == sealed['object']
    assert any(row['event']['event_id'] == item['event_id'] for row in page.json()['items'])
    with sqlite3.connect(db) as connection:
        expected_content = json.loads(connection.execute("SELECT payload_json FROM notes WHERE id='native-note'").fetchone()[0])['content']
    fixture_path = tmp_path / 'native-recovery-browser.json'
    fixture_path.write_text(json.dumps({'amk': amk, 'accountId': str(uuid4()), 'deviceId': str(uuid4()),
        'projectId': PROJECT_ID, 'sequence': sequence, 'item': page.json()['items'][-1], 'expectedContent': expected_content, 'contentFormat': stage_format or 'plain', 'stageId': 'S1' if stage_format else None, 'stages': ['S1', 'S2'] if stage_format else []}), encoding='utf-8')
    fixture_path.chmod(0o600)
    api_socket = socket.socket()
    api_socket.bind(('127.0.0.1', 0))
    port = api_socket.getsockname()[1]
    server = uvicorn.Server(uvicorn.Config(client.app, lifespan='off', access_log=False, log_level='warning'))
    thread = threading.Thread(target=server.run, kwargs={'sockets': [api_socket]}, daemon=True)
    thread.start()
    try:
        with socket.socket() as vite_socket:
            vite_socket.bind(('127.0.0.1', 0))
            vite_port = vite_socket.getsockname()[1]
        profile_path = tmp_path / 'persistent-browser'
        result = subprocess.run(['node', 'scripts/browser-recovery-native-http.mjs', str(fixture_path), f'http://127.0.0.1:{port}', str(profile_path), str(vite_port)],
            cwd=ROOT / 'frontend', capture_output=True, text=True, timeout=120)
        assert result.returncode == 0, result.stdout + result.stderr
        assert json.loads(result.stdout.strip().splitlines()[-1])['pass']
        from sqlalchemy import select
        from sqlalchemy.orm import Session
        from backend.app.cloud.models import SyncDevice
        with Session(engine) as session:
            registered = session.scalars(select(SyncDevice).where(SyncDevice.user_id == user)).all()
            assert [str(device.device_id) for device in registered] == [identity['device_id']]
        updated_content = '<p>Native <strong>HTML update</strong> after browser recovery</p>' if stage_format == 'html' else 'Native edit after browser recovery'
        content(tmp_path, db, identity, user, 'edit', note_id='native-note', stage_id='S1' if stage_format else None, patch={'content': updated_content})
        assert len(emit(client, tmp_path, token, db, identity, user, amk)) == 1
        latest = client.get('/api/v3/recovery/encrypted/history', headers=headers,
            params={'since': 0, 'limit': 100, 'expected_cursor': sequence + 1, 'expected_epoch': 2})
        assert latest.status_code == 200, latest.text
        fixture_path.write_text(json.dumps({'amk': amk, 'accountId': str(uuid4()), 'deviceId': str(uuid4()),
            'projectId': PROJECT_ID, 'sequence': sequence + 1, 'item': latest.json()['items'][-1],
            'expectedContent': updated_content, 'contentFormat': stage_format or 'plain', 'stageId': 'S1' if stage_format else None, 'stages': ['S1', 'S2'] if stage_format else [], 'provisioned': True}), encoding='utf-8')
        result = subprocess.run(['node', 'scripts/browser-recovery-native-http.mjs', str(fixture_path), f'http://127.0.0.1:{port}', str(profile_path), str(vite_port)],
            cwd=ROOT / 'frontend', capture_output=True, text=True, timeout=120)
        assert result.returncode == 0, result.stdout + result.stderr
        assert json.loads(result.stdout.strip().splitlines()[-1])['pass']
        # The Stage cohort needs one real update and same-profile reopen.
        # Leave the longer project/plain pagination regression unchanged.
        if stage_format:
            receive(client, tmp_path, token, db, identity, user, amk)
            def check_stage_order(order, names=None):
                fixture_path.write_text(json.dumps({'projectId': PROJECT_ID, 'stages': order,
                    'firstText': 'Native edit after browser recovery' if stage_format == 'plain' else 'Native HTML update after browser recovery', 'firstHtml': stage_format == 'html', 'secondText': second_text, 'names': names}), encoding='utf-8')
                checked = subprocess.run(['node', 'scripts/browser-recovery-stage-order.mjs', str(fixture_path), f'http://127.0.0.1:{port}', str(profile_path), str(vite_port)],
                    cwd=ROOT / 'frontend', capture_output=True, text=True, timeout=120)
                assert checked.returncode == 0, checked.stdout + checked.stderr
                assert json.loads(checked.stdout.strip().splitlines()[-1])['pass']
            check_stage_order(['S1', 'S2'])
            structural(tmp_path, db, identity, user, 'order', stage_ids=['S2', 'S1'])
            assert len(publish_pending(client, tmp_path, token, db, identity, user, amk)) == 1
            pull_structure(client, tmp_path, token, db, identity, user, amk)
            check_stage_order(['S2', 'S1'])
            structural(tmp_path, db, identity, user, 'edit', stage_id='S3', proposed=dict(stage, name='Stage S3'))
            assert len(publish_pending(client, tmp_path, token, db, identity, user, amk)) == 1
            pull_structure(client, tmp_path, token, db, identity, user, amk)
            assert len(publish_pending(client, tmp_path, token, db, identity, user, amk)) == 1
            pull_structure(client, tmp_path, token, db, identity, user, amk)
            with sqlite3.connect(db) as connection:
                stage_order = [row[0] for row in connection.execute('SELECT stage_id FROM stage_order WHERE project_id=? ORDER BY position', (PROJECT_ID,))]
            check_stage_order(stage_order)
            structural(tmp_path, db, identity, user, 'edit', stage_id='S1', proposed=dict(stage, name='Renamed Stage S1'))
            assert len(publish_pending(client, tmp_path, token, db, identity, user, amk)) == 1
            pull_structure(client, tmp_path, token, db, identity, user, amk)
            check_stage_order(stage_order, {'S1': 'Renamed Stage S1'})
            return
        receive(client, tmp_path, token, db, identity, user, amk)
        content(tmp_path, db, identity, user, 'edit', note_id='native-note', stage_id=None, patch={'content': 'Second native edit after browser recovery'})
        assert len(emit(client, tmp_path, token, db, identity, user, amk)) == 1
        last_cursor = sequence + 2
        items = []
        for since in range(last_cursor):
            params = {'since': since, 'limit': 1, 'expected_cursor': last_cursor, 'expected_epoch': 2}
            paged = client.get('/api/v3/recovery/encrypted/history', headers=headers, params=params)
            assert paged.status_code == 200, paged.text
            page_body = paged.json()
            assert [entry['event']['server_sequence'] for entry in page_body['items']] == [since + 1]
            assert page_body['next_cursor'] == since + 1 and page_body['has_more'] == (since + 1 < last_cursor)
            assert client.get('/api/v3/recovery/encrypted/history', headers=headers, params=params).json() == page_body
            items.extend(page_body['items'])
        complete = client.get('/api/v3/recovery/encrypted/history', headers=headers,
            params={'since': 0, 'limit': 100, 'expected_cursor': last_cursor, 'expected_epoch': 2})
        assert complete.status_code == 200 and complete.json()['items'] == items
        assert items[sequence - 1]['object'] == sealed['object']
        fixture_path.write_text(json.dumps({'amk': amk, 'accountId': str(uuid4()), 'deviceId': str(uuid4()),
            'projectId': PROJECT_ID, 'sequence': last_cursor, 'item': items[-1], 'items': items,
            'expectedContent': 'Second native edit after browser recovery', 'provisioned': True, 'pagination': True}), encoding='utf-8')
        result = subprocess.run(['node', 'scripts/browser-recovery-native-http.mjs', str(fixture_path), f'http://127.0.0.1:{port}', str(profile_path), str(vite_port)],
            cwd=ROOT / 'frontend', capture_output=True, text=True, timeout=120)
        assert result.returncode == 0, result.stdout + result.stderr
        assert json.loads(result.stdout.strip().splitlines()[-1])['pass']
    finally:
        server.should_exit = True
        thread.join(timeout=15)
        api_socket.close()
