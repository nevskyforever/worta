"""Native Progress owner → production HTTP → authenticated Vue reader."""
import json
import os
import socket
import sqlite3
import subprocess
import threading
from queue import Queue

import uvicorn
import pytest
from sqlalchemy import select
from sqlalchemy.orm import Session

from test_cloud_auth import ROOT, cloud_client, create_user, login, migrated_database
from test_cloud_c15_headless_cross_runtime import PROJECT_ID, _headers
from test_cloud_c18_authority_cross_runtime import provision, bootstrap, native, publish, sync
from test_cloud_c18_structural_cross_runtime import structural
from test_cloud_c18_structural_integration import publish_pending, pull_structure
from nfprogress.core.sqlite.connection import register_remote_apply_authorization_guard



from test_cloud_c18_progress_acceptance import progress, support as progress_support, emit as emit_progress, receive as receive_progress

@pytest.mark.parametrize("stage_scoped", [False, True], ids=["project", "stage"])
def test_native_progress_http_read_update_restart(cloud_client, tmp_path, stage_scoped):
    client, engine = cloud_client
    user = str(create_user(engine))
    token = login(client).json()['access_token']
    headers = _headers(token)
    db, identity = provision(tmp_path, 'progress-http-native', user, True)
    with sqlite3.connect(db) as connection:
        register_remote_apply_authorization_guard(connection)
        connection.execute("UPDATE projects SET status='активен',goal=1000,infinite=0,payload_json=json_set(payload_json,'$.work_method','manual','$.total',0,'$.progress_entries',json('[]'))")
        if stage_scoped:
            connection.execute("UPDATE projects SET payload_json=json_set(payload_json,'$.stages_enabled',json('true'))")
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
    # Owner-approved isolated Native declarations only, never the browser.
    progress_support(client, headers, device)
    stage_id = 'S1' if stage_scoped else None
    if stage_scoped:
        from test_cloud_c18_structural_cross_runtime import structural
        from test_cloud_c18_structural_integration import publish_pending, pull_structure
        stage = json.loads((ROOT / 'frontend/src/cloud/__fixtures__/stageCodecV1.json').read_text())['event']['stage']
        with sqlite3.connect(db) as connection:
            register_remote_apply_authorization_guard(connection)
            for position, sid in enumerate(('S2', 'S1')):
                details = dict(stage, name=f'Stage {sid}', work_method='manual', total=0, progress_entries=[])
                connection.execute('INSERT INTO stages(id,project_id,name,goal,infinite,unit,status,created_at,payload_json) VALUES(?,?,?,?,?,?,?,?,?)', (sid, PROJECT_ID, details['name'], details['goal'], int(details['infinite']), details['unit'], details['status'], details['created_at'], json.dumps(details)))
                connection.execute('INSERT INTO stage_order VALUES(?,?,?)', (sid, PROJECT_ID, position))
        structural(tmp_path, db, identity, user, 'begin')
        assert len(publish_pending(client, tmp_path, token, db, identity, user, key)) == 2
        pull_structure(client, tmp_path, token, db, identity, user, key)
        assert len(publish_pending(client, tmp_path, token, db, identity, user, key)) == 1
        pull_structure(client, tmp_path, token, db, identity, user, key)
    if stage_scoped:
        with sqlite3.connect(db) as connection:
            register_remote_apply_authorization_guard(connection)
            connection.execute("UPDATE stages SET payload_json=json_set(payload_json,'$.total',0,'$.progress_entries',json('[]'))")
    captured = progress(tmp_path, db, identity, user, 'begin')
    assert all(value['blocker'] is None for value in captured['owners']), captured
    assert len(emit_progress(client, tmp_path, token, db, identity, user, key)) == (2 if stage_scoped else 1)
    outcomes = receive_progress(client, tmp_path, token, db, identity, user, key)
    with sqlite3.connect(db) as connection:
        blockers = connection.execute('SELECT entity_id,state,blocker FROM cloud_progress_events').fetchall()
    assert outcomes == ['applied'] * (2 if stage_scoped else 1), blockers
    def append(total):
        owner = next(value for value in progress(tmp_path, db, identity, user, 'view')['owners'] if value['stage_id'] == stage_id)
        result = progress(tmp_path, db, identity, user, 'manual', stage_id=stage_id, total=total, expected=owner['tips'])
        assert 'error' not in result, result
        sent = emit_progress(client, tmp_path, token, db, identity, user, key)
        assert len(sent) == 1
        assert receive_progress(client, tmp_path, token, db, identity, user, key) == ['applied']
    for total in (100, 250, 300):
        append(total)
    def history():
        with sqlite3.connect(db) as connection:
            cursor = connection.execute('SELECT pull_cursor FROM cloud_sync_state').fetchone()[0]
        response = client.get('/api/v3/recovery/encrypted/history', headers=headers,
            params={'since': 0, 'limit': 100, 'expected_cursor': cursor, 'expected_epoch': 2})
        assert response.status_code == 200, response.text
        return response.json()['items']
    def expected():
        with sqlite3.connect(db) as connection:
            chain = json.loads(connection.execute('SELECT snapshot_json FROM cloud_progress_projection WHERE entity_id=?', ('stage:S1' if stage_scoped else 'project',)).fetchone()[0])
            payload = json.loads(connection.execute("SELECT payload_json FROM stages WHERE id='S1'" if stage_scoped else 'SELECT payload_json FROM projects').fetchone()[0])
            goal = connection.execute("SELECT goal FROM stages WHERE id='S1'" if stage_scoped else 'SELECT goal FROM projects').fetchone()[0]
        return {'goal': goal, 'chain': chain, 'total': payload['total'], 'progress': payload['progress'], 'remaining': payload['remaining'], 'entries': payload['progress_entries']}
    items = history()
    assert len(items) == (9 if stage_scoped else 5)
    fixture = tmp_path / 'progress-http.json'
    fixture.write_text(json.dumps({'key': key, 'project': PROJECT_ID, 'expected': expected(), 'items': items, 'stageId': stage_id}))
    fixture.chmod(0o600)
    api_socket = socket.socket(); api_socket.bind(('127.0.0.1', 0)); port = api_socket.getsockname()[1]
    server = uvicorn.Server(uvicorn.Config(client.app, lifespan='off', access_log=False, log_level='warning'))
    thread = threading.Thread(target=server.run, kwargs={'sockets': [api_socket]}, daemon=True); thread.start()
    try:
        process = subprocess.Popen(['node', 'scripts/browser-progress-http.mjs', str(fixture), f'http://127.0.0.1:{port}', str(tmp_path / 'browser-profile')],
            cwd=ROOT / 'frontend', stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            ready = Queue(); threading.Thread(target=lambda: ready.put(process.stdout.readline()), daemon=True).start()
            line = ready.get(timeout=120)
            if not line:
                stdout, stderr = process.communicate(timeout=10); raise AssertionError(stdout + stderr)
            assert json.loads(line)['phase'] == 'ready_for_update', line
            append(475)
            latest = history(); assert latest[:len(items)] == items
            stdout, stderr = process.communicate(json.dumps({'items': latest, 'expected': expected()}) + '\n', timeout=180)
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
