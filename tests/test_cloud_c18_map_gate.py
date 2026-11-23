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
from backend.app.cloud.schemas import MapReaderCapabilities, V3EncryptedSyncPushRequest
from test_cloud_auth import ROOT, cloud_client, create_user, login, migrated_database
from test_cloud_c18_content_note_gate import capabilities
from test_cloud_c18_metadata import _event, _headers, _object, _request


def map_capabilities(device, **changes):
    return {**dict(device_id=device, frame_version=1, codec_id=9, codec_version=1,
                   reader_version=1, compression_zero=True), **changes}


def test_map_descriptor_and_capability_contract():
    device = str(uuid4())
    body = map_capabilities(device)
    MapReaderCapabilities.model_validate(body)
    for changes in ({'title': 'plaintext'}, {'codec_id': 8}, {'codec_version': True},
                    {'frame_version': '1'}, {'reader_version': 2}, {'compression_zero': 1}):
        with pytest.raises(ValueError):
            MapReaderCapabilities.model_validate({**body, **changes})
    for entity in ('project-map', 'stage-map-' + hashlib.sha256(b'S1').hexdigest()):
        event = _event(entity_id=entity, entity_type='map', operation='event')
        V3EncryptedSyncPushRequest.model_validate(_request(device, event))
        for changes in ({'entity_id': 'combined-map'}, {'entity_id': 'S1'},
                        {'operation': 'upsert'}, {'deleted_at': '2026-10-03T00:00:00Z'},
                        {'nodes': []}):
            with pytest.raises(ValueError):
                V3EncryptedSyncPushRequest.model_validate(_request(device, {**event, **changes}))


def test_map_reader_upgrade_preserves_populated_note_reader_baseline(cloud_client):
    client, engine = cloud_client
    user = create_user(engine)
    headers = _headers(login(client).json()['access_token'])
    device = str(uuid4())
    assert client.put(f'/api/v1/sync/devices/{device}', headers=headers).status_code == 200
    assert client.put('/api/v3/sync/encrypted/note-reader-capabilities', headers=headers,
                      json=capabilities(device)).status_code == 204
    config = Config(str(ROOT / 'alembic.ini'))
    command.downgrade(config, 'c18_note_readers')
    with engine.connect() as connection:
        before = connection.execute(text('SELECT user_id,device_id,last_ack_sequence,note_codec_version,'
            'note_resolution_reader_version,note_compression_zero FROM sync_devices')).all()
        assert 'map_reader_version' not in {c['name'] for c in inspect(connection).get_columns('sync_devices')}
    command.upgrade(config, 'head')
    command.upgrade(config, 'head')
    with engine.connect() as connection:
        after = connection.execute(text('SELECT user_id,device_id,last_ack_sequence,note_codec_version,'
            'note_resolution_reader_version,note_compression_zero FROM sync_devices')).all()
        assert after == before
        assert connection.execute(text('SELECT map_frame_version,map_codec_version,map_reader_version,'
            'map_compression_zero FROM sync_devices')).one() == (0, 0, 0, False)
        assert connection.execute(text('SELECT version_num FROM alembic_version')).scalar_one() == 'cloud_project_retirement'
    with Session(engine) as session:
        from backend.app.cloud.services import SyncService
        assert not SyncService.map_reader_ready(session.query(SyncDevice).filter_by(user_id=user).one())


def test_map_gate_three_registered_devices_exact_replay_and_downgrade(cloud_client):
    client, engine = cloud_client
    user = create_user(engine)
    headers = _headers(login(client).json()['access_token'])
    a, b, c = [str(uuid4()) for _ in range(3)]
    path = '/api/v3/sync/encrypted/map-reader-capabilities'
    notes = '/api/v3/sync/encrypted/note-reader-capabilities'
    push = '/api/v3/sync/encrypted/push'
    assert client.get(path).status_code == 401
    assert client.put(path, headers=headers, json=map_capabilities(a)).status_code == 409
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
        assert client.put(path, headers=headers, json=map_capabilities(device)).status_code == 204
    assert client.get(notes, headers=headers).json()['ready'] is True
    assert client.get(path, headers=headers).json() == dict(ready=False, missing_devices=1)
    assert client.post('/api/v1/cloud/projects/project-1', headers=headers).status_code == 200
    event = _event(entity_id='project-map', entity_type='map', operation='event')
    obj = _object()
    request = _request(a, event, obj)
    blocked = client.post(push, headers=headers, json=request)
    assert blocked.status_code == 409 and 'map_readers_not_ready' in blocked.text
    assert client.put(path, headers=headers, json=map_capabilities(c)).status_code == 204
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
    assert client.put(path, headers=foreign_headers, json=map_capabilities(a)).status_code == 409
    assert client.put(path, headers=headers, json=map_capabilities(c, reader_version=0)).status_code == 204
    assert client.get(path, headers=headers).json() == dict(ready=False, missing_devices=1)
    params['device_id'] = c
    blocked = client.get('/api/v3/sync/encrypted/pull', headers=headers, params=params)
    assert blocked.status_code == 409 and 'map_reader_required' in blocked.text
    params['device_id'] = b
    assert client.get('/api/v3/sync/encrypted/pull', headers=headers, params=params).status_code == 200
    assert client.post(push, headers=headers, json=_request(a,
        _event(entity_id='project-map', entity_type='map', operation='event'))).status_code == 409
    with Session(engine) as session:
        assert session.query(SyncEvent).filter_by(user_id=user).count() == 1
        encrypted = session.query(EncryptedObject).filter_by(user_id=user, event_id=event['event_id']).one()
        assert encrypted.ciphertext == base64.urlsafe_b64decode(obj['ciphertext'] + '=' * (-len(obj['ciphertext']) % 4))
        assert all(d.last_ack_sequence == 0 for d in session.query(SyncDevice).filter_by(user_id=user))
    with pytest.raises(RuntimeError, match='map history exists'):
        command.downgrade(Config(str(ROOT / 'alembic.ini')), 'c18_note_readers')
    with engine.connect() as connection:
        assert connection.execute(text('SELECT version_num FROM alembic_version')).scalar_one() == 'cloud_project_retirement'
        assert connection.execute(text('SELECT count(*) FROM sync_events')).scalar_one() == 1
