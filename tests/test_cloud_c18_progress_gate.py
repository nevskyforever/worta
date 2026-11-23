"""Normalized codec11 capabilities and forward-only PostgreSQL upgrade."""
from uuid import uuid4
import pytest
from alembic import command
from alembic.config import Config
from sqlalchemy import text,inspect
from test_cloud_auth import (accept_current_legal_documents, cloud_client,
                             migrated_database, create_user, login)
from test_cloud_c15_headless_cross_runtime import ROOT,_headers
from test_cloud_c18_progress_acceptance import support
from backend.app.cloud.schemas import V3EncryptedSyncPushRequest
from test_cloud_c18_metadata import _event,_request

def test_progress_descriptor_rejects_plaintext_wrong_operation():
    device=str(uuid4());event=_event(entity_id='project',entity_type='progress',operation='event')
    V3EncryptedSyncPushRequest.model_validate(_request(device,event))
    for values in ({'operation':'upsert'},{'deleted_at':'2026-10-03T00:00:00Z'},{'new_total':100},{'delta':20},{'unit':'symbols'}):
        with pytest.raises(ValueError):V3EncryptedSyncPushRequest.model_validate(_request(device,{**event,**values}))

def test_progress_upgrade_registered_gate_and_normalized_defaults(cloud_client):
    client,engine=cloud_client;create_user(engine);headers=_headers(login(client).json()['access_token']);device=str(uuid4())
    assert client.put(f'/api/v1/sync/devices/{device}',headers=headers).status_code==200
    config=Config(str(ROOT/'alembic.ini'));command.downgrade(config,'c18_document_readers')
    with engine.connect() as db:
        before=db.execute(text('SELECT user_id,device_id,last_ack_sequence,note_codec_version,document_codec_version FROM sync_devices')).all()
        assert 'progress_reader_version' not in {c['name'] for c in inspect(db).get_columns('sync_devices')}
    command.upgrade(config,'head');command.upgrade(config,'head')
    with engine.connect() as db:
        assert db.execute(text('SELECT user_id,device_id,last_ack_sequence,note_codec_version,document_codec_version FROM sync_devices')).all()==before
        assert db.execute(text('SELECT progress_frame_version,progress_codec_version,progress_reader_version,progress_compression_zero FROM sync_devices')).one()==(0,0,0,False)
        assert db.execute(text('SELECT version_num FROM alembic_version')).scalar_one()=='cloud_project_retirement'
    accept_current_legal_documents(client,headers)
    assert client.get('/api/v3/sync/encrypted/progress-reader-capabilities',headers=headers).json()==dict(ready=False,missing_devices=1)
    support(client,headers,device)
    # Prerequisite transport mode is mandatory, independent of codec declaration.
    assert client.get('/api/v3/sync/encrypted/progress-reader-capabilities',headers=headers).json()==dict(ready=False,missing_devices=0)
    assert client.post('/api/v2/sync/encrypted/cutover',headers=headers,json=dict(expected_cutover_epoch=0)).status_code==200
    assert client.post('/api/v3/sync/encrypted/reader-ready',headers=headers,json=dict(device_id=device,reader_transport_version=3)).status_code==204
    assert client.post('/api/v3/sync/encrypted/cutover',headers=headers,json=dict(expected_cutover_epoch=1)).status_code==200
    assert client.get('/api/v3/sync/encrypted/progress-reader-capabilities',headers=headers).json()==dict(ready=True,missing_devices=0)
    path='/api/v3/sync/encrypted/progress-reader-capabilities'
    for change in ({'codec_id':10},{'reader_version':True},{'compression_zero':1},{'unknown':1}):
        body=dict(device_id=device,frame_version=1,codec_id=11,codec_version=1,reader_version=1,compression_zero=True);body.update(change)
        assert client.put(path,headers=headers,json=body).status_code==422
    body=dict(device_id=str(uuid4()),frame_version=1,codec_id=11,codec_version=1,reader_version=1,compression_zero=True)
    assert client.put(path,headers=headers,json=body).status_code==409
