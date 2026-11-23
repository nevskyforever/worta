"""Map transport remains opaque and needs independent account-wide reader evidence."""
import base64
import hashlib
from uuid import uuid4

import pytest
from alembic import command
from alembic.config import Config
from sqlalchemy import inspect, text
from sqlalchemy.orm import Session

from backend.app.cloud.models import EncryptedObject, SyncDevice, SyncEvent
from backend.app.cloud.schemas import DocumentReaderCapabilities, V3EncryptedSyncPushRequest
from test_cloud_auth import ROOT, cloud_client, create_user, login, migrated_database
from test_cloud_c18_content_note_gate import capabilities
from test_cloud_c18_metadata import _event, _headers, _object, _request


def document_capabilities(device, **changes):
    return {**dict(device_id=device, frame_version=1, codec_id=10, codec_version=1,
                   reader_version=1, compression_zero=True), **changes}


def test_document_descriptor_and_capability_contract():
    device = str(uuid4())
    body = document_capabilities(device)
    DocumentReaderCapabilities.model_validate(body)
    for changes in ({'title': 'plaintext'}, {'codec_id': 8}, {'codec_version': True},
                    {'frame_version': '1'}, {'reader_version': 2}, {'compression_zero': 1}):
        with pytest.raises(ValueError):
            DocumentReaderCapabilities.model_validate({**body, **changes})
    for entity in ('project-map', 'stage-map-' + hashlib.sha256(b'S1').hexdigest()):
        event = _event(entity_id=entity, entity_type='document', operation='event')
        V3EncryptedSyncPushRequest.model_validate(_request(device, event))
        for changes in ({'operation': 'upsert'}, {'deleted_at': '2026-10-03T00:00:00Z'},
                        {'nodes': []}):
            with pytest.raises(ValueError):
                V3EncryptedSyncPushRequest.model_validate(_request(device, {**event, **changes}))


def test_document_reader_upgrade_preserves_populated_note_reader_baseline(cloud_client):
    client, engine = cloud_client
    user = create_user(engine)
    headers = _headers(login(client).json()['access_token'])
    device = str(uuid4())
    assert client.put(f'/api/v1/sync/devices/{device}', headers=headers).status_code == 200
    assert client.put('/api/v3/sync/encrypted/note-reader-capabilities', headers=headers,
                      json=capabilities(device)).status_code == 204
    config = Config(str(ROOT / 'alembic.ini'))
    command.downgrade(config, 'c18_map_readers')
    with engine.connect() as connection:
        before = connection.execute(text('SELECT user_id,device_id,last_ack_sequence,note_codec_version,'
            'note_resolution_reader_version,note_compression_zero FROM sync_devices')).all()
        assert 'document_reader_version' not in {c['name'] for c in inspect(connection).get_columns('sync_devices')}
    command.upgrade(config, 'head')
    command.upgrade(config, 'head')
    with engine.connect() as connection:
        after = connection.execute(text('SELECT user_id,device_id,last_ack_sequence,note_codec_version,'
            'note_resolution_reader_version,note_compression_zero FROM sync_devices')).all()
        assert after == before
        assert connection.execute(text('SELECT document_frame_version,document_codec_version,document_reader_version,'
            'document_compression_zero FROM sync_devices')).one() == (0, 0, 0, False)
        assert connection.execute(text('SELECT version_num FROM alembic_version')).scalar_one() == 'cloud_project_retirement'
    with Session(engine) as session:
        from backend.app.cloud.services import SyncService
        assert not SyncService.document_reader_ready(session.query(SyncDevice).filter_by(user_id=user).one())


def test_document_gate_three_registered_devices_exact_replay_and_downgrade(cloud_client):
    client, engine = cloud_client
    user = create_user(engine)
    headers = _headers(login(client).json()['access_token'])
    a, b, c = [str(uuid4()) for _ in range(3)]
    path = '/api/v3/sync/encrypted/document-reader-capabilities'
    notes = '/api/v3/sync/encrypted/note-reader-capabilities'
    push = '/api/v3/sync/encrypted/push'
    assert client.get(path).status_code == 401
    assert client.put(path, headers=headers, json=document_capabilities(a)).status_code == 409
    for device in (a, b, c):
        assert client.put(f'/api/v1/sync/devices/{device}', headers=headers).status_code == 200
    assert client.post('/api/v2/sync/encrypted/cutover', headers=headers,
                       json=dict(expected_cutover_epoch=0)).status_code == 200
    for device in (a, b, c):
        assert client.post('/api/v3/sync/encrypted/reader-ready', headers=headers,
            json=dict(device_id=device, reader_transport_version=3)).status_code == 204
    assert client.post('/api/v3/sync/encrypted/cutover', headers=headers,
                       json=dict(expected_cutover_epoch=1)).status_code == 200
    for device in (a, b, c):
        assert client.put(notes, headers=headers, json=capabilities(device)).status_code == 204
    for device in (a, b):
        assert client.put(path, headers=headers, json=document_capabilities(device)).status_code == 204
    assert client.get(notes, headers=headers).json()['ready'] is True
    assert client.get(path, headers=headers).json() == dict(ready=False, missing_devices=1)
    assert client.post('/api/v1/cloud/projects/project-1', headers=headers).status_code == 200
    event = _event(entity_id='project-map', entity_type='document', operation='event')
    obj = _object()
    request = _request(a, event, obj)
    blocked = client.post(push, headers=headers, json=request)
    assert blocked.status_code == 409 and 'document_readers_not_ready' in blocked.text
    assert client.put(path, headers=headers, json=document_capabilities(c)).status_code == 204
    assert client.get(path, headers=headers).json() == dict(ready=True, missing_devices=0)
    first = client.post(push, headers=headers, json=request)
    assert first.status_code == 200, first.text
    assert first.json()['results'][0]['duplicate'] is False
    assert client.post(push, headers=headers, json=request).json()['results'][0]['duplicate'] is True
    changed = {**obj, 'nonce': base64.urlsafe_b64encode(b'x' * 24).decode().rstrip('=')}
    assert client.post(push, headers=headers, json=_request(a, event, changed)).status_code == 409
    params = dict(device_id=b, since=0, protocol_version=3, encrypted_sync_version=3)
    pulled = client.get('/api/v3/sync/encrypted/pull', headers=headers, params=params)
    assert pulled.status_code == 200, pulled.text
    assert pulled.json()['items'][0]['object'] == obj
    create_user(engine, username='Foreign', email='foreign@example.test')
    foreign_headers = _headers(login(client, 'Foreign').json()['access_token'])
    assert client.put(path, headers=foreign_headers, json=document_capabilities(a)).status_code == 409
    assert client.put(path, headers=headers, json=document_capabilities(c, reader_version=0)).status_code == 204
    assert client.get(path, headers=headers).json() == dict(ready=False, missing_devices=1)
    params['device_id'] = c
    blocked = client.get('/api/v3/sync/encrypted/pull', headers=headers, params=params)
    assert blocked.status_code == 409 and 'document_reader_required' in blocked.text
    params['device_id'] = b
    assert client.get('/api/v3/sync/encrypted/pull', headers=headers, params=params).status_code == 200
    assert client.post(push, headers=headers, json=_request(a,
        _event(entity_id='project-map', entity_type='document', operation='event'))).status_code == 409
    with Session(engine) as session:
        assert session.query(SyncEvent).filter_by(user_id=user).count() == 1
        encrypted = session.query(EncryptedObject).filter_by(user_id=user, event_id=event['event_id']).one()
        assert encrypted.ciphertext == base64.urlsafe_b64decode(obj['ciphertext'] + '=' * (-len(obj['ciphertext']) % 4))
        assert all(d.last_ack_sequence == 0 for d in session.query(SyncDevice).filter_by(user_id=user))
    with pytest.raises(RuntimeError, match='document history exists'):
        command.downgrade(Config(str(ROOT / 'alembic.ini')), 'c18_map_readers')
    with engine.connect() as connection:
        assert connection.execute(text('SELECT version_num FROM alembic_version')).scalar_one() == 'cloud_project_retirement'
        assert connection.execute(text('SELECT count(*) FROM sync_events')).scalar_one() == 1


def test_online_document_push_pins_snapshot_and_preserves_desktop_read_gate(cloud_client):
    client, engine = cloud_client
    user = create_user(engine)
    headers = _headers(login(client).json()['access_token'])
    desktop, web, boot = (str(uuid4()) for _ in range(3))
    sync = '/api/v3/sync/encrypted'
    project = '/api/v1/cloud/projects/project-1/bootstrap'
    assert client.put(f'/api/v1/sync/devices/{desktop}', headers=headers).status_code == 200
    assert client.post('/api/v2/sync/encrypted/cutover', headers=headers, json={'expected_cutover_epoch': 0}).status_code == 200
    assert client.post(sync + '/reader-ready', headers=headers,
                       json={'device_id': desktop, 'reader_transport_version': 3}).status_code == 204
    assert client.post(sync + '/cutover', headers=headers, json={'expected_cutover_epoch': 1}).status_code == 200
    assert client.put(sync + '/note-reader-capabilities', headers=headers, json=capabilities(desktop)).status_code == 204
    assert client.post(project, headers=headers, json={'device_id': desktop, 'bootstrap_id': boot}).status_code == 200
    assert client.post(project + '/complete', headers=headers, json={
        'device_id': desktop, 'bootstrap_id': boot, 'initial_event_count': 0, 'initial_max_server_sequence': 0,
    }).status_code == 200
    create = _event(entity_id='document-1', entity_type='document', operation='event')
    pin = {'web_document_version': 1, 'expected_epoch': 2, 'expected_cursor': 0}
    request = _request(web, create)
    assert client.post(sync + '/push', headers=headers, json=request).status_code == 409
    assert client.post(sync + '/push', headers=headers, params=pin, json=request).json()['detail']['code'] == 'document_readers_not_ready'
    assert client.put(sync + '/document-reader-capabilities', headers=headers,
                      json=document_capabilities(desktop)).status_code == 204
    first = client.post(sync + '/push', headers=headers, params=pin, json=request)
    assert first.status_code == 200, first.text
    assert first.json()['results'][0]['server_sequence'] == 1
    assert client.post(sync + '/push', headers=headers, params=pin, json=request).json()['results'][0]['duplicate']
    update = {**create, 'event_id': str(uuid4()), 'revision': 2}
    edit = {**pin, 'document_parent': create['event_id']}
    stale = client.post(sync + '/push', headers=headers, params=edit, json=_request(web, update))
    assert stale.status_code == 409 and 'web_document_snapshot_changed' in stale.text
    edit['expected_cursor'] = 1
    accepted = client.post(sync + '/push', headers=headers, params=edit, json=_request(web, update))
    assert accepted.status_code == 200, accepted.text
    pulled = client.get(sync + '/pull', headers=headers, params={
        'device_id': desktop, 'since': 0, 'protocol_version': 3, 'encrypted_sync_version': 3,
    })
    assert pulled.status_code == 200, pulled.text
    assert [item['event']['event_id'] for item in pulled.json()['items']] == [create['event_id'], update['event_id']]
    with Session(engine) as session:
        assert [str(device.device_id) for device in session.query(SyncDevice).filter_by(user_id=user)] == [desktop]
