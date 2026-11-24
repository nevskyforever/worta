from __future__ import annotations

import base64
from datetime import datetime, timezone
from uuid import uuid4

import pytest
from sqlalchemy import text
from sqlalchemy.orm import Session

from backend.app.cloud.models import EncryptedObject, SyncEvent
from backend.app.cloud.schemas import V3EncryptedSyncPushRequest
from test_cloud_auth import cloud_client, create_user, login, migrated_database


def _headers(token: str) -> dict[str, str]:
    return {'Authorization': f'Bearer {token}'}


def _object() -> dict:
    return {'crypto_version': 1, 'aad_version': 1,
            'nonce': base64.urlsafe_b64encode(b'n' * 24).decode().rstrip('='),
            'ciphertext': base64.urlsafe_b64encode(b'opaque project metadata ciphertext').decode().rstrip('=')}


def _event(**changes) -> dict:
    event = {'event_id': str(uuid4()), 'project_id': 'project-1', 'entity_id': 'project-1',
             'entity_type': 'project_metadata', 'operation': 'upsert', 'revision': 1,
             'updated_at': datetime.now(timezone.utc).isoformat(), 'deleted_at': None}
    event.update(changes)
    return event


def _request(device: str, event: dict, obj: dict | None = None) -> dict:
    return {'protocol_version': 3, 'encrypted_sync_version': 3, 'device_id': device,
            'items': [{'event': event, 'object': obj or _object()}]}


def test_metadata_v3_schema_rejects_scope_and_unknown_plaintext_fields():
    device = str(uuid4())
    V3EncryptedSyncPushRequest.model_validate(_request(device, _event()))
    with pytest.raises(ValueError):
        V3EncryptedSyncPushRequest.model_validate(_request(device, _event(entity_id='other')))
    with pytest.raises(ValueError):
        V3EncryptedSyncPushRequest.model_validate(_request(device, _event(name='server-visible')))


def test_metadata_v3_explicit_cutover_and_opaque_replay(cloud_client):
    client, engine = cloud_client
    user_id = create_user(engine)
    token = login(client).json()['access_token']
    device = str(uuid4())
    assert client.put(f'/api/v1/sync/devices/{device}', headers=_headers(token)).status_code == 200
    assert client.post('/api/v1/cloud/projects/project-1', headers=_headers(token)).status_code == 200
    event, obj = _event(), _object()
    request = _request(device, event, obj)
    path = '/api/v3/sync/encrypted/push'
    assert client.post(path, json=request).status_code == 401
    assert client.post(path, headers=_headers(token), json={**request, 'protocol_version': 2}).status_code == 422
    blocked = client.post(path, headers=_headers(token), json=request)
    assert blocked.status_code == 409
    assert blocked.json()['detail']['code'] == 'sync_transport_mode_incompatible'
    assert client.post('/api/v2/sync/encrypted/cutover', headers=_headers(token), json={
        'expected_cutover_epoch': 0,
    }).status_code == 200
    assert client.post('/api/v3/sync/encrypted/cutover', headers=_headers(token), json={
        'expected_cutover_epoch': 1,
    }).json()['detail']['code'] == 'sync_transport_readers_not_ready'
    assert client.post('/api/v3/sync/encrypted/reader-ready', headers=_headers(token), json={
        'device_id': device, 'reader_transport_version': 3,
    }).status_code == 204
    assert client.post('/api/v3/sync/encrypted/reader-ready', headers=_headers(token), json={
        'device_id': str(uuid4()), 'reader_transport_version': 3,
    }).status_code == 409
    assert client.post('/api/v3/sync/encrypted/cutover', headers=_headers(token), json={
        'expected_cutover_epoch': 1,
    }).json() == {'writer_transport_version': 3, 'cutover_epoch': 2}
    first = client.post(path, headers=_headers(token), json=request)
    assert first.status_code == 200, first.text
    assert first.json()['results'][0]['duplicate'] is False
    replay = client.post(path, headers=_headers(token), json=request)
    assert replay.status_code == 200
    assert replay.json()['results'][0]['duplicate'] is True
    missing_project = client.post(path, headers=_headers(token), json=_request(
        device, _event(project_id='not-enabled', entity_id='not-enabled')))
    assert missing_project.status_code == 409
    unregistered = client.post(path, headers=_headers(token), json=_request(str(uuid4()), _event()))
    assert unregistered.status_code == 409
    invalid_identity = client.post(path, headers=_headers(token), json=_request(device, _event(entity_id='other')))
    assert invalid_identity.status_code == 422
    altered = _request(device, event, {**obj, 'nonce': base64.urlsafe_b64encode(b'x' * 24).decode().rstrip('=')})
    assert client.post(path, headers=_headers(token), json=altered).status_code == 409
    pull = client.get('/api/v3/sync/encrypted/pull', headers=_headers(token), params={
        'device_id': device, 'since': 0, 'protocol_version': 3, 'encrypted_sync_version': 3,
    })
    assert pull.status_code == 200, pull.text
    assert pull.json()['items'][0]['event']['entity_type'] == 'project_metadata'
    assert pull.json()['items'][0]['object'] == obj
    assert client.post('/api/v3/sync/encrypted/ack', headers=_headers(token), json={
        'protocol_version': 3, 'encrypted_sync_version': 3, 'device_id': device, 'cursor': 1,
    }).status_code == 204
    with Session(engine) as session:
        row = session.query(SyncEvent).filter_by(user_id=user_id, event_id=event['event_id']).one()
        assert row.entity_id == 'project-1'
        encrypted = session.query(EncryptedObject).filter_by(user_id=user_id, event_id=event['event_id']).one()
        assert encrypted.ciphertext == b'opaque project metadata ciphertext'


def test_metadata_v3_all_registered_readers_and_strict_note_pairing(cloud_client):
    client, engine = cloud_client
    create_user(engine)
    token = login(client).json()['access_token']
    first, second = str(uuid4()), str(uuid4())
    for device in (first, second):
        assert client.put(f'/api/v1/sync/devices/{device}', headers=_headers(token)).status_code == 200
    assert client.post('/api/v1/cloud/projects/project-1', headers=_headers(token)).status_code == 200
    assert client.post('/api/v2/sync/encrypted/cutover', headers=_headers(token), json={'expected_cutover_epoch': 0}).status_code == 200
    ready = '/api/v3/sync/encrypted/reader-ready'
    assert client.post(ready, headers=_headers(token), json={'device_id': first, 'reader_transport_version': 3}).status_code == 204
    blocked = client.post('/api/v3/sync/encrypted/cutover', headers=_headers(token), json={'expected_cutover_epoch': 1})
    assert blocked.status_code == 409 and blocked.json()['detail']['code'] == 'sync_transport_readers_not_ready'
    assert client.post(ready, headers=_headers(token), json={'device_id': second, 'reader_transport_version': 3}).status_code == 204
    assert client.post('/api/v3/sync/encrypted/cutover', headers=_headers(token), json={'expected_cutover_epoch': 1}).status_code == 200
    events = [_event(), _event()]
    for device, event in zip((first, second), events, strict=True):
        assert client.post('/api/v3/sync/encrypted/push', headers=_headers(token), json=_request(device, event)).status_code == 200
    page = client.get('/api/v3/sync/encrypted/pull', headers=_headers(token), params={
        'device_id': second, 'since': 0, 'protocol_version': 3, 'encrypted_sync_version': 3,
    })
    assert page.status_code == 200 and len(page.json()['items']) == 2
    assert {item['event']['event_id'] for item in page.json()['items']} == {event['event_id'] for event in events}
    invalid = _request(first, _event(entity_type='future_entity'))
    assert client.post('/api/v3/sync/encrypted/push', headers=_headers(token), json=invalid).status_code == 422
    invalid = _request(first, _event(entity_id='wrong'))
    assert client.post('/api/v3/sync/encrypted/push', headers=_headers(token), json=invalid).status_code == 422
    note = _event(entity_id='note-1', entity_type='note')
    assert client.post('/api/v3/sync/encrypted/push', headers=_headers(token), json=_request(first, note)).status_code == 200
    note_v2 = _event(entity_id='note-2', entity_type='note')
    assert client.post('/api/v2/sync/encrypted/push', headers=_headers(token), json={
        'protocol_version': 2, 'encrypted_sync_version': 2, 'device_id': first,
        'items': [{'event': note_v2, 'object': _object()}],
    }).status_code == 200
    assert client.post('/api/v2/sync/encrypted/push', headers=_headers(token), json={
        'protocol_version': 2, 'encrypted_sync_version': 2, 'device_id': first,
        'items': [{'event': _event(entity_type='project_metadata'), 'object': _object()}],
    }).status_code == 422


def test_initial_web_setup_requires_empty_scope_and_actual_reader(cloud_client):
    client, engine = cloud_client
    create_user(engine)
    headers = _headers(login(client).json()['access_token'])
    v2 = '/api/v2/sync/encrypted'
    v3 = '/api/v3/sync/encrypted'
    device, other = str(uuid4()), str(uuid4())
    assert client.get(f'{v2}/capabilities', headers=headers).json()['writer_transport_version'] == 1
    assert client.post(f'{v2}/cutover', headers=headers, json={
        'initial_web_device_id': device, 'expected_cutover_epoch': 99,
    }).json()['detail']['code'] == 'sync_transport_cutover_stale'
    assert client.post(f'{v2}/cutover', headers=headers, json={
        'initial_web_device_id': device, 'expected_cutover_epoch': 0,
    }).json()['writer_transport_version'] == 2
    assert client.put(f'/api/v1/sync/devices/{device}?initial_web_setup=true', headers=headers).status_code == 200
    assert client.put(f'/api/v1/sync/devices/{other}?initial_web_setup=true', headers=headers).json()[
        'detail']['code'] == 'initial_web_sync_requires_empty_account'
    assert client.post(f'{v3}/cutover', headers=headers, json={
        'initial_web_device_id': device, 'expected_cutover_epoch': 1,
    }).json()['detail']['code'] == 'sync_transport_readers_not_ready'
    assert client.post(f'{v3}/reader-ready', headers=headers, json={
        'initial_web_setup': True, 'device_id': device, 'reader_transport_version': 3,
    }).status_code == 204
    assert client.post(f'{v3}/cutover', headers=headers, json={
        'initial_web_device_id': device, 'expected_cutover_epoch': 1,
    }).json() == {'writer_transport_version': 3, 'cutover_epoch': 2}


def test_initial_web_setup_rejects_account_with_project(cloud_client):
    client, engine = cloud_client
    create_user(engine)
    headers = _headers(login(client).json()['access_token'])
    assert client.post('/api/v1/cloud/projects/project-1', headers=headers).status_code == 200
    result = client.post('/api/v2/sync/encrypted/cutover', headers=headers, json={
        'initial_web_device_id': str(uuid4()), 'expected_cutover_epoch': 0,
    })
    assert result.status_code == 409
    assert result.json()['detail']['code'] == 'initial_web_sync_requires_empty_account'
    assert client.get('/api/v2/sync/encrypted/capabilities', headers=headers).json()['writer_transport_version'] == 1


def test_web_project_edit_preserves_history_and_obeys_the_slot_limit(cloud_client):
    client, engine = cloud_client
    user = create_user(engine)
    headers = _headers(login(client).json()['access_token'])
    desktop, browser, boot = (str(uuid4()) for _ in range(3))
    sync = '/api/v3/sync/encrypted'
    projects = '/api/v1/cloud/projects'
    bootstrap = projects + '/project-1/bootstrap'
    with engine.begin() as connection:
        connection.execute(text('UPDATE global_limits SET max_cloud_projects=1'))
    assert client.put(f'/api/v1/sync/devices/{desktop}', headers=headers).status_code == 200
    assert client.post('/api/v2/sync/encrypted/cutover', headers=headers, json={'expected_cutover_epoch': 0}).status_code == 200
    assert client.post(sync + '/reader-ready', headers=headers, json={
        'device_id': desktop, 'reader_transport_version': 3,
    }).status_code == 204
    assert client.post(sync + '/cutover', headers=headers, json={'expected_cutover_epoch': 1}).status_code == 200
    assert client.post(bootstrap, headers=headers, json={'bootstrap_id': boot, 'device_id': desktop}).status_code == 200
    assert client.post(bootstrap + '/complete', headers=headers, json={
        'bootstrap_id': boot, 'device_id': desktop, 'initial_event_count': 0, 'initial_max_server_sequence': 0,
    }).status_code == 200
    assert client.get(projects, headers=headers).json()['cloud_project_ids'] == ['project-1']
    initial = _event()
    assert client.post(sync + '/push', headers=headers, json=_request(desktop, initial)).status_code == 200
    replacement_boot = str(uuid4())
    replacement_name = 'replacement'
    create_pin = {'web_metadata_version': 1, 'expected_epoch': 2, 'expected_cursor': 1, 'metadata_bootstrap': replacement_boot}
    new_project = _event(project_id=replacement_name, entity_id=replacement_name)
    request = _request(browser, new_project)
    denied = client.post(sync + '/push', headers=headers, params=create_pin, json=request)
    assert denied.status_code == 409 and 'cloud_project_limit_reached' in denied.text
    changed = _event(revision=2)
    pin = {'web_metadata_version': 1, 'expected_epoch': 2, 'expected_cursor': 1, 'metadata_parent': initial['event_id']}
    assert client.post(sync + '/push', headers=headers, json=_request(browser, changed)).status_code == 409
    assert client.post(sync + '/push', headers=headers, params=pin, json=_request(browser, changed)).status_code == 200
    later = _event(revision=3)
    stale = client.post(sync + '/push', headers=headers, params=pin, json=_request(browser, later))
    assert stale.status_code == 409 and 'web_metadata_snapshot_changed' in stale.text
    assert client.get(projects, headers=headers).json()['cloud_project_count'] == 1
    retry = client.post(sync + '/push', headers=headers, params=pin, json=_request(browser, changed))
    assert retry.json()['results'][0]['duplicate']
    pulled = client.get(sync + '/pull', headers=headers, params={
        'device_id': desktop, 'since': 0, 'protocol_version': 3, 'encrypted_sync_version': 3,
    })
    assert pulled.status_code == 200, pulled.text
    assert [row['event']['event_id'] for row in pulled.json()['items']] == [
        initial['event_id'], changed['event_id'],
    ]
    with engine.connect() as connection:
        assert connection.execute(text('SELECT count(*) FROM sync_devices WHERE user_id=:user'), {'user': user}).scalar_one() == 1


def test_web_stage_and_order_publish_atomically_without_enrolling_a_reader(cloud_client):
    client, engine = cloud_client
    user = create_user(engine)
    headers = _headers(login(client).json()['access_token'])
    desktop, browser, boot = (str(uuid4()) for _ in range(3))
    sync = '/api/v3/sync/encrypted'
    assert client.put(f'/api/v1/sync/devices/{desktop}', headers=headers).status_code == 200
    assert client.post('/api/v2/sync/encrypted/cutover', headers=headers, json={'expected_cutover_epoch': 0}).status_code == 200
    assert client.post(sync + '/reader-ready', headers=headers, json={
        'device_id': desktop, 'reader_transport_version': 3,
    }).status_code == 204
    assert client.post(sync + '/cutover', headers=headers, json={'expected_cutover_epoch': 1}).status_code == 200
    initial = _event()
    assert client.post(sync + '/push', headers=headers, json=_request(browser, initial), params={
        'web_metadata_version': 1, 'expected_cursor': 0, 'expected_epoch': 2, 'metadata_bootstrap': boot,
    }).status_code == 200
    stage = _event(entity_type='stage', entity_id=str(uuid4()))
    order = _event(entity_type='stage_order', entity_id='stage_order')
    enabled = _event(revision=2)
    body = _request(browser, stage)
    body['items'] += [_request(browser, event)['items'][0] for event in (order, enabled)]
    pin = {'web_stage_version': 1, 'expected_cursor': 1, 'expected_epoch': 2, 'stage_metadata_parent': initial['event_id']}

    def push(request=body, params=pin):
        return client.post(sync + '/push', headers=headers, params=params, json=request)

    assert push(_request(browser, stage)).status_code == 422
    assert push(params={**pin, 'web_document_version': 1}).status_code == 422
    assert push(params={**pin, 'stage_metadata_parent': str(uuid4())}).status_code == 409
    wrong_project = {**body, 'items': [body['items'][0], _request(browser, {**order, 'project_id': 'other'})['items'][0]]}
    assert push(wrong_project).status_code == 422
    with engine.connect() as connection:
        assert connection.execute(text('SELECT count(*) FROM sync_events')).scalar_one() == 1
    accepted = push()
    assert accepted.status_code == 200, accepted.text
    assert [row['server_sequence'] for row in accepted.json()['results']] == [2, 3, 4]
    assert all(row['duplicate'] for row in push().json()['results'])
    later_stage = _event(entity_type='stage', entity_id=str(uuid4()))
    later_order = _event(entity_type='stage_order', entity_id='stage_order', revision=2)
    later = _request(browser, later_stage)
    later['items'] += _request(browser, later_order)['items']
    later_pin = {**pin, 'expected_cursor': 4, 'stage_metadata_parent': enabled['event_id'], 'stage_order_parent': order['event_id']}
    assert push(later, {**later_pin, 'stage_order_parent': str(uuid4())}).status_code == 409
    assert push(later, later_pin).status_code == 200
    assert push({**later, 'items': [later['items'][0], body['items'][1]]}, later_pin).status_code == 409
    with engine.connect() as connection:
        assert connection.execute(text('SELECT count(*) FROM sync_events')).scalar_one() == 6
        assert connection.execute(text('SELECT count(*) FROM sync_devices WHERE user_id=:user'), {'user': user}).scalar_one() == 1
