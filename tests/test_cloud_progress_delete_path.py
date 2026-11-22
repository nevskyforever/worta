"""Bounded Progress deletion through the real encrypted server and native owner."""
import json
import os
import sqlite3
import subprocess

from nfprogress.core.sqlite.connection import register_remote_apply_authorization_guard
from test_cloud_auth import cloud_client, create_user, login, migrated_database
from test_cloud_c15_headless_cross_runtime import PROJECT_ID, _crypto_bridge, _headers
from test_cloud_c18_authority_cross_runtime import provision, bootstrap, native, publish, sync
from test_cloud_c18_progress_acceptance import progress, support, emit, receive, owner, totals


def test_desktop_progress_delete_encrypted_server_and_web_transition(cloud_client, tmp_path):
    client, engine = cloud_client
    user = str(create_user(engine))
    token = login(client).json()['access_token']
    headers = _headers(token)
    path, identity = provision(tmp_path, 'progress-delete', user, True)
    device = identity['device_id']
    with sqlite3.connect(path) as db:
        register_remote_apply_authorization_guard(db)
        db.execute("UPDATE projects SET status='активен',goal=1000,infinite=0,payload_json=json_set(payload_json,'$.work_method','manual','$.total',0,'$.progress_entries',json('[]'))")
    assert client.put(f'/api/v1/sync/devices/{device}', headers=headers).status_code == 200
    boot = bootstrap(tmp_path, path, identity, 'bootstrap_prepare')['bootstrap_id']
    assert client.post(f'/api/v1/cloud/projects/{PROJECT_ID}/bootstrap', headers=headers, json={'bootstrap_id': boot, 'device_id': device}).status_code == 200
    bootstrap(tmp_path, path, identity, 'bootstrap_prepare_capture', bootstrap_id=boot, remote_high_water=0)
    assert client.post(f'/api/v1/cloud/projects/{PROJECT_ID}/bootstrap/complete', headers=headers, json={'bootstrap_id': boot, 'device_id': device, 'initial_event_count': 0, 'initial_max_server_sequence': 0}).status_code == 200
    bootstrap(tmp_path, path, identity, 'bootstrap_confirm_active', bootstrap_id=boot, remote_high_water=0)
    assert client.post('/api/v2/sync/encrypted/cutover', headers=headers, json={'expected_cutover_epoch': 0}).status_code == 200
    assert client.post('/api/v3/sync/encrypted/reader-ready', headers=headers, json={'device_id': device, 'reader_transport_version': 3}).status_code == 204
    assert client.post('/api/v3/sync/encrypted/cutover', headers=headers, json={'expected_cutover_epoch': 1}).status_code == 200
    key = list(os.urandom(32))
    native(tmp_path, path, identity, user, 'begin')
    publish(client, tmp_path, token, path, identity, user, key)
    assert sync(client, tmp_path, token, path, identity, user, key)['view']['state'] == 'active'
    support(client, headers, device)
    assert progress(tmp_path, path, identity, user, 'begin')['owners'][0]['blocker'] is None
    emit(client, tmp_path, token, path, identity, user, key)
    assert receive(client, tmp_path, token, path, identity, user, key) == ['applied']
    for total in (100, 250):
        current = owner(tmp_path, path, identity, user)
        assert 'error' not in progress(tmp_path, path, identity, user, 'manual', stage_id=None, total=total, expected=current['tips'])
        emit(client, tmp_path, token, path, identity, user, key)
        assert receive(client, tmp_path, token, path, identity, user, key) == ['applied']
    current = owner(tmp_path, path, identity, user)
    entries = current['versions'][0]['chain']['entries']
    decision = {'project_id': PROJECT_ID, 'stage_id': None, 'expected_tips': current['tips'], 'expected_local': current['local'], 'selected_event_id': current['tips'][0], 'operation': 'tombstone', 'target_entry_id': entries[0]['entry_id'], 'rebased_from': [entries[1]['entry_id']], 'corrected_delta': None}
    event_id = progress(tmp_path, path, identity, user, 'decide', decision=decision)
    assert isinstance(event_id, str)
    assert totals(path)[0] == 250
    emit(client, tmp_path, token, path, identity, user, key)
    assert receive(client, tmp_path, token, path, identity, user, key) == ['applied']
    assert totals(path)[0] == 150
    with sqlite3.connect(path) as db:
        cursor = db.execute('SELECT pull_cursor FROM cloud_sync_state').fetchone()[0]
        expected = json.loads(db.execute('SELECT snapshot_json FROM cloud_progress_projection').fetchone()[0])
    response = client.get('/api/v3/recovery/encrypted/history', headers=headers, params={'since': 0, 'limit': 100, 'expected_cursor': cursor, 'expected_epoch': 2})
    assert response.status_code == 200, response.text
    items = [item for item in response.json()['items'] if item['event']['entity_type'] == 'progress']
    assert len(items) == 4 and items[-1]['event']['event_id'] == event_id
    events = [_crypto_bridge(tmp_path, {'action': 'progress_open', 'canonical_user_id': user, 'amk': key, 'item': item})['decoded'] for item in items]
    fixture = tmp_path / 'received-progress.json'
    fixture.write_text(json.dumps({'events': events, 'expected': expected}), encoding='utf-8')
    fixture.chmod(0o600)
    run = subprocess.run(['node', 'scripts/progress-server-transition.mjs', str(fixture)], cwd='frontend', capture_output=True, text=True, timeout=60)
    assert run.returncode == 0, run.stderr
