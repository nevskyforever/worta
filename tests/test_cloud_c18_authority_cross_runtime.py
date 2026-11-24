"""Bounded C18 metadata acceptance: production crypto, file-backed SQLite and PostgreSQL."""
from __future__ import annotations

import os
import sqlite3
from datetime import datetime, timezone
from pathlib import Path

from nfprogress.core.sqlite.connection import register_remote_apply_authorization_guard

from test_cloud_auth import cloud_client, create_user, login, migrated_database
from test_cloud_c15_headless_cross_runtime import PROJECT_ID, _crypto_bridge, _headers, _native_bridge


def native(tmp_path, path, identity, user, step, **values):
    return _native_bridge(tmp_path, dict(action='metadata_authority', database_path=str(path),
        local_account_id=identity['local_account_id'], device_id=identity['device_id'],
        canonical_user_id=user, project_id=PROJECT_ID, step=step, **values))


def provision(tmp_path, name, user, create):
    root = tmp_path / name
    path = root / 'nfprogress.db'
    identity = _native_bridge(tmp_path, dict(action='provision', database_path=str(path),
        data_root=str(root), canonical_user_id=user, project_id=PROJECT_ID,
        create_project=create, bind_project=False))
    return path, identity


def bootstrap(tmp_path, path, identity, action, **values):
    return _native_bridge(tmp_path, dict(action=action, database_path=str(path),
        local_account_id=identity['local_account_id'], device_id=identity['device_id'], project_id=PROJECT_ID, **values))


def publish(client, tmp_path, token, path, identity, user, amk):
    event = native(tmp_path, path, identity, user, 'read')['unsealed'][0]
    sealed = _crypto_bridge(tmp_path, dict(action='metadata_seal', payload=event, amk=amk))
    native(tmp_path, path, identity, user, 'seal', event_id=event['header']['event_id'],
        nonce=sealed['nonce'], ciphertext=sealed['ciphertext'])
    header = event['header']
    wire = {key: header[key] for key in ('event_id', 'project_id', 'entity_id', 'revision', 'updated_at')}
    wire.update(entity_type='project_metadata', operation='upsert', deleted_at=None)
    request = dict(protocol_version=3, encrypted_sync_version=3, device_id=identity['device_id'],
        items=[dict(event=wire, object=sealed['object'])])
    response = client.post('/api/v3/sync/encrypted/push', headers=_headers(token), json=request)
    assert response.status_code == 200, response.text
    replay = client.post('/api/v3/sync/encrypted/push', headers=_headers(token), json=request)
    assert replay.status_code == 200 and replay.json()['results'][0]['duplicate']
    assert replay.json()['results'][0]['server_sequence'] == response.json()['results'][0]['server_sequence']
    return event, request


def sync(client, tmp_path, token, path, identity, user, amk):
    with sqlite3.connect(path) as db:
        cursor = db.execute('SELECT pull_cursor FROM cloud_sync_state').fetchone()[0]
    response = client.get('/api/v3/sync/encrypted/pull', headers=_headers(token), params=dict(
        device_id=identity['device_id'], since=cursor, limit=100, protocol_version=3, encrypted_sync_version=3))
    assert response.status_code == 200, response.text
    for item in response.json()['items']:
        opened = _crypto_bridge(tmp_path, dict(action='metadata_open', canonical_user_id=user, amk=amk, item=item))
        descriptor = item['event']
        row = dict(descriptor)
        row['source_device_id'] = row.pop('device_id')
        row['envelope'] = item['object']
        row['updated_at'] = datetime.fromisoformat(row['updated_at'].replace('Z', '+00:00')).astimezone(timezone.utc).isoformat(timespec='microseconds').replace('+00:00', 'Z')
        command = dict(account_id=identity['local_account_id'], device_id=identity['device_id'], canonical_user_id=user,
            expected_cursor=cursor, next_cursor=descriptor['server_sequence'], has_more=False, items=[row])
        native(tmp_path, path, identity, user, 'receive', command=command, opened=opened)
        cursor = descriptor['server_sequence']
    return native(tmp_path, path, identity, user, 'read')


def test_metadata_migration_import_mismatch_and_concurrent_rename(cloud_client, tmp_path: Path):
    client, engine = cloud_client
    user = str(create_user(engine)); token = login(client).json()['access_token']; headers = _headers(token)
    a, ia = provision(tmp_path, 'A', user, True)
    with sqlite3.connect(a) as db:
        register_remote_apply_authorization_guard(db)
        db.execute("UPDATE projects SET goal=1234.0,infinite=0,payload_json=json_set(payload_json,'$.personal_goal',200.0)")
    b, ib = provision(tmp_path, 'B', user, False)
    c, ic = provision(tmp_path, 'C-legacy-shell', user, False)
    for identity in (ia, ib, ic):
        assert client.put(f"/api/v1/sync/devices/{identity['device_id']}", headers=headers).status_code == 200
    prepared = bootstrap(tmp_path, a, ia, 'bootstrap_prepare')
    boot = prepared['bootstrap_id']
    assert client.post(f'/api/v1/cloud/projects/{PROJECT_ID}/bootstrap', headers=headers,
        json=dict(bootstrap_id=boot, device_id=ia['device_id'])).status_code == 200
    captured = bootstrap(tmp_path, a, ia, 'bootstrap_prepare_capture', bootstrap_id=boot, remote_high_water=0)
    assert captured['record']['initial_event_count'] == 0
    assert client.post(f'/api/v1/cloud/projects/{PROJECT_ID}/bootstrap/complete', headers=headers,
        json=dict(bootstrap_id=boot, device_id=ia['device_id'], initial_event_count=0, initial_max_server_sequence=0)).status_code == 200
    bootstrap(tmp_path, a, ia, 'bootstrap_confirm_active', bootstrap_id=boot, remote_high_water=0)
    assert native(tmp_path, a, ia, user, 'read')['view']['state'] == 'local_legacy_only'
    assert client.post('/api/v2/sync/encrypted/cutover', headers=headers, json=dict(expected_cutover_epoch=0)).status_code == 200
    for identity in (ia, ib, ic):
        assert client.post('/api/v3/sync/encrypted/reader-ready', headers=headers,
            json=dict(device_id=identity['device_id'], reader_transport_version=3)).status_code == 204
    assert client.post('/api/v3/sync/encrypted/cutover', headers=headers, json=dict(expected_cutover_epoch=1)).status_code == 200
    native(tmp_path, a, ia, user, 'begin'); amk = list(os.urandom(32))
    genesis, _ = publish(client, tmp_path, token, a, ia, user, amk)
    assert sync(client, tmp_path, token, a, ia, user, amk)['view']['state'] == 'active'
    # New shell uses authenticated portable values; no invented competing authoritative name.
    bootstrap(tmp_path, b, ib, 'bootstrap_import', bootstrap_id=boot, remote_high_water=1,
        display_name=genesis['metadata']['name'], authenticated_metadata=genesis['metadata'])
    rb = sync(client, tmp_path, token, b, ib, user, amk)
    assert rb['view']['state'] == 'active' and rb['view']['local'] == genesis['metadata']
    # A pre-existing legacy import shell remains visible until its explicit decision.
    bootstrap(tmp_path, c, ic, 'bootstrap_import', bootstrap_id=boot, remote_high_water=1, display_name='Different legacy name')
    rc = sync(client, tmp_path, token, c, ic, user, amk)
    assert rc['view']['state'] == 'local_differs_from_authenticated'
    assert rc['view']['local']['name'] == 'Different legacy name'
    native(tmp_path, c, ic, user, 'change', kind='keep_local')
    descendant, _ = publish(client, tmp_path, token, c, ic, user, amk)
    assert descendant['header']['parent_event_ids'] == [genesis['header']['event_id']]
    for path, identity in ((a, ia), (b, ib), (c, ic)):
        view = sync(client, tmp_path, token, path, identity, user, amk)['view']
        assert view['state'] == 'active' and view['local']['name'] == 'Different legacy name'
    # Both writers edit the same authenticated parent before either peer update arrives.
    for path, identity, name in ((a, ia, 'Project Beta'), (b, ib, 'Project Gamma')):
        proposed = native(tmp_path, path, identity, user, 'read')['view']['local']
        proposed['name'] = name
        native(tmp_path, path, identity, user, 'normal_edit', proposed=proposed)
    beta, _ = publish(client, tmp_path, token, a, ia, user, amk)
    gamma, _ = publish(client, tmp_path, token, b, ib, user, amk)
    assert beta['header']['parent_event_ids'] == gamma['header']['parent_event_ids']
    for path, identity, name in ((a, ia, 'Project Beta'), (b, ib, 'Project Gamma')):
        view = sync(client, tmp_path, token, path, identity, user, amk)['view']
        assert view['state'] == 'metadata_conflict' and view['local']['name'] == name
        assert {branch['event_id'] for branch in view['branches']} == {beta['header']['event_id'], gamma['header']['event_id']}
    native(tmp_path, a, ia, user, 'change', kind='choose_branch', selected=gamma['header']['event_id'])
    resolution, request = publish(client, tmp_path, token, a, ia, user, amk)
    assert resolution['header']['parent_event_ids'] == sorted([beta['header']['event_id'], gamma['header']['event_id']])
    for path, identity in ((a, ia), (b, ib), (c, ic)):
        view = sync(client, tmp_path, token, path, identity, user, amk)['view']
        assert view['state'] == 'active' and view['local']['name'] == 'Project Gamma'
        # Each bridge call reopens its file-backed database; restart/replay cannot change the result.
        assert native(tmp_path, path, identity, user, 'read')['view'] == view
        with sqlite3.connect(path) as db:
            assert db.execute('SELECT COUNT(*) FROM cloud_sync_metadata_events').fetchone()[0] == 5
            assert db.execute('SELECT COUNT(*) FROM cloud_sync_metadata_reconciliation').fetchone()[0] == 1
    assert client.post('/api/v3/sync/encrypted/push', json=request).status_code == 401
    altered = {**request, 'items': [{**request['items'][0], 'event': {**request['items'][0]['event'], 'revision': 99}}]}
    assert client.post('/api/v3/sync/encrypted/push', headers=headers, json=altered).status_code == 409


def test_web_project_name_is_authoritative_for_desktop_import(cloud_client, tmp_path):
    from uuid import uuid4

    client, engine = cloud_client
    user = str(create_user(engine))
    token = login(client).json()['access_token']
    headers = _headers(token)
    path, identity = provision(tmp_path, 'desktop-imports-web', user, False)
    web, boot = str(uuid4()), str(uuid4())
    base = '/api/v3/sync/encrypted'
    for device in (identity['device_id'],):
        assert client.put(f'/api/v1/sync/devices/{device}', headers=headers).status_code == 200
    assert client.post('/api/v2/sync/encrypted/cutover', headers=headers, json={'expected_cutover_epoch': 0}).status_code == 200
    for device in (identity['device_id'],):
        assert client.post(base + '/reader-ready', headers=headers, json={
            'device_id': device, 'reader_transport_version': 3,
        }).status_code == 204
    assert client.post(base + '/cutover', headers=headers, json={'expected_cutover_epoch': 1}).status_code == 200

    amk = list(os.urandom(32))
    now = '2026-11-23T12:00:00.000000Z'
    head = {'account_id': user, 'device_id': web, 'bootstrap_id': boot, 'project_id': PROJECT_ID, 'entity_id': PROJECT_ID,
            'event_id': str(uuid4()), 'operation': 'create', 'parent_event_ids': [], 'revision': 1, 'generation': 1, 'updated_at': now}
    metadata = {'name': 'Первое имя из Web', 'goal': None, 'infinite': True, 'unit': 'symbols', 'status': 'активен',
                'deadline': None, 'personal_goal': 0, 'auto_freeze': False, 'streak_enabled': True, 'work_method': 'по дням',
                'stages_enabled': False, 'combine_stage_mindmaps': False}

    def push(pin=None):
        body = {'version': 1, 'header': head, 'metadata': metadata, 'deleted_at': None}
        sealed = _crypto_bridge(tmp_path, {'action': 'metadata_seal', 'payload': body, 'amk': amk})
        descriptor = {key: head[key] for key in ('event_id', 'project_id', 'entity_id', 'revision', 'updated_at')}
        descriptor.update(entity_type='project_metadata', operation='upsert', deleted_at=None)
        return client.post(base + '/push', headers=headers, params=pin, json={
            'protocol_version': 3, 'encrypted_sync_version': 3, 'device_id': head['device_id'],
            'items': [{'event': descriptor, 'object': sealed['object']}],
        })

    created = push({'web_metadata_version': 1, 'expected_epoch': 2, 'expected_cursor': 0, 'metadata_bootstrap': boot})
    assert created.status_code == 200, created.text
    bootstrap(tmp_path, path, identity, 'bootstrap_import', bootstrap_id=boot, remote_high_water=1,
              display_name=metadata['name'], authenticated_metadata=metadata)
    assert sync(client, tmp_path, token, path, identity, user, amk)['view']['local']['name'] == 'Первое имя из Web'
    parent = head['event_id']
    head.update(device_id=str(uuid4()), event_id=str(uuid4()), operation='update', parent_event_ids=[parent], revision=2, generation=2)
    metadata['name'] = 'Новое имя из Web'
    response = push({'web_metadata_version': 1, 'expected_epoch': 2, 'expected_cursor': 1, 'metadata_parent': parent})
    assert response.status_code == 200, response.text
    assert sync(client, tmp_path, token, path, identity, user, amk)['view']['local']['name'] == 'Новое имя из Web'

    from test_cloud_c18_content_note_gate import capabilities
    from test_cloud_c18_document_acceptance import content, document, receive, stored, support

    assert client.put(base + '/note-reader-capabilities', headers=headers,
                      json=capabilities(identity['device_id'])).status_code == 204
    support(client, headers, identity['device_id'])
    document(tmp_path, path, identity, user, 'init')
    text_id = 'web-project-manuscript'
    text_body = {
        'version': 1, 'mutation': 'upsert', 'deleted_at': None,
        'header': {
            'account_id': user, 'device_id': str(uuid4()), 'project_id': PROJECT_ID, 'stage_id': None,
            'entity_id': text_id, 'event_id': str(uuid4()), 'bootstrap_id': boot, 'metadata_event_id': head['event_id'],
            'stage_event_ids': [], 'parents': [], 'revision': 1, 'generation': 1, 'operation': 'create', 'updated_at': now,
        },
        'document': {'id': text_id, 'project_id': PROJECT_ID, 'stage_id': None, 'title': metadata['name'],
                     'content_json': content('Текст проекта Web'), 'content_format': 'tiptap-json/v1',
                     'created_at': now, 'extensions': {}},
    }
    wire = _crypto_bridge(tmp_path, {'action': 'web_document_seal', 'amk': amk, 'payload': text_body})
    saved = client.post(base + '/push', headers=headers, json=wire, params={
        'web_document_version': 1, 'expected_cursor': 2, 'expected_epoch': 2,
    })
    assert saved.status_code == 200, saved.text
    assert receive(client, tmp_path, token, path, identity, user, amk) == ['applied']
    assert stored(path, text_id)['content_json'] == text_body['document']['content_json']
