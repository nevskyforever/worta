"""Real PostgreSQL metadata-v2 reader declaration and publication gate."""
from copy import deepcopy
from uuid import uuid4

import pytest
from alembic import command
from alembic.config import Config
from alembic.script import ScriptDirectory
from sqlalchemy import inspect, text

from backend.app.cloud.schemas import ProjectCoverReaderCapabilities
from test_cloud_auth import cloud_client, migrated_database, create_user, login
from test_cloud_c15_headless_cross_runtime import ROOT, _headers

PATH = '/api/v3/sync/encrypted/cover-reader-capabilities'


def declaration(device):
    return dict(device_id=device, frame_version=1, codec_id=1,
                metadata_codec_version=2, cover_crypto_version=1,
                cover_aad_version=1, compression_zero=True, reader_version=2)


def test_cover_declaration_strict():
    body = declaration(str(uuid4()))
    ProjectCoverReaderCapabilities.model_validate(body)
    for field in ('frame_version', 'codec_id', 'metadata_codec_version',
                  'cover_crypto_version', 'cover_aad_version', 'reader_version'):
        for value in (True, 1.0, '1', 14):
            with pytest.raises(ValueError):
                ProjectCoverReaderCapabilities.model_validate({**body, field: value})
    for change in ({'compression_zero': 1}, {'compression_zero': False},
                   {'pixels': 'secret'}, {'device_id': 'invalid'}):
        with pytest.raises(ValueError):
            ProjectCoverReaderCapabilities.model_validate({**body, **change})


def mode3(client, headers, devices):
    for device in devices:
        assert client.put(f'/api/v1/sync/devices/{device}', headers=headers).status_code == 200
    assert client.post('/api/v2/sync/encrypted/cutover', headers=headers,
                       json=dict(expected_cutover_epoch=0)).status_code == 200
    for device in devices:
        assert client.post('/api/v3/sync/encrypted/reader-ready', headers=headers,
                           json=dict(device_id=device, reader_transport_version=3)).status_code == 204
    assert client.post('/api/v3/sync/encrypted/cutover', headers=headers,
                       json=dict(expected_cutover_epoch=1)).status_code == 200


def test_cover_gate_old_device_exact_retry_and_v1_continuation(cloud_client):
    from test_cloud_c18_metadata import _event, _request
    client, engine = cloud_client
    create_user(engine)
    headers = _headers(login(client).json()['access_token'])
    devices = [str(uuid4()) for _ in range(3)]
    mode3(client, headers, devices)
    assert client.post('/api/v1/cloud/projects/project-1', headers=headers).status_code == 200
    for device in devices[:2]:
        response = client.put(PATH, headers=headers, json=declaration(device))
        assert response.status_code == 204, response.text
    assert client.get(PATH, headers=headers).json() == dict(ready=False, missing_devices=1)
    event = _event(entity_type='project_metadata', entity_id='project-1')
    request = _request(devices[0], event)
    endpoint = '/api/v3/sync/encrypted/cover-metadata/push'
    rejected = client.post(endpoint, headers=headers, json=request)
    assert rejected.status_code == 409 and rejected.json()['detail']['code'] == 'cover_readers_not_ready', rejected.text
    with engine.connect() as db:
        assert db.execute(text('SELECT count(*) FROM sync_events')).scalar_one() == 0
    # Existing v1 opaque metadata transport remains available while C is old.
    first = client.post('/api/v3/sync/encrypted/push', headers=headers, json=request)
    assert first.status_code == 200, first.text
    assert client.put(PATH, headers=headers, json=declaration(devices[2])).status_code == 204
    assert client.get(PATH, headers=headers).json() == dict(ready=True, missing_devices=0)
    replay = client.post(endpoint, headers=headers, json=request)
    assert replay.status_code == 200, replay.text
    assert replay.json()['results'][0]['duplicate'] is True
    assert replay.json()['results'][0]['server_sequence'] == first.json()['results'][0]['server_sequence']
    revoked = {**declaration(devices[2]), 'reader_version': 0}
    assert client.put(PATH, headers=headers, json=revoked).status_code == 204
    assert client.get(PATH, headers=headers).json() == dict(ready=False, missing_devices=1)
    assert client.put(PATH, headers=headers, json=declaration(str(uuid4()))).status_code == 409
    assert client.get(PATH).status_code == 401
    altered = deepcopy(request)
    altered['items'][0]['event']['entity_type'] = 'note'
    assert client.put(PATH, headers=headers, json=declaration(devices[2])).status_code == 204
    denied = client.post(endpoint, headers=headers, json=altered)
    assert denied.status_code == 409 and denied.json()['detail']['code'] == 'cover_readers_not_ready'


def test_cover_forward_upgrade_preserves_devices(cloud_client):
    client, engine = cloud_client
    create_user(engine)
    headers = _headers(login(client).json()['access_token'])
    device = str(uuid4())
    assert client.put(f'/api/v1/sync/devices/{device}', headers=headers).status_code == 200
    config = Config(str(ROOT / 'alembic.ini'))
    assert ScriptDirectory.from_config(config).get_heads() == ['cloud_project_retirement']
    command.downgrade(config, 'c18_game_readers')
    with engine.connect() as db:
        before = db.execute(text('SELECT user_id,device_id,last_ack_sequence FROM sync_devices')).all()
        assert 'metadata_cover_reader_version' not in {c['name'] for c in inspect(db).get_columns('sync_devices')}
    command.upgrade(config, 'head')
    command.upgrade(config, 'head')
    with engine.connect() as db:
        assert db.execute(text('SELECT user_id,device_id,last_ack_sequence FROM sync_devices')).all() == before
        assert db.execute(text('SELECT metadata_cover_reader_version FROM sync_devices')).scalar_one() == 0
        assert db.execute(text('SELECT version_num FROM alembic_version')).scalar_one() == 'cloud_project_retirement'
