"""Real PostgreSQL explicit ID1 capability, late readers, and forward migration."""
from uuid import uuid4
import pytest
from alembic import command
from alembic.config import Config
from alembic.script import ScriptDirectory
from sqlalchemy import inspect, text
from test_cloud_auth import (accept_current_legal_documents, cloud_client,
                             migrated_database, create_user, login)
from test_cloud_c18_cover_gate import mode3
from test_cloud_c18_metadata import _headers, _event, _request
from test_cloud_c15_headless_cross_runtime import ROOT

PATH = '/api/v3/sync/encrypted/compression-reader-capabilities'
AUTHORIZE = '/api/v3/sync/encrypted/compression-writer'


def declare(client, headers, device, support=True):
    return client.put(PATH, headers=headers, json=dict(device_id=device, compression_id1=support))


def test_account_gate_late_registration_history_read_barrier_and_id0_continuation(cloud_client):
    client, engine = cloud_client
    create_user(engine)
    headers = _headers(login(client).json()['access_token'])
    a, b, c, d = [str(uuid4()) for _ in range(4)]
    mode3(client, headers, [a, b])
    gate = lambda: client.get(PATH, headers=headers).json()
    authorize = lambda: client.post(AUTHORIZE, headers=headers, json=dict(device_id=a)).json()
    assert gate() == dict(ready=False, missing_devices=2)
    assert declare(client, headers, a).status_code == 204
    assert gate() == dict(ready=False, missing_devices=1)
    assert declare(client, headers, b, False).status_code == 204
    assert authorize() == dict(ready=False, missing_devices=1)
    assert declare(client, headers, b).status_code == 204
    assert authorize() == dict(ready=True, missing_devices=0)
    with engine.connect() as db:
        assert db.execute(text('SELECT compression_id1_required FROM sync_user_state')).scalar_one() is True
    assert client.post('/api/v1/cloud/projects/project-1', headers=headers).status_code == 200
    request = _request(a, _event())
    first = client.post('/api/v3/sync/encrypted/push', headers=headers, json=request)
    assert first.status_code == 200, first.text
    # New explicitly capable reader joins; safe ID1 eligibility resumes.
    assert client.put(f'/api/v1/sync/devices/{c}', headers=headers).status_code == 200
    assert gate() == dict(ready=False, missing_devices=1)
    assert declare(client, headers, c).status_code == 204
    assert gate() == dict(ready=True, missing_devices=0)
    # New legacy reader is ID0-only. Existing history cannot be delivered or ACKed.
    assert client.put(f'/api/v1/sync/devices/{d}', headers=headers).status_code == 200
    assert authorize() == dict(ready=False, missing_devices=1)
    params=dict(device_id=d, since=0, limit=200, protocol_version=3, encrypted_sync_version=3)
    denied=client.get('/api/v3/sync/encrypted/pull',headers=headers,params=params)
    assert denied.status_code == 409 and denied.json()['detail']['code']=='compression_reader_required'
    ack=client.post('/api/v3/sync/encrypted/ack',headers=headers,json=dict(protocol_version=3,encrypted_sync_version=3,device_id=d,cursor=1))
    assert ack.status_code==409 and ack.json()['detail']['code']=='compression_reader_required'
    # Gate false doesn't prevent ordinary ID0 publications by capable clients.
    second=client.post('/api/v3/sync/encrypted/push',headers=headers,json=_request(a,_event(revision=2)))
    assert second.status_code==200,second.text
    replay=client.post('/api/v3/sync/encrypted/push',headers=headers,json=request)
    assert replay.status_code==200 and replay.json()['results'][0]['duplicate'] is True
    assert declare(client,headers,d).status_code==204
    pulled=client.get('/api/v3/sync/encrypted/pull',headers=headers,params=params)
    assert pulled.status_code==200 and len(pulled.json()['items'])==2,pulled.text
    assert gate()==dict(ready=True,missing_devices=0)
    assert declare(client,headers,b,False).status_code==204
    assert gate()==dict(ready=False,missing_devices=1)
    config=Config(str(ROOT/'alembic.ini'))
    with pytest.raises(RuntimeError,match='retained ciphertext'):
        command.downgrade(config,'c18_cover_readers')


@pytest.mark.parametrize('bad', [None, 0, 1, 'true', [], {}])
def test_capability_api_strict_boolean_and_registered_account_scope(cloud_client,bad):
    client,engine=cloud_client;create_user(engine)
    headers=_headers(login(client).json()['access_token']);device=str(uuid4())
    assert client.put(f'/api/v1/sync/devices/{device}',headers=headers).status_code==200
    assert declare(client,headers,device,bad).status_code==422
    assert client.put(PATH,headers=headers,json=dict(device_id=device)).status_code==422
    assert client.put(PATH,headers=headers,json=dict(device_id=device,compression_id1=True,extra=True)).status_code==422
    assert declare(client,headers,str(uuid4())).status_code==409
    assert client.post(AUTHORIZE,headers=headers,json=dict(device_id=str(uuid4()))).status_code==409
    with engine.connect() as db:
        assert db.execute(text('SELECT compression_id1 FROM sync_devices')).scalar_one() is False


def test_upgrade_populated_old_registry_safe_defaults_and_reversible_before_activation(cloud_client):
    client,engine=cloud_client;create_user(engine)
    headers=_headers(login(client).json()['access_token']);device=str(uuid4())
    assert client.put(f'/api/v1/sync/devices/{device}',headers=headers).status_code==200
    config=Config(str(ROOT/'alembic.ini'))
    assert ScriptDirectory.from_config(config).get_heads()==['cloud_project_retirement']
    command.downgrade(config,'c18_cover_readers')
    with engine.connect() as db:
        before=db.execute(text('SELECT user_id,device_id,last_ack_sequence FROM sync_devices')).all()
        columns={c['name'] for c in inspect(db).get_columns('sync_devices')}
        constraints=inspect(db).get_check_constraints('sync_devices')
        state_columns={c['name'] for c in inspect(db).get_columns('sync_user_state')}
    command.upgrade(config,'head');command.upgrade(config,'head')
    with engine.connect() as db:
        assert db.execute(text('SELECT user_id,device_id,last_ack_sequence FROM sync_devices')).all()==before
        assert db.execute(text('SELECT compression_id1 FROM sync_devices')).scalar_one() is False
        assert db.execute(text('SELECT compression_id1_required FROM sync_user_state')).scalar_one() is False
        assert {c['name'] for c in inspect(db).get_columns('sync_devices')}==columns|{'compression_id1'}
        assert {c['name'] for c in inspect(db).get_columns('sync_user_state')}==state_columns|{'compression_id1_required'}
        # The later Game v2 migration extends precisely these two checks.
        expected = [dict(c) for c in constraints]
        assert {'ck_sync_devices_account_game_codec_version', 'ck_sync_devices_account_game_reader_version'} <= {c['name'] for c in expected}
        for c in expected:
            if c['name'] in {'ck_sync_devices_account_game_codec_version', 'ck_sync_devices_account_game_reader_version'}:
                assert c['sqltext'].endswith('ARRAY[0, 1])')
                c['sqltext'] = c['sqltext'].replace('ARRAY[0, 1])', 'ARRAY[0, 1, 2])')
        assert inspect(db).get_check_constraints('sync_devices')==expected
    accept_current_legal_documents(client,headers)
    assert declare(client,headers,device).status_code==204
    command.downgrade(config,'c18_cover_readers');command.upgrade(config,'head')
    with engine.connect() as db:
        assert db.execute(text('SELECT compression_id1 FROM sync_devices')).scalar_one() is False


def test_capability_cannot_cross_account_or_count_unrelated_devices(cloud_client):
    client,engine=cloud_client
    create_user(engine);create_user(engine,username='Other',email='other@example.test')
    owner=_headers(login(client).json()['access_token'])
    other=_headers(login(client,username='Other').json()['access_token'])
    a,b=str(uuid4()),str(uuid4())
    mode3(client,owner,[a])
    assert client.put(f'/api/v1/sync/devices/{b}',headers=other).status_code==200
    assert declare(client,owner,b).status_code==409
    assert declare(client,other,a).status_code==409
    assert declare(client,owner,a).status_code==204
    assert client.get(PATH,headers=owner).json()==dict(ready=True,missing_devices=0)
    assert client.get(PATH,headers=other).json()==dict(ready=False,missing_devices=1)


def test_failed_seal_lost_admission_response_barrier_is_sticky_and_recoverable(cloud_client):
    """Admission is durable even with no subsequent upload; never guess rollback."""
    client,engine=cloud_client;create_user(engine)
    headers=_headers(login(client).json()['access_token']);a,b,legacy=[str(uuid4()) for _ in range(3)]
    mode3(client,headers,[a,b])
    assert client.post('/api/v1/cloud/projects/project-1',headers=headers).status_code==200
    initial=_request(a,_event())
    assert client.post('/api/v3/sync/encrypted/push',headers=headers,json=initial).status_code==200
    for device in [a,b]:assert declare(client,headers,device).status_code==204
    with engine.connect() as db:
        original=db.execute(text('SELECT nonce,ciphertext FROM encrypted_objects')).one()
        assert db.execute(text('SELECT compression_id1_required FROM sync_user_state')).scalar_one() is False
    # Receive no usable response; server still commits. Crash before any seal/upload.
    assert client.post(AUTHORIZE,headers=headers,json=dict(device_id=a)).status_code==200
    retry=client.post(AUTHORIZE,headers=headers,json=dict(device_id=a))
    assert retry.json()==dict(ready=True,missing_devices=0)
    with engine.connect() as db:
        assert db.execute(text('SELECT compression_id1_required FROM sync_user_state')).scalar_one() is True
        assert db.execute(text('SELECT current_sequence FROM sync_user_state')).scalar_one()==1
        assert db.execute(text('SELECT nonce,ciphertext FROM encrypted_objects')).one()==original
    assert client.put(f'/api/v1/sync/devices/{legacy}',headers=headers).status_code==200
    params=dict(device_id=legacy,since=0,protocol_version=3,encrypted_sync_version=3)
    assert client.get('/api/v3/sync/encrypted/pull',headers=headers,params=params).json()['detail']['code']=='compression_reader_required'
    assert client.post('/api/v3/sync/encrypted/ack',headers=headers,json=dict(device_id=legacy,cursor=1,protocol_version=3,encrypted_sync_version=3)).status_code==409
    assert client.post(AUTHORIZE,headers=headers,json=dict(device_id=a)).json()['ready'] is False
    with engine.connect() as db:
        assert db.execute(text('SELECT last_ack_sequence FROM sync_devices WHERE device_id=:id'),dict(id=legacy)).scalar_one()==0
        assert db.execute(text('SELECT current_sequence FROM sync_user_state')).scalar_one()==1
    assert declare(client,headers,legacy).status_code==204
    resumed=client.get('/api/v3/sync/encrypted/pull',headers=headers,params=params)
    assert resumed.status_code==200 and len(resumed.json()['items'])==1
    with engine.connect() as db:
        assert db.execute(text('SELECT nonce,ciphertext FROM encrypted_objects')).one()==original
        assert db.execute(text('SELECT COUNT(*) FROM sync_events')).scalar_one()==1


def test_only_id0_history_never_sets_barrier_without_admission(cloud_client):
    client,engine=cloud_client;create_user(engine)
    headers=_headers(login(client).json()['access_token']);a,b=[str(uuid4()) for _ in range(2)]
    mode3(client,headers,[a,b])
    assert client.post('/api/v1/cloud/projects/project-1',headers=headers).status_code==200
    assert client.post('/api/v3/sync/encrypted/push',headers=headers,json=_request(a,_event())).status_code==200
    assert client.get('/api/v3/sync/encrypted/pull',headers=headers,params=dict(device_id=b,since=0,protocol_version=3,encrypted_sync_version=3)).status_code==200
    assert client.post('/api/v3/sync/encrypted/ack',headers=headers,json=dict(device_id=b,cursor=1,protocol_version=3,encrypted_sync_version=3)).status_code==204
    for device in [a,b]:assert declare(client,headers,device).status_code==204
    assert client.get(PATH,headers=headers).json()['ready'] is True
    with engine.connect() as db:
        assert db.execute(text('SELECT compression_id1_required FROM sync_user_state')).scalar_one() is False
