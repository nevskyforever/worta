"""Native Progress owner → production HTTP → authenticated Vue reader."""
import json
import os
import socket
import sqlite3
import subprocess
import threading
from queue import Queue

import uvicorn
import pytest
from sqlalchemy import select
from sqlalchemy.orm import Session

from test_cloud_auth import ROOT, cloud_client, create_user, login, migrated_database
from test_cloud_c15_headless_cross_runtime import PROJECT_ID, _headers
from test_cloud_c18_authority_cross_runtime import provision, bootstrap, native, publish, sync
from test_cloud_c18_structural_cross_runtime import structural
from test_cloud_c18_structural_integration import publish_pending, pull_structure
from nfprogress.core.sqlite.connection import register_remote_apply_authorization_guard



from test_cloud_c18_progress_acceptance import progress, support as progress_support, emit as emit_progress, receive as receive_progress

from test_cloud_c18_game_acceptance import game, command, receive_mixed, projections
from test_cloud_c18_game_gate import declaration
from test_cloud_c15_headless_cross_runtime import _crypto_bridge
from base64 import urlsafe_b64encode

def test_native_project_game_http_read_update_restart(cloud_client, tmp_path):
    client, engine = cloud_client
    user = str(create_user(engine))
    token = login(client).json()['access_token']
    headers = _headers(token)
    db, identity = provision(tmp_path, 'game-http-native', user, True)
    with sqlite3.connect(db) as connection:
        register_remote_apply_authorization_guard(connection)
        connection.execute("UPDATE projects SET status='активен',goal=100,infinite=0,payload_json=json_set(payload_json,'$.work_method','manual','$.total',0,'$.progress_entries',json('[]'))")
    device = identity['device_id']
    assert client.put(f'/api/v1/sync/devices/{device}', headers=headers).status_code == 200
    boot = bootstrap(tmp_path, db, identity, 'bootstrap_prepare')['bootstrap_id']
    assert client.post(f'/api/v1/cloud/projects/{PROJECT_ID}/bootstrap', headers=headers,
                       json={'bootstrap_id': boot, 'device_id': device}).status_code == 200
    bootstrap(tmp_path, db, identity, 'bootstrap_prepare_capture', bootstrap_id=boot, remote_high_water=0)
    assert client.post(f'/api/v1/cloud/projects/{PROJECT_ID}/bootstrap/complete', headers=headers,
        json={'bootstrap_id': boot, 'device_id': device, 'initial_event_count': 0, 'initial_max_server_sequence': 0}).status_code == 200
    bootstrap(tmp_path, db, identity, 'bootstrap_confirm_active', bootstrap_id=boot, remote_high_water=0)
    assert client.post('/api/v2/sync/encrypted/cutover', headers=headers, json={'expected_cutover_epoch': 0}).status_code == 200
    assert client.post('/api/v3/sync/encrypted/reader-ready', headers=headers,
                       json={'device_id': device, 'reader_transport_version': 3}).status_code == 204
    assert client.post('/api/v3/sync/encrypted/cutover', headers=headers, json={'expected_cutover_epoch': 1}).status_code == 200
    key = list(os.urandom(32))
    native(tmp_path, db, identity, user, 'begin')
    publish(client, tmp_path, token, db, identity, user, key)
    assert sync(client, tmp_path, token, db, identity, user, key)['view']['state'] == 'active'
    # Qualified synthetic Native Desktop only. Browser PUT remains forbidden.
    progress_support(client, headers, device)
    source = {'gamer': json.loads((ROOT/'frontend/src/cloud/__fixtures__/gameLegacyDefaultsV1.json').read_text()),
              'project_game_state': {f'project:{PROJECT_ID}': {'streaks':['2026-10-01','2026-10-02'], 'max_streak':4, 'streak_status':True}}}
    assert game(tmp_path, db, identity, user, 'init', source=source) is True
    assert client.put('/api/v3/sync/encrypted/game-reader-capabilities', headers=headers, json=declaration(device)).status_code == 204
    progress(tmp_path, db, identity, user, 'begin')
    emit_progress(client, tmp_path, token, db, identity, user, key)
    assert receive_progress(client, tmp_path, token, db, identity, user, key) == ['applied']
    def publish_game():
        for item in command(tmp_path, db, identity, user, 'pending', sealed=False):
            sealed = _crypto_bridge(tmp_path, {'action':'game_seal','scope':item['event']['header']['scope'],'frame':item['frame'],'amk':key})
            command(tmp_path, db, identity, user, 'seal', event_id=item['event']['header']['event_id'], frame=item['frame'], nonce=sealed['nonce'], ciphertext=sealed['ciphertext'])
        # A supported dependency-first publication order. Sequence is assigned
        # by the real server, never fabricated or rewritten in the fixture.
        rows = sorted(command(tmp_path, db, identity, user, 'pending', sealed=True), key=lambda item:item['event']['header']['scope']=='account')
        for item in rows:
            h=item['event']['header'];account=h['scope']=='account'
            event={k:h[k] for k in ('event_id','entity_id','revision','updated_at')};event['deleted_at']=None
            event.update({'canonical_user_id':user,'scope':'account','entity_type':'account_game','operation':'upsert'} if account else {'project_id':h['project_id'],'entity_type':'project_game','operation':'event'})
            obj={'crypto_version':2 if account else 1,'aad_version':2 if account else 1,
                 'nonce':urlsafe_b64encode(bytes(item['nonce'])).decode().rstrip('='),'ciphertext':urlsafe_b64encode(bytes(item['ciphertext'])).decode().rstrip('=')}
            body={'protocol_version':3,'encrypted_sync_version':3,'device_id':device,'items':[{'event':event,'object':obj}]}
            endpoint='/api/v3/sync/encrypted/account/push' if account else '/api/v3/sync/encrypted/push'
            response=client.post(endpoint,headers=headers,json=body);assert response.status_code==200,response.text
            duplicate=client.post(endpoint,headers=headers,json=body);assert duplicate.status_code==200 and duplicate.json()['results'][0]['duplicate']
            receipt=response.json()['results'][0]
            command(tmp_path,db,identity,user,'receipt',event_id=h['event_id'],server_sequence=receipt['server_sequence'],duplicate=False,now=h['updated_at'])
        outcomes=receive_mixed(client,tmp_path,token,db,identity,user,key)
        assert all(value=='applied' for value in outcomes),outcomes
        return rows
    command(tmp_path,db,identity,user,'begin',now='2026-11-06T00:00:00.000000Z')
    assert len(publish_game())==2
    owner=progress(tmp_path,db,identity,user,'view')['owners'][0]
    result=progress(tmp_path,db,identity,user,'manual',total=100,expected=owner['tips'])
    assert 'entry' in result,result
    assert game(tmp_path,db,identity,user,'process')['processed']==1
    emit_progress(client,tmp_path,token,db,identity,user,key)
    assert len(publish_game())==2
    def history():
        with sqlite3.connect(db) as connection:
            cursor=connection.execute('SELECT pull_cursor FROM cloud_sync_state').fetchone()[0]
        response=client.get('/api/v3/recovery/encrypted/history',headers=headers,params={'since':0,'limit':100,'expected_cursor':cursor,'expected_epoch':2})
        assert response.status_code==200,response.text
        return response.json()['items']
    def expected():
        with sqlite3.connect(db) as connection:
            chain=json.loads(connection.execute('SELECT snapshot_json FROM cloud_progress_projection').fetchone()[0])
            rows=connection.execute("SELECT canonical_frame FROM cloud_game_events WHERE scope='project' ORDER BY server_sequence").fetchall()
        return {'projections':dict(projections(db)), 'projectEvents':[json.loads(bytes(row[0])[20:]) for row in rows], 'progress':chain}
    items=history();assert len(items)==7
    fixture=tmp_path/'game-http.json';fixture.write_text(json.dumps({'key':key,'project':PROJECT_ID,'expected':expected(),'items':items}));fixture.chmod(0o600)
    api_socket=socket.socket();api_socket.bind(('127.0.0.1',0));port=api_socket.getsockname()[1]
    server=uvicorn.Server(uvicorn.Config(client.app,lifespan='off',access_log=False,log_level='warning'))
    thread=threading.Thread(target=server.run,kwargs={'sockets':[api_socket]},daemon=True);thread.start()
    try:
        process=subprocess.Popen(['node','scripts/browser-game-http.mjs',str(fixture),f'http://127.0.0.1:{port}',str(tmp_path/'browser-profile')],cwd=ROOT/'frontend',stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
        try:
            ready=Queue();threading.Thread(target=lambda:ready.put(process.stdout.readline()),daemon=True).start()
            line=ready.get(timeout=120)
            if not line:
                stdout,stderr=process.communicate(timeout=10);raise AssertionError(stdout+stderr)
            assert json.loads(line)['phase']=='ready_for_update',line
            completed=game(tmp_path,db,identity,user,'complete');assert 'error' not in completed,completed
            assert game(tmp_path,db,identity,user,'process')['processed']==1
            publish(client,tmp_path,token,db,identity,user,key)
            assert len(publish_game())==2
            latest=history();assert latest[:len(items)]==items
            stdout,stderr=process.communicate(json.dumps({'items':latest,'expected':expected()})+'\n',timeout=180)
            assert process.returncode==0,stdout+stderr
            assert json.loads(stdout.strip().splitlines()[-1])['pass']
        finally:
            if process.poll() is None:process.kill();process.communicate()
        from backend.app.cloud.models import SyncDevice
        with Session(engine) as session:
            devices=session.scalars(select(SyncDevice).where(SyncDevice.user_id==user)).all()
            assert [str(value.device_id) for value in devices]==[device]
            assert devices[0].last_ack_sequence==0
    finally:
        server.should_exit=True;thread.join(timeout=10);api_socket.close()
