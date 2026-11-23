"""Real PostgreSQL declaration gate; production Game reader activation is separate."""
from copy import deepcopy
from uuid import uuid4

import pytest
from alembic import command
from alembic.config import Config
from alembic.script import ScriptDirectory
from sqlalchemy import inspect, text

from backend.app.cloud.schemas import GameReaderCapabilities
from test_cloud_auth import (accept_current_legal_documents, cloud_client,
                             migrated_database, create_user, login)
from test_cloud_c15_headless_cross_runtime import ROOT, _headers
from test_cloud_c18_progress_acceptance import support

PATH = '/api/v3/sync/encrypted/game-reader-capabilities'


def declaration(device):
    def reader(codec):
        return dict(frame_version=1, codec_id=codec, codec_version=2 if codec == 13 else 1,
                    reader_version=2 if codec == 13 else 1, compression_zero=True)
    return dict(device_id=device, project=reader(12), account=reader(13))


def test_game_capability_strict_schema():
    body = declaration(str(uuid4()))
    GameReaderCapabilities.model_validate(body)
    for domain in ('project', 'account'):
        for change in ({'codec_id': 8}, {'frame_version': True},
                       {'codec_version': 1.0}, {'reader_version': 3},
                       {'compression_zero': 1}, {'balance': 10}):
            invalid = deepcopy(body)
            invalid[domain].update(change)
            with pytest.raises(ValueError):
                GameReaderCapabilities.model_validate(invalid)
    for change in ({'account': None}, {'snapshot': {}}, {'project': {}}):
        with pytest.raises(ValueError):
            GameReaderCapabilities.model_validate({**body, **change})


def test_game_registered_all_device_gate_and_progress_dependency(cloud_client):
    client, engine = cloud_client
    create_user(engine)
    headers = _headers(login(client).json()['access_token'])
    devices = [str(uuid4()) for _ in range(3)]
    for device in devices:
        assert client.put(f'/api/v1/sync/devices/{device}', headers=headers).status_code == 200
        support(client, headers, device)
    assert client.post('/api/v2/sync/encrypted/cutover', headers=headers,
                       json=dict(expected_cutover_epoch=0)).status_code == 200
    for device in devices:
        assert client.post('/api/v3/sync/encrypted/reader-ready', headers=headers,
                           json=dict(device_id=device, reader_transport_version=3)).status_code == 204
    assert client.post('/api/v3/sync/encrypted/cutover', headers=headers,
                       json=dict(expected_cutover_epoch=1)).status_code == 200
    for device in devices[:2]:
        assert client.put(PATH, headers=headers, json=declaration(device)).status_code == 204
    assert client.get(PATH, headers=headers).json() == dict(ready=False, missing_devices=1)
    assert client.get('/api/v3/sync/encrypted/progress-reader-capabilities', headers=headers).json() == dict(ready=True, missing_devices=0)
    third = declaration(devices[2])
    third['account']['reader_version'] = 0
    assert client.put(PATH, headers=headers, json=third).status_code == 204
    assert client.get(PATH, headers=headers).json() == dict(ready=False, missing_devices=1)
    assert client.put(PATH, headers=headers, json=declaration(devices[2])).status_code == 204
    assert client.get(PATH, headers=headers).json() == dict(ready=True, missing_devices=0)
    # A v1 reader still declares honestly but cannot authorize v2 publication.
    legacy = declaration(devices[2])
    legacy['account'].update(codec_version=1, reader_version=1)
    assert client.put(PATH, headers=headers, json=legacy).status_code == 204
    assert client.get(PATH, headers=headers).json() == dict(ready=False, missing_devices=1)
    assert client.put(PATH, headers=headers, json=declaration(devices[2])).status_code == 204
    # Revoking an existing dependency blocks Game again, independent of Game declarations.
    assert client.put('/api/v3/sync/encrypted/progress-reader-capabilities', headers=headers,
                      json=dict(device_id=devices[2], frame_version=1, codec_id=11,
                                codec_version=1, reader_version=0, compression_zero=True)).status_code == 204
    assert client.get(PATH, headers=headers).json() == dict(ready=False, missing_devices=1)
    assert client.put(PATH, headers=headers, json=declaration(str(uuid4()))).status_code == 409
    for domain in ('project', 'account'):
        invalid = declaration(devices[0]); invalid[domain]['codec_id'] = 11
        assert client.put(PATH, headers=headers, json=invalid).status_code == 422
    assert client.get(PATH).status_code == 401
    # Opaque transport is not admitted before the native writer/reader contract.
    with engine.connect() as db:
        assert db.execute(text('SELECT COUNT(*) FROM sync_events')).scalar_one() == 0


def test_game_forward_upgrade_preserves_registered_devices(cloud_client):
    client, engine = cloud_client
    create_user(engine)
    headers = _headers(login(client).json()['access_token'])
    device = str(uuid4())
    assert client.put(f'/api/v1/sync/devices/{device}', headers=headers).status_code == 200
    config = Config(str(ROOT / 'alembic.ini'))
    assert ScriptDirectory.from_config(config).get_heads() == ['cloud_project_retirement']
    command.downgrade(config, 'c18_progress_readers')
    with engine.connect() as db:
        before = db.execute(text('SELECT user_id,device_id,last_ack_sequence,progress_reader_version FROM sync_devices')).all()
        assert 'project_game_reader_version' not in {c['name'] for c in inspect(db).get_columns('sync_devices')}
    command.upgrade(config, 'head'); command.upgrade(config, 'head')
    with engine.connect() as db:
        assert db.execute(text('SELECT user_id,device_id,last_ack_sequence,progress_reader_version FROM sync_devices')).all() == before
        assert db.execute(text('SELECT project_game_frame_version,project_game_codec_version,project_game_reader_version,project_game_compression_zero,account_game_frame_version,account_game_codec_version,account_game_reader_version,account_game_compression_zero FROM sync_devices')).one() == (0, 0, 0, False, 0, 0, 0, False)
        assert db.execute(text('SELECT version_num FROM alembic_version')).scalar_one() == 'cloud_project_retirement'
    accept_current_legal_documents(client,headers)
    assert client.get(PATH, headers=headers).json() == dict(ready=False, missing_devices=1)


def test_game_opaque_publication_gate_blocks_both_domains_and_non_game_continuation(cloud_client):
    from test_cloud_c18_metadata import _event, _object, _request
    client, engine = cloud_client
    user = create_user(engine)
    headers = _headers(login(client).json()['access_token'])
    devices = [str(uuid4()) for _ in range(3)]
    for device in devices:
        assert client.put(f'/api/v1/sync/devices/{device}', headers=headers).status_code == 200
        support(client, headers, device)
    assert client.post('/api/v1/cloud/projects/project-1', headers=headers).status_code == 200
    assert client.post('/api/v2/sync/encrypted/cutover', headers=headers, json=dict(expected_cutover_epoch=0)).status_code == 200
    for device in devices:
        assert client.post('/api/v3/sync/encrypted/reader-ready', headers=headers,
                           json=dict(device_id=device, reader_transport_version=3)).status_code == 204
    assert client.post('/api/v3/sync/encrypted/cutover', headers=headers, json=dict(expected_cutover_epoch=1)).status_code == 200
    for device in devices[:2]:
        assert client.put(PATH, headers=headers, json=declaration(device)).status_code == 204
    project = _event(entity_type='project_game', operation='event')
    project['entity_id'] = f"game:project:{project['event_id']}"
    reward_id = str(uuid4())
    reward = dict(event_id=reward_id, canonical_user_id=str(user), scope='account',
                  entity_type='account_game', entity_id=f'game:{reward_id}',
                  operation='upsert', revision=1, updated_at=project['updated_at'], deleted_at=None)
    obj = _object()
    account_obj = {**obj, 'crypto_version': 2, 'aad_version': 2}
    request = _request(devices[0], project, obj)
    account_request = {**request, 'items': [dict(event=reward, object=account_obj)]}
    account_push = '/api/v3/sync/encrypted/account/push'
    push = '/api/v3/sync/encrypted/push'
    denied = client.post(push, headers=headers, json=request)
    assert denied.status_code == 409 and denied.json()['detail']['code'] == 'game_readers_not_ready', denied.text
    denied_account = client.post(account_push, headers=headers, json=account_request)
    assert denied_account.status_code == 409 and denied_account.json()['detail']['code'] == 'game_readers_not_ready', denied_account.text
    with engine.connect() as db:
        assert db.execute(text('SELECT count(*) FROM sync_events')).scalar_one() == 0
    # Progress11 remains usable while C cannot read Game. Both opaque domains
    # of the pending pair remain identical when publication later becomes legal.
    progress = _event(entity_type='progress', entity_id='project', operation='event')
    assert client.post(push, headers=headers, json=_request(devices[0], progress)).status_code == 200
    assert client.put(PATH, headers=headers, json=declaration(devices[2])).status_code == 204
    for endpoint, body in ((push, request), (account_push, account_request)):
        first = client.post(endpoint, headers=headers, json=body)
        assert first.status_code == 200, first.text
        repeat = client.post(endpoint, headers=headers, json=body)
        assert repeat.status_code == 200 and all(r['duplicate'] for r in repeat.json()['results'])
        assert [r['server_sequence'] for r in first.json()['results']] == [r['server_sequence'] for r in repeat.json()['results']]
    pulled = client.get('/api/v3/sync/encrypted/pull', headers=headers, params=dict(
        device_id=devices[1], since=0, limit=100, protocol_version=3, encrypted_sync_version=3))
    assert pulled.status_code == 200, pulled.text
    items = {v['event']['event_id']: v for v in pulled.json()['items']}
    assert items[project['event_id']]['object'] == obj
    assert items[reward_id]['object'] == account_obj
    import base64
    altered = deepcopy(account_request)
    altered['items'][0]['object']['nonce'] = base64.urlsafe_b64encode(b'x' * 24).decode().rstrip('=')
    assert client.post(account_push, headers=headers, json=altered).status_code == 409
    with engine.connect() as db:
        rows = db.execute(text('SELECT project_id,entity_type FROM sync_events ORDER BY server_sequence')).all()
        assert rows == [('project-1', 'progress'), ('project-1', 'project_game'), (None, 'account_game')]
        from sqlalchemy import inspect
        columns = {c['name'] for c in inspect(db).get_columns('sync_events')}
        assert not columns.intersection({'coins','experience','reward','inventory','quest','bank','frame','action','payload'})
    # Downgrade refuses to remove the ability to read retained Game history.
    config = Config(str(ROOT / 'alembic.ini'))
    with pytest.raises(RuntimeError, match='Game history'):
        command.downgrade(config, 'c18_progress_readers')


def test_loss_reader_upgrade_and_safe_downgrade(cloud_client):
    client, engine = cloud_client
    create_user(engine)
    headers = _headers(login(client).json()['access_token'])
    device = str(uuid4())
    assert client.put(f'/api/v1/sync/devices/{device}', headers=headers).status_code == 200
    assert client.put(PATH, headers=headers, json=declaration(device)).status_code == 204
    config = Config(str(ROOT / 'alembic.ini'))
    command.downgrade(config, 'legal_acceptances')
    with engine.connect() as db:
        assert db.execute(text('SELECT account_game_codec_version,account_game_reader_version FROM sync_devices')).one() == (0, 0)
    command.upgrade(config, 'head'); command.upgrade(config, 'head')
    with engine.connect() as db:
        assert str(db.execute(text('SELECT device_id FROM sync_devices')).scalar_one()) == device
