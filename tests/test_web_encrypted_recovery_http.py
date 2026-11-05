"""Authenticated read-only recovery delivery on isolated PostgreSQL."""
import json
import socket
import subprocess
import threading

import uvicorn
import pytest
from fastapi import HTTPException
from sqlalchemy import event as sqlalchemy_event, select
from sqlalchemy.orm import Session

from backend.app.cloud.models import EncryptedObject, SyncDevice, SyncUserState
from test_cloud_auth import ROOT, cloud_client, create_user, login, migrated_database
from test_cloud_encrypted_sync import _device, _enable, _encrypted_request, _event, _headers, _object
from backend.app.cloud.sync_v3_router import _recovery_budget


def test_recovery_budget_bounds_rate_and_account_concurrency():
    from uuid import uuid4
    account, session = uuid4(), uuid4()
    with _recovery_budget(account, session):
        with pytest.raises(HTTPException) as busy:
            with _recovery_budget(account, uuid4()):
                pass
        assert busy.value.status_code == 429
    for _ in range(119):
        with _recovery_budget(account, session):
            pass
    with pytest.raises(HTTPException) as limited:
        with _recovery_budget(account, session):
            pass
    assert limited.value.status_code == 429


def test_recovery_http_account_snapshot_and_unchanged_sync_state(cloud_client):
    client, engine = cloud_client
    user = create_user(engine)
    token = login(client).json()['access_token']
    headers = _headers(token)
    device = _device(client, token)
    _enable(client, token)
    first, second = _event(entity_id='n1'), _event(entity_id='n2')
    for event in (first, second):
        response = client.post('/api/v1/sync/encrypted/push', headers=headers,
            json=_encrypted_request(device, (event, _object())))
        assert response.status_code == 200, response.text
    assert client.post('/api/v2/sync/encrypted/cutover', headers=headers,
        json={'expected_cutover_epoch': 0}).status_code == 200
    assert client.post('/api/v3/sync/encrypted/reader-ready', headers=headers,
        json={'device_id': device, 'reader_transport_version': 3}).status_code == 204
    assert client.post('/api/v3/sync/encrypted/cutover', headers=headers,
        json={'expected_cutover_epoch': 1}).status_code == 200
    with Session(engine) as db:
        before = (db.get(SyncUserState, user).current_sequence,
                  db.scalar(select(SyncDevice.last_ack_sequence).where(SyncDevice.user_id == user)),
                  db.scalar(select(EncryptedObject.ciphertext).where(EncryptedObject.user_id == user)))
    path = '/api/v3/recovery/encrypted/history'
    params = {'since': 0, 'limit': 1, 'expected_cursor': 2, 'expected_epoch': 2}
    assert client.get(path, params=params).status_code == 401
    statements = []
    def capture(_conn, _cursor, statement, _parameters, _context, _many):
        statements.append(statement.strip().upper())
    app_engine = client.app.state.cloud_database.engine
    sqlalchemy_event.listen(app_engine, 'before_cursor_execute', capture)
    try:
        response = client.get(path, headers=headers, params=params)
    finally:
        sqlalchemy_event.remove(app_engine, 'before_cursor_execute', capture)
    assert response.status_code == 200, response.text
    assert statements[0] == 'SET TRANSACTION READ ONLY'
    assert statements[1] == "SET LOCAL STATEMENT_TIMEOUT = '10S'"
    assert not any(statement.startswith(('INSERT ', 'UPDATE ', 'DELETE ')) for statement in statements)
    page = response.json()
    assert response.headers['cache-control'] == 'no-store'
    assert page['items'][0]['event']['event_id'] == first['event_id']
    assert page['items'][0]['object'] == _object()
    assert page['next_cursor'] == 1 and page['has_more'] is True
    # Real loopback HTTP and a real Chromium request, not a mocked fetch.
    api_socket = socket.socket()
    api_socket.bind(('127.0.0.1', 0))
    port = api_socket.getsockname()[1]
    server = uvicorn.Server(uvicorn.Config(client.app, lifespan='off', access_log=False, log_level='warning'))
    thread = threading.Thread(target=server.run, kwargs={'sockets': [api_socket]}, daemon=True)
    thread.start()
    try:
        script = '''import {chromium} from '@playwright/test';
const b=await chromium.launch({headless:true});try{const p=await b.newPage();
await p.goto(process.env.RECOVERY_URL + '/docs');
const result=await p.evaluate(async token=>{const r=await fetch('/api/v3/recovery/encrypted/history?since=0&limit=1&expected_cursor=2&expected_epoch=2',{headers:{Authorization:`Bearer ${token}`}});return {status:r.status,body:await r.json()}},process.env.RECOVERY_TOKEN);
console.log(JSON.stringify(result));}finally{await b.close()}'''
        import os
        result = subprocess.run(['node', '--input-type=module', '-e', script], cwd=ROOT / 'frontend',
            env={**os.environ, 'RECOVERY_URL': f'http://127.0.0.1:{port}', 'RECOVERY_TOKEN': token},
            capture_output=True, text=True, timeout=90)
        assert result.returncode == 0, result.stderr
        chromium_page = json.loads(result.stdout.strip().splitlines()[-1])
        assert chromium_page['status'] == 200 and chromium_page['body'] == page
    finally:
        server.should_exit = True
        thread.join(timeout=15)
        api_socket.close()
    second_page = client.get(path, headers=headers, params={**params, 'since': 1}).json()
    assert second_page['items'][0]['event']['event_id'] == second['event_id']
    assert second_page['has_more'] is False
    assert client.get(path, headers=headers, params={**params, 'expected_cursor': 3}).status_code == 409
    assert client.get(path, headers=headers, params={**params, 'account_id': str(user)}).status_code == 422
    other = create_user(engine, username='Other', email='other@example.test')
    other_token = login(client, username='Other').json()['access_token']
    assert other != user
    assert client.get(path, headers=_headers(other_token), params=params).status_code == 409
    # Both accounts have independently valid, equally sized histories; a
    # matching cursor cannot be mistaken for authority over the other account.
    other_device = _device(client, other_token)
    _enable(client, other_token)
    other_events = [_event(entity_id='other-1'), _event(entity_id='other-2')]
    for item in other_events:
        assert client.post('/api/v1/sync/encrypted/push', headers=_headers(other_token),
            json=_encrypted_request(other_device, (item, _object(ciphertext=b'other-account-ciphertext')))).status_code == 200
    assert client.post('/api/v2/sync/encrypted/cutover', headers=_headers(other_token), json={'expected_cutover_epoch': 0}).status_code == 200
    assert client.post('/api/v3/sync/encrypted/reader-ready', headers=_headers(other_token),
        json={'device_id': other_device, 'reader_transport_version': 3}).status_code == 204
    assert client.post('/api/v3/sync/encrypted/cutover', headers=_headers(other_token), json={'expected_cutover_epoch': 1}).status_code == 200
    other_page = client.get(path, headers=_headers(other_token), params=params)
    assert other_page.status_code == 200
    assert other_page.json()['items'][0]['event']['event_id'] == other_events[0]['event_id']
    assert other_page.json()['items'][0]['event']['event_id'] != page['items'][0]['event']['event_id']
    assert other_page.json()['items'][0]['object'] != page['items'][0]['object']
    assert client.get(path, headers=headers, params={**params, 'account_id': str(other)}).status_code == 422
    with Session(engine) as db:
        after = (db.get(SyncUserState, user).current_sequence,
                 db.scalar(select(SyncDevice.last_ack_sequence).where(SyncDevice.user_id == user)),
                 db.scalar(select(EncryptedObject.ciphertext).where(EncryptedObject.user_id == user)))
    assert after == before
    assert client.post('/api/v1/auth/logout', headers=headers).status_code == 204
    assert client.get(path, headers=headers, params=params).status_code == 401


def test_recovery_http_network_natural_pagination(cloud_client):
    """Actual HTTP crosses the production 100-item page boundary."""
    client, engine = cloud_client
    create_user(engine)
    token = login(client).json()['access_token']
    headers = _headers(token)
    device = _device(client, token)
    _enable(client, token)
    events = [_event(entity_id=f'page-note-{i}') for i in range(102)]
    for start in (0, 100):
        pairs = [(item, _object(ciphertext=f'encrypted-page-{i:03}'.encode()))
                 for i, item in enumerate(events[start:start + 100], start)]
        response = client.post('/api/v1/sync/encrypted/push', headers=headers, json=_encrypted_request(device, *pairs))
        assert response.status_code == 200, response.text
    assert client.post('/api/v2/sync/encrypted/cutover', headers=headers, json={'expected_cutover_epoch': 0}).status_code == 200
    assert client.post('/api/v3/sync/encrypted/reader-ready', headers=headers,
        json={'device_id': device, 'reader_transport_version': 3}).status_code == 204
    assert client.post('/api/v3/sync/encrypted/cutover', headers=headers, json={'expected_cutover_epoch': 1}).status_code == 200
    api_socket = socket.socket(); api_socket.bind(('127.0.0.1', 0))
    port = api_socket.getsockname()[1]
    server = uvicorn.Server(uvicorn.Config(client.app, lifespan='off', access_log=False, log_level='warning'))
    thread = threading.Thread(target=server.run, kwargs={'sockets': [api_socket]}, daemon=True); thread.start()
    try:
        script = '''import {chromium} from '@playwright/test';
const b=await chromium.launch({headless:true});try{const p=await b.newPage();await p.goto(process.env.RECOVERY_URL+'/docs');
const result=await p.evaluate(async token=>{const get=async since=>{const r=await fetch(`/api/v3/recovery/encrypted/history?since=${since}&limit=100&expected_cursor=102&expected_epoch=2`,{headers:{Authorization:`Bearer ${token}`}});if(!r.ok)throw Error(`status:${r.status}`);return r.json()};return [await get(0),await get(100),await get(0),await get(102)]},process.env.RECOVERY_TOKEN);console.log(JSON.stringify(result));}finally{await b.close()}'''
        import os
        result = subprocess.run(['node', '--input-type=module', '-e', script], cwd=ROOT / 'frontend',
            env={**os.environ, 'RECOVERY_URL': f'http://127.0.0.1:{port}', 'RECOVERY_TOKEN': token},
            capture_output=True, text=True, timeout=90)
        assert result.returncode == 0, result.stderr
        first, second, duplicate, tail = json.loads(result.stdout.strip().splitlines()[-1])
        assert first == duplicate and len(first['items']) == 100 and first['next_cursor'] == 100 and first['has_more']
        assert len(second['items']) == 2 and second['next_cursor'] == 102 and not second['has_more']
        assert tail['items'] == [] and tail['next_cursor'] == 102 and not tail['has_more']
        rows = first['items'] + second['items']
        assert [row['event']['server_sequence'] for row in rows] == list(range(1, 103))
        assert [row['event']['event_id'] for row in rows] == [event['event_id'] for event in events]
        assert [row['object']['ciphertext'] for row in rows] == [_object(ciphertext=f'encrypted-page-{i:03}'.encode())['ciphertext'] for i in range(102)]
    finally:
        server.should_exit = True; thread.join(timeout=15); api_socket.close()
