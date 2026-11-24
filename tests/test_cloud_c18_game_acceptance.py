"""Mandatory Game acceptance: real PostgreSQL, production TS crypto, native files.

No substitute projection or test-only Game action generator is used. The bridge
calls the ordinary native Progress/completion/inventory writers and Game command.
"""
import json
import os
import sqlite3
from collections import Counter
from datetime import datetime, timezone
from uuid import uuid4

from sqlalchemy import text
from nfprogress.core.sqlite.connection import register_remote_apply_authorization_guard
from test_cloud_auth import cloud_client, migrated_database, create_user, login
from test_cloud_c18_authority_cross_runtime import provision, bootstrap, native, publish, sync
from test_cloud_c18_progress_acceptance import progress, support, emit as emit_progress, receive as receive_progress
from test_cloud_c18_game_gate import declaration
from test_cloud_c15_headless_cross_runtime import PROJECT_ID, ROOT, _crypto_bridge, _native_bridge, _headers

NOW = '2026-10-04T00:00:00.000000Z'


def game(tmp, path, identity, user, step='command', **values):
    return _native_bridge(tmp, dict(action='game', database_path=str(path),
        local_account_id=identity['local_account_id'], device_id=identity['device_id'],
        canonical_user_id=user, project_id=PROJECT_ID, step=step, **values))


def command(tmp, path, identity, user, action, **values):
    value = game(tmp, path, identity, user, request=dict(action=action, **values))
    assert not isinstance(value, dict) or 'error' not in value, value
    return value


def projections(path):
    with sqlite3.connect(path) as db:
        return [(key, json.loads(raw)) for key, raw in db.execute(
            'SELECT owner_key,snapshot_json FROM cloud_game_projection ORDER BY owner_key')]


def emit_game(client, tmp, token, path, identity, user, amk, lose=False, expected_status=200):
    for item in command(tmp, path, identity, user, 'pending', sealed=False):
        sealed = _crypto_bridge(tmp, dict(action='game_seal', scope=item['event']['header']['scope'],
            frame=item['frame'], amk=amk))
        command(tmp, path, identity, user, 'seal', event_id=item['event']['header']['event_id'],
            frame=item['frame'], nonce=sealed['nonce'], ciphertext=sealed['ciphertext'])
    sent = []
    for item in command(tmp, path, identity, user, 'pending', sealed=True):
        h = item['event']['header']
        from base64 import urlsafe_b64encode
        wire = {k: h[k] for k in ('event_id', 'entity_id', 'revision', 'updated_at')}
        wire['deleted_at'] = None
        account = h['scope'] == 'account'
        if account:
            wire.update(canonical_user_id=user, scope='account', entity_type='account_game', operation='upsert')
        else:
            wire.update(project_id=h['project_id'], entity_type='project_game', operation='event')
        obj = dict(crypto_version=2 if account else 1, aad_version=2 if account else 1,
            nonce=urlsafe_b64encode(bytes(item['nonce'])).decode().rstrip('='),
            ciphertext=urlsafe_b64encode(bytes(item['ciphertext'])).decode().rstrip('='))
        body = dict(protocol_version=3, encrypted_sync_version=3, device_id=identity['device_id'],
            items=[dict(event=wire, object=obj)])
        endpoint = '/api/v3/sync/encrypted/account/push' if account else '/api/v3/sync/encrypted/push'
        response = client.post(endpoint, headers=_headers(token), json=body)
        assert response.status_code == expected_status, response.text
        if expected_status != 200:
            sent.append((endpoint, body))
            continue
        repeated = client.post(endpoint, headers=_headers(token), json=body)
        assert repeated.status_code == 200, repeated.text
        receipt = repeated.json()['results'][0]
        assert receipt['duplicate'] and receipt['server_sequence'] == response.json()['results'][0]['server_sequence']
        if not lose:
            command(tmp, path, identity, user, 'receipt', event_id=h['event_id'],
                server_sequence=receipt['server_sequence'], duplicate=True, now=NOW)
        else:
            frozen = next(v for v in command(tmp, path, identity, user, 'pending', sealed=True)
                if v['event']['header']['event_id'] == h['event_id'])
            assert frozen == item
        sent.append((endpoint, body))
    return sent


def receive_mixed(client, tmp, token, path, identity, user, amk, game_first=False):
    with sqlite3.connect(path) as db:
        cursor = db.execute('SELECT pull_cursor FROM cloud_sync_state').fetchone()[0]
    response = client.get('/api/v3/sync/encrypted/pull', headers=_headers(token), params=dict(
        device_id=identity['device_id'], since=cursor, limit=100, protocol_version=3, encrypted_sync_version=3))
    assert response.status_code == 200, response.text
    page = response.json()
    rows = []
    for item in page['items']:
        row = dict(item['event']);row['source_device_id'] = row.pop('device_id');row['envelope'] = item['object']
        row['updated_at'] = datetime.fromisoformat(row['updated_at'].replace('Z', '+00:00')).astimezone(timezone.utc).isoformat(timespec='microseconds').replace('+00:00', 'Z')
        if row.get('deleted_at') is not None:
            row['deleted_at']=datetime.fromisoformat(row['deleted_at'].replace('Z','+00:00')).astimezone(timezone.utc).isoformat(timespec='microseconds').replace('+00:00','Z')
        rows.append(row)
    assert game(tmp, path, identity, user, 'receive', command=dict(account_id=identity['local_account_id'],
        device_id=identity['device_id'], canonical_user_id=user, expected_cursor=cursor,
        next_cursor=page['next_cursor'], has_more=page['has_more'], items=rows)) is True
    outcomes = []
    def read_games():
        for row in command(tmp, path, identity, user, 'received', limit=32):
            from base64 import urlsafe_b64encode
            account = row['scope'] == 'account'
            descriptor = dict(event_id=row['event_id'], device_id=row['source_device_id'],
                entity_id=row['entity_id'], revision=row['revision'], updated_at=row['updated_at'],
                deleted_at=None, server_sequence=row['server_sequence'])
            if account:
                descriptor.update(scope='account', canonical_user_id=user, entity_type='account_game', operation='upsert')
            else:
                descriptor.update(project_id=row['project_id'], entity_type='project_game', operation='event')
            item = dict(event=descriptor, object=dict(crypto_version=2 if account else 1,
                aad_version=2 if account else 1,
                nonce=urlsafe_b64encode(bytes(row['nonce'])).decode().rstrip('='),
                ciphertext=urlsafe_b64encode(bytes(row['ciphertext'])).decode().rstrip('=')))
            opened = _crypto_bridge(tmp, dict(action='game_open', scope=row['scope'], canonical_user_id=user, amk=amk, item=item))
            outcome = command(tmp, path, identity, user, 'apply', project=row['scope']=='project',
                frame=opened['frame'], nonce=opened['nonce'], ciphertext=opened['ciphertext'])
            outcomes.append(outcome)
    if game_first:
        read_games()
    for item in page['items']:
        if item['event']['entity_type']=='note':
            opened=_crypto_bridge(tmp,dict(action='open',canonical_user_id=user,amk=amk,item=item))
            assert game(tmp,path,identity,user,'note_apply',event=item['event'],opened=opened)=='applied'
            if game_first:
                unresolved=[row['server_sequence'] for row in command(tmp,path,identity,user,'received',limit=32)]
                assert unresolved
                assert progress(tmp,path,identity,user,'ack')['candidate_cursor'] < min(unresolved)
    for item in page['items']:
        kind = item['event']['entity_type']
        if kind == 'progress':
            opened = _crypto_bridge(tmp, dict(action='progress_open', canonical_user_id=user, amk=amk, item=item))
            assert progress(tmp, path, identity, user, 'apply', **{k: opened[k] for k in ('frame','nonce','ciphertext')}) == 'applied'
        elif kind == 'project_metadata':
            opened = _crypto_bridge(tmp, dict(action='metadata_open', canonical_user_id=user, amk=amk, item=item))
            native(tmp, path, identity, user, 'apply', opened=opened)
        elif kind in ('stage', 'stage_order'):
            from test_cloud_c18_structural_cross_runtime import structural
            opened = _crypto_bridge(tmp, dict(action='structural_open', canonical_user_id=user, amk=amk, item=item))
            structural(tmp,path,identity,user,'apply',event_id=item['event']['event_id'],opened=opened)
    # Reverse transport order is common: account R precedes G. Rotate and retry
    # with exact retained bytes; successful proofs disappear from the reader list.
    for _ in range(3):
        read_games()
    return outcomes


def test_game_real_postgresql_two_devices_reward_once_and_bidirectional(cloud_client, tmp_path):
    client, engine = cloud_client
    user = str(create_user(engine));token = login(client).json()['access_token'];headers = _headers(token)
    a, ia = provision(tmp_path, 'game-A', user, True);b, ib = provision(tmp_path, 'game-B', user, False)
    for identity in (ia, ib):
        assert client.put(f"/api/v1/sync/devices/{identity['device_id']}", headers=headers).status_code == 200
    with sqlite3.connect(a) as db:
        register_remote_apply_authorization_guard(db)
        db.execute("UPDATE projects SET goal=1000,infinite=0,payload_json=json_set(payload_json,'$.total',100,'$.progress_entries',json('[]'))")
    boot = bootstrap(tmp_path, a, ia, 'bootstrap_prepare')['bootstrap_id']
    assert client.post(f'/api/v1/cloud/projects/{PROJECT_ID}/bootstrap', headers=headers,
        json=dict(bootstrap_id=boot, device_id=ia['device_id'])).status_code == 200
    bootstrap(tmp_path, a, ia, 'bootstrap_prepare_capture', bootstrap_id=boot, remote_high_water=0)
    assert client.post(f'/api/v1/cloud/projects/{PROJECT_ID}/bootstrap/complete', headers=headers,
        json=dict(bootstrap_id=boot, device_id=ia['device_id'], initial_event_count=0, initial_max_server_sequence=0)).status_code == 200
    bootstrap(tmp_path, a, ia, 'bootstrap_confirm_active', bootstrap_id=boot, remote_high_water=0)
    assert client.post('/api/v2/sync/encrypted/cutover', headers=headers, json=dict(expected_cutover_epoch=0)).status_code == 200
    for identity in (ia, ib):
        assert client.post('/api/v3/sync/encrypted/reader-ready', headers=headers,
            json=dict(device_id=identity['device_id'], reader_transport_version=3)).status_code == 204
    assert client.post('/api/v3/sync/encrypted/cutover', headers=headers, json=dict(expected_cutover_epoch=1)).status_code == 200
    amk = list(os.urandom(32));native(tmp_path, a, ia, user, 'begin')
    meta, _ = publish(client, tmp_path, token, a, ia, user, amk)
    assert sync(client, tmp_path, token, a, ia, user, amk)['view']['state'] == 'active'
    bootstrap(tmp_path, b, ib, 'bootstrap_import', bootstrap_id=boot, remote_high_water=1,
        display_name=meta['metadata']['name'], authenticated_metadata=meta['metadata'])
    assert sync(client, tmp_path, token, b, ib, user, amk)['view']['state'] == 'active'
    for path, identity in ((a, ia), (b, ib)):
        support(client, headers, identity['device_id'])
        assert game(tmp_path, path, identity, user, 'init') is True
        assert client.put('/api/v3/sync/encrypted/game-reader-capabilities', headers=headers,
            json=declaration(identity['device_id'])).status_code == 204
    with sqlite3.connect(b) as db:
        register_remote_apply_authorization_guard(db)
        db.execute("UPDATE projects SET payload_json=json_set(payload_json,'$.total',0,'$.progress_entries',json('[]'))")
    progress(tmp_path, a, ia, user, 'begin');emit_progress(client, tmp_path, token, a, ia, user, amk)
    for path, identity in ((a, ia), (b, ib)):
        assert receive_progress(client, tmp_path, token, path, identity, user, amk) == ['applied']
    source = dict(gamer=json.loads((ROOT/'frontend/src/cloud/__fixtures__/gameLegacyDefaultsV1.json').read_text()))
    source['gamer'].update(coins=80, exp=1000, complete_bonus_projects=['project:old-local'])
    source['gamer']['items'].setdefault('Зелья', {})['Микро зелье здоровья'] = 2
    source['project_game_state'] = {f'project:{PROJECT_ID}':dict(streaks=['2026-10-01','2026-10-02'], max_streak=2, streak_status=True)}
    source['global_streak'] = dict(global_streaks=['2026-10-02'], max_global_streak=1, global_streak_status=True)
    assert game(tmp_path, a, ia, user, 'init', source=source) is True
    assert command(tmp_path, a, ia, user, 'pending', sealed=False) == []
    before = game(tmp_path, a, ia, user, 'state')['gamer']['coins']
    command(tmp_path, a, ia, user, 'begin', now=NOW)
    requests = emit_game(client, tmp_path, token, a, ia, user, amk, lose=True)
    assert len(requests) == 2
    for path, identity in ((a, ia), (b, ib)):
        receive_mixed(client, tmp_path, token, path, identity, user, amk)
        assert all(o['state']=='active' for o in command(tmp_path,path,identity,user,'view')['owners'])
    assert projections(a) == projections(b)
    assert game(tmp_path, a, ia, user, 'state')['gamer']['coins'] == before
    for index, (path, identity, total) in enumerate(((a, ia, 120), (b, ib, 130), (a, ia, 1900)), 1):
        heads = progress(tmp_path, path, identity, user, 'view')['owners'][0]['tips']
        result = progress(tmp_path, path, identity, user, 'manual', expected=heads, total=total)
        assert 'entry' in result, result
        if index == 3:
            # The Python HTTP compatibility consumer uses the same durable pair,
            # but retains its historical raw-XP rule rather than native leveling.
            from nfprogress.core.game_state import GameEventConsumer
            assert GameEventConsumer(path.parent).process_pending() == dict(processed=1, failed=0)
            assert GameEventConsumer(path.parent).process_pending() == dict(processed=0, failed=0)
            state = game(tmp_path,path,identity,user,'state')['gamer']
            assert state['exp'] == 10000 and state['level'] == 1
        else:
            assert game(tmp_path, path, identity, user, 'process')['processed'] == 1
        with sqlite3.connect(path) as db:
            assert db.execute('SELECT count(*) FROM cloud_game_rewards').fetchone()[0] == index
        emit_progress(client, tmp_path, token, path, identity, user, amk)
        requests += emit_game(client, tmp_path, token, path, identity, user, amk, lose=True)
        if index==1:
            # An independently processed legacy Note is beyond the Progress/G/R
            # dependency hole in the same account-wide sequence.
            from test_cloud_c18_content_note_acceptance import note
            payload=note('game-ack-note',fmt='html');payload.pop('revision')
            payload['updated_at']=NOW
            sealed=_crypto_bridge(tmp_path,dict(action='seal',canonical_user_id=user,
                device_id=identity['device_id'],amk=amk,note=payload,
                event=dict(event_id=str(uuid4()),project_id=PROJECT_ID,entity_id=payload['id'],
                    entity_type='note',operation='upsert',revision=1,updated_at=NOW,deleted_at=None)))
            body=sealed['push'];body.update(protocol_version=3,encrypted_sync_version=3)
            response=client.post('/api/v3/sync/encrypted/push',headers=headers,json=body)
            assert response.status_code==200,response.text
        for target, device in ((a, ia), (b, ib)):
            receive_mixed(client, tmp_path, token, target, device, user, amk, game_first=True)
            assert game(tmp_path, target, device, user, 'process')['processed'] == 0
        assert projections(a) == projections(b)
    for endpoint, body in requests:
        replay = client.post(endpoint, headers=headers, json=body)
        assert replay.status_code == 200 and replay.json()['results'][0]['duplicate']
    for path, identity in ((a, ia), (b, ib)):
        receive_mixed(client, tmp_path, token, path, identity, user, amk)
        with sqlite3.connect(path) as db:
            assert db.execute('SELECT count(*) FROM cloud_game_rewards').fetchone()[0] == 3
            assert db.execute('SELECT count(*) FROM cloud_game_local_mutations').fetchone()[0] == 0
        assert progress(tmp_path, path, identity, user, 'ack')['candidate_cursor'] > 0
    # Registering a Progress-only third reader pauses BOTH frozen Game halves.
    c = str(uuid4())
    assert client.put(f'/api/v1/sync/devices/{c}', headers=headers).status_code == 200
    support(client, headers, c)
    assert client.post('/api/v3/sync/encrypted/reader-ready', headers=headers,
        json=dict(device_id=c, reader_transport_version=3)).status_code == 204
    completed = game(tmp_path, a, ia, user, 'complete')
    assert 'error' not in completed, completed
    assert game(tmp_path, a, ia, user, 'process')['processed'] == 1
    publish(client, tmp_path, token, a, ia, user, amk)
    frozen = emit_game(client,tmp_path,token,a,ia,user,amk,expected_status=409)
    assert len(frozen) == 2
    with engine.connect() as db:
        # The overdue one-day global series now emits its own loss event.
        assert db.execute(text("SELECT count(*) FROM sync_events WHERE entity_type IN ('project_game','account_game')")).scalar() == 9
    assert client.get('/api/v3/sync/encrypted/progress-reader-capabilities', headers=headers).json()['ready']
    assert client.put('/api/v3/sync/encrypted/game-reader-capabilities', headers=headers,
        json=declaration(c)).status_code == 204
    # Ordinary completion resumes the exact pending pair after C upgrades.
    assert emit_game(client, tmp_path, token, a, ia, user, amk, lose=True) == frozen
    for path, identity in ((a, ia), (b, ib)):
        receive_mixed(client, tmp_path, token, path, identity, user, amk, game_first=True)
    assert projections(a) == projections(b)
    with sqlite3.connect(a) as db:
        assert db.execute('SELECT count(*) FROM cloud_game_rewards').fetchone()[0] == 4

    # Two ordinary purchases from the same causal balance preserve both branches.
    for path, identity in ((a, ia), (b, ib)):
        game(tmp_path, path, identity, user, 'inventory', category='Зелья',
            item_id='Микро зелье здоровья', count=1, operation='buy')
        emit_game(client, tmp_path, token, path, identity, user, amk)
    for path, identity in ((a, ia), (b, ib)):
        receive_mixed(client, tmp_path, token, path, identity, user, amk)
        owner = next(o for o in command(tmp_path,path,identity,user,'view')['owners'] if o['owner_key']=='account')
        assert owner['state'] == 'conflict' and len(owner['tips']) == 2
    owner = next(o for o in command(tmp_path,a,ia,user,'view')['owners'] if o['owner_key']=='account')
    stale = game(tmp_path,a,ia,user,request=dict(action='decide',owner_key='account',
        expected_tips=owner['tips'][:1],expected_local=owner['local'],
        selected_event_id=owner['tips'][0],now=NOW))
    assert stale['error'] == 'game_noncommutative_conflict'
    command(tmp_path,a,ia,user,'decide',owner_key='account',expected_tips=owner['tips'],
        expected_local=owner['local'],selected_event_id=owner['tips'][0],now=NOW)
    emit_game(client,tmp_path,token,a,ia,user,amk)
    for path, identity in ((a, ia), (b, ib)):
        receive_mixed(client,tmp_path,token,path,identity,user,amk)
    assert projections(a) == projections(b)
    owner = next(o for o in command(tmp_path,a,ia,user,'view')['owners'] if o['owner_key']=='account')
    assert owner['state'] == 'active'
    with sqlite3.connect(a) as db:
        target = db.execute('SELECT event_id FROM cloud_game_rewards ORDER BY rowid LIMIT 1').fetchone()[0]
    kwargs = dict(target_action_id=target,expected_tips=owner['tips'],expected_local=owner['local'],now=NOW)
    reversal = command(tmp_path,a,ia,user,'compensate',**kwargs)
    assert command(tmp_path,a,ia,user,'compensate',**kwargs) == reversal
    emit_game(client,tmp_path,token,a,ia,user,amk,lose=True)
    for path, identity in ((a, ia), (b, ib)):
        receive_mixed(client,tmp_path,token,path,identity,user,amk)
        with sqlite3.connect(path) as db:
            assert db.execute('SELECT count(*) FROM cloud_game_compensations').fetchone()[0] == 1
            assert db.execute('SELECT count(*) FROM cloud_game_rewards').fetchone()[0] == 4
    assert projections(a) == projections(b)
    # Add a Stage after account/root Game are already active. Explicit migration
    # publishes only its missing genesis and never regenerates either old base.
    from test_cloud_c18_structural_cross_runtime import event, structural, structural_publish
    from test_cloud_c18_structural_integration import publish_pending
    with sqlite3.connect(a) as db:
        proposed = json.loads(db.execute('SELECT payload_json FROM projects').fetchone()[0])
    proposed['stages_enabled'] = True
    native(tmp_path,a,ia,user,'normal_edit',proposed=proposed)
    metadata,_ = publish(client,tmp_path,token,a,ia,user,amk)
    for path,identity in ((a,ia),(b,ib)):
        receive_mixed(client,tmp_path,token,path,identity,user,amk)
    stage_event = event(ia,user,boot,metadata['header']['event_id'],entity='S1')
    stage = stage_event['stage'];stage.update(goal=1000,infinite=False,unit='symbols',work_method='manual')
    with sqlite3.connect(a) as db:
        register_remote_apply_authorization_guard(db)
        db.execute('INSERT INTO stages(id,project_id,name,goal,infinite,unit,status,created_at,payload_json) VALUES(?,?,?,?,?,?,?,?,?)',
            ('S1',PROJECT_ID,stage['name'],1000,0,'symbols',stage['status'],stage['created_at'],json.dumps({**stage,'total':50,'progress_entries':[]})))
        db.execute('INSERT INTO stage_order VALUES(?,?,0)',('S1',PROJECT_ID))
    structural(tmp_path,a,ia,user,'capture',stage_id='S1')
    structural_publish(client,tmp_path,token,a,ia,user,amk,stage_event)
    for path,identity in ((a,ia),(b,ib)):
        receive_mixed(client,tmp_path,token,path,identity,user,amk)
    order = event(ia,user,boot,metadata['header']['event_id'],order=['S1'],
        heads={'S1':[stage_event['header']['event_id']]})
    structural_publish(client,tmp_path,token,a,ia,user,amk,order)
    for path,identity in ((a,ia),(b,ib)):
        receive_mixed(client,tmp_path,token,path,identity,user,amk)
    with sqlite3.connect(b) as db:
        register_remote_apply_authorization_guard(db)
        db.execute("UPDATE stages SET payload_json=json_set(payload_json,'$.total',0,'$.progress_entries',json('[]')) WHERE id='S1'")
    progress(tmp_path,a,ia,user,'begin')
    emit_progress(client,tmp_path,token,a,ia,user,amk)
    for path,identity in ((a,ia),(b,ib)):
        receive_mixed(client,tmp_path,token,path,identity,user,amk)
    command(tmp_path,a,ia,user,'begin',now=NOW)
    assert len(command(tmp_path,a,ia,user,'pending',sealed=False)) == 1
    emit_game(client,tmp_path,token,a,ia,user,amk)
    for path,identity in ((a,ia),(b,ib)):
        receive_mixed(client,tmp_path,token,path,identity,user,amk)
    for o in command(tmp_path,a,ia,user,'view')['owners']:
        assert o['state']=='active', o
    owner = next(o for o in progress(tmp_path,a,ia,user,'view')['owners'] if o['stage_id']=='S1')
    assert 'entry' in progress(tmp_path,a,ia,user,'manual',stage_id='S1',expected=owner['tips'],total=1000)
    assert game(tmp_path,a,ia,user,'process')['processed']==1
    emit_progress(client,tmp_path,token,a,ia,user,amk)
    emit_game(client,tmp_path,token,a,ia,user,amk)
    for path,identity in ((a,ia),(b,ib)):
        receive_mixed(client,tmp_path,token,path,identity,user,amk,game_first=True)
        assert all(owner['state']=='active' for owner in command(tmp_path,path,identity,user,'view')['owners'])
    completed = game(tmp_path,a,ia,user,'complete',stage_id='S1')
    assert 'error' not in completed,completed
    assert game(tmp_path,a,ia,user,'process')['processed']==1
    publish_pending(client,tmp_path,token,a,ia,user,amk)
    emit_game(client,tmp_path,token,a,ia,user,amk,lose=True)
    for path,identity in ((a,ia),(b,ib)):
        receive_mixed(client,tmp_path,token,path,identity,user,amk,game_first=True)
        assert game(tmp_path,path,identity,user,'process')['processed']==0
        with sqlite3.connect(path) as db:
            assert db.execute('SELECT count(*) FROM cloud_game_rewards').fetchone()[0]==6
    assert projections(a)==projections(b)
    # Renaming owners cannot reopen their stable completion claim.
    with sqlite3.connect(a) as db:
        proposed=json.loads(db.execute('SELECT payload_json FROM projects').fetchone()[0])
    proposed['name']='Renamed project'
    native(tmp_path,a,ia,user,'normal_edit',proposed=proposed)
    publish(client,tmp_path,token,a,ia,user,amk)
    for path,identity in ((a,ia),(b,ib)):
        receive_mixed(client,tmp_path,token,path,identity,user,amk)
    with sqlite3.connect(a) as db:
        proposed=json.loads(db.execute("SELECT payload_json FROM stages WHERE id='S1'").fetchone()[0])
    proposed['name']='Renamed stage'
    structural(tmp_path,a,ia,user,'edit',stage_id='S1',proposed=proposed)
    publish_pending(client,tmp_path,token,a,ia,user,amk)
    for path,identity in ((a,ia),(b,ib)):
        receive_mixed(client,tmp_path,token,path,identity,user,amk)
        assert command(tmp_path,path,identity,user,'pending',sealed=False)==[]
    before_delete=projections(a)
    owner=next(o for o in progress(tmp_path,a,ia,user,'view')['owners'] if o['stage_id']=='S1')
    selected=owner['tips'][0]
    target=next(v for v in owner['versions'] if v['event_id']==selected)['chain']['entries'][-1]['entry_id']
    deleted=progress(tmp_path,a,ia,user,'decide',decision=dict(project_id=PROJECT_ID,stage_id='S1',
        expected_tips=owner['tips'],expected_local=owner['local'],selected_event_id=selected,
        operation='tombstone',target_entry_id=target,rebased_from=[],corrected_delta=None))
    assert isinstance(deleted,str),deleted
    emit_progress(client,tmp_path,token,a,ia,user,amk)
    for path,identity in ((a,ia),(b,ib)):
        receive_mixed(client,tmp_path,token,path,identity,user,amk)
        assert game(tmp_path,path,identity,user,'process')['processed']==0
        assert projections(path)==before_delete
    # Corrupt only a derived compatibility field. Repair uses authenticated
    # history; explicit reconciliation resumes without discarding recovery.
    expected=game(tmp_path,b,ib,user,'state')['gamer']['coins']
    with sqlite3.connect(b) as db:
        register_remote_apply_authorization_guard(db)
        db.execute("UPDATE game_state SET payload_json=json_set(payload_json,'$.gamer.coins',99999)")
    command(tmp_path,b,ib,user,'rebuild',owner_key='account')
    assert game(tmp_path,b,ib,user,'state')['gamer']['coins']==expected
    command(tmp_path,b,ib,user,'begin',now=NOW)
    assert len(command(tmp_path,b,ib,user,'pending',sealed=False))==3
    emit_game(client,tmp_path,token,b,ib,user,amk)
    for path,identity in ((a,ia),(b,ib)):
        receive_mixed(client,tmp_path,token,path,identity,user,amk)
        with sqlite3.connect(path) as db:
            assert db.execute('SELECT count(*) FROM cloud_game_rewards').fetchone()[0]==6
        assert all(o['state']=='active' for o in command(tmp_path,path,identity,user,'view')['owners'])
    assert projections(a)==projections(b)
    with sqlite3.connect(b) as db:
        assert db.execute('SELECT count(*) FROM cloud_game_local_mutations').fetchone()[0]==1
    # An authenticated Stage tombstone with unresolved retained children must
    # retain history and reject further unsafe Progress/Game production.
    # Reopen through the ordinary structural writer so the earlier completion
    # read-only guard cannot mask the tombstone dependency guard under test.
    with sqlite3.connect(a) as db:
        proposed=json.loads(db.execute("SELECT payload_json FROM stages WHERE id='S1'").fetchone()[0])
    proposed.update(status='активен',completed_at=None)
    structural(tmp_path,a,ia,user,'edit',stage_id='S1',proposed=proposed)
    publish_pending(client,tmp_path,token,a,ia,user,amk)
    for path,identity in ((a,ia),(b,ib)):
        receive_mixed(client,tmp_path,token,path,identity,user,amk)
        assert command(tmp_path,path,identity,user,'pending',sealed=False)==[]
    with sqlite3.connect(a) as db:
        old_stage=json.loads(bytes(db.execute("SELECT e.canonical_frame FROM cloud_sync_structural_tips t JOIN cloud_sync_structural_events e USING(account_id,event_id) WHERE t.entity_type='stage' AND t.entity_id='S1'").fetchone()[0])[20:])
    tombstone=json.loads(json.dumps(old_stage));h=tombstone['header']
    h.update(event_id=str(uuid4()),parent_event_ids=[old_stage['header']['event_id']],
        revision=h['revision']+1,generation=h['generation']+1,operation='delete',updated_at=NOW)
    tombstone.update(stage=None,deleted_at=NOW)
    structural(tmp_path,a,ia,user,'prepare',event=tombstone,expected_tips=h['parent_event_ids'])
    publish_pending(client,tmp_path,token,a,ia,user,amk)
    for path,identity in ((a,ia),(b,ib)):
        receive_mixed(client,tmp_path,token,path,identity,user,amk)
        with sqlite3.connect(path) as db:
            assert db.execute('SELECT state FROM cloud_sync_structural_events WHERE event_id=?',(h['event_id'],)).fetchone()[0]=='tombstone_blocked'
            assert db.execute('SELECT count(*) FROM cloud_game_rewards').fetchone()[0]==6
        owner=next(o for o in progress(tmp_path,path,identity,user,'view')['owners'] if o['stage_id']=='S1')
        rejected=progress(tmp_path,path,identity,user,'manual',stage_id='S1',expected=owner['tips'],total=60)
        assert rejected['error']=='stage_tombstone_child_manifest_incomplete',rejected
        assert command(tmp_path,path,identity,user,'pending',sealed=False)==[]
    with sqlite3.connect(a) as db:
        retained = db.execute("""
            SELECT event_id, json_extract(CAST(substr(canonical_frame,21) AS TEXT),'$.action.kind')
            FROM cloud_game_events
        """).fetchall()
    # Stage writing earns both its writing reward and a separate daily-goal
    # streak event. Neither event may disappear when Stage completion follows.
    assert Counter(kind for _, kind in retained) == {
        'genesis': 3, 'writing': 4, 'reward': 6, 'global_streak': 2, 'completion': 2,
        'inventory': 2, 'resolution': 1, 'compensation': 1, 'adopt_local': 3,
    }
    with engine.connect() as db:
        remote_ids = db.execute(text("SELECT event_id FROM sync_events WHERE entity_type IN ('project_game','account_game')")).scalars()
        assert {str(event_id) for event_id in remote_ids} == {event_id for event_id, _ in retained}
        columns = {r[0] for r in db.execute(text("SELECT column_name FROM information_schema.columns WHERE table_name='sync_events'"))}
        assert not columns.intersection({'coins','experience','inventory','reward_id','game_action','total_symbols'})


def verify_web_shop(client, engine, tmp_path, token, user, amk, native_devices):
    headers = _headers(token)
    browser = dict(accountId=str(uuid4()), deviceId=str(uuid4()), canonicalUserId=user)
    endpoint = '/api/v3/sync/encrypted/account/push'
    gate = client.get('/api/v3/sync/encrypted/game-reader-capabilities', headers=headers,
                      params=dict(web_game_version=1))
    assert gate.json() == dict(ready=True, missing_devices=0, web_game_version=1)
    for operation, expected_coins, expected_count in (('buy', 71, 1), ('sell', 92.8, 0)):
        path, _ = native_devices[0]
        with sqlite3.connect(path) as db:
            raw, snapshot = db.execute("""
                SELECT e.canonical_frame, p.snapshot_json
                FROM cloud_game_projection p JOIN cloud_game_events e
                ON e.account_id=p.account_id AND e.event_id=p.head_event_id
                WHERE p.owner_key='account'
            """).fetchone()
        head, base = json.loads(raw[20:]), json.loads(snapshot)
        with engine.connect() as db:
            cursor = db.execute(text('SELECT max(server_sequence) FROM sync_events')).scalar_one()
        prepared = _crypto_bridge(tmp_path, dict(action='game_shop', amk=amk, scope=browser,
            head=head, base=base, now=NOW, event_id=str(uuid4()), command=dict(
                operation=operation, category='Зелья', item_id='Микро зелье здоровья', count=1)))
        raw_body = prepared['raw_body']
        params = dict(web_game_version=1, expected_cursor=cursor, expected_epoch=2, game_parent=head['header']['event_id'])
        assert client.post(endpoint, headers=headers, content=raw_body).status_code == 409
        rejected = client.post(endpoint, headers=headers, content=raw_body, params={**params, 'expected_cursor': cursor - 1})
        assert rejected.status_code == 409 and rejected.json()['detail']['code'] == 'web_game_snapshot_changed'
        response = client.post(endpoint, headers=headers, content=raw_body, params=params)
        assert response.status_code == 200, response.text
        repeat = client.post(endpoint, headers=headers, content=raw_body, params=params)
        assert repeat.status_code == 200 and repeat.json()['results'][0]['duplicate']
        for path, identity in native_devices:
            receive_mixed(client, tmp_path, token, path, identity, user, amk)
            state = game(tmp_path, path, identity, user, 'state')['gamer']
            assert state['coins'] == expected_coins
            assert state['items'].get('Зелья', {}).get('Микро зелье здоровья', 0) == expected_count
    with engine.connect() as db:
        assert db.execute(text('SELECT count(*) FROM sync_devices')).scalar_one() == len(native_devices)


def test_global_loss_without_progress_and_web_shop_converge_on_two_devices(cloud_client,tmp_path):
    client, engine = cloud_client
    user = str(create_user(engine));token = login(client).json()['access_token'];headers = _headers(token)
    a, ia = provision(tmp_path, 'game-A', user, True);b, ib = provision(tmp_path, 'game-B', user, False)
    for identity in (ia, ib):
        assert client.put(f"/api/v1/sync/devices/{identity['device_id']}", headers=headers).status_code == 200
    with sqlite3.connect(a) as db:
        register_remote_apply_authorization_guard(db)
        db.execute("UPDATE projects SET goal=1000,infinite=0,payload_json=json_set(payload_json,'$.total',100,'$.progress_entries',json('[]'))")
    boot = bootstrap(tmp_path, a, ia, 'bootstrap_prepare')['bootstrap_id']
    assert client.post(f'/api/v1/cloud/projects/{PROJECT_ID}/bootstrap', headers=headers,
        json=dict(bootstrap_id=boot, device_id=ia['device_id'])).status_code == 200
    bootstrap(tmp_path, a, ia, 'bootstrap_prepare_capture', bootstrap_id=boot, remote_high_water=0)
    assert client.post(f'/api/v1/cloud/projects/{PROJECT_ID}/bootstrap/complete', headers=headers,
        json=dict(bootstrap_id=boot, device_id=ia['device_id'], initial_event_count=0, initial_max_server_sequence=0)).status_code == 200
    bootstrap(tmp_path, a, ia, 'bootstrap_confirm_active', bootstrap_id=boot, remote_high_water=0)
    assert client.post('/api/v2/sync/encrypted/cutover', headers=headers, json=dict(expected_cutover_epoch=0)).status_code == 200
    for identity in (ia, ib):
        assert client.post('/api/v3/sync/encrypted/reader-ready', headers=headers,
            json=dict(device_id=identity['device_id'], reader_transport_version=3)).status_code == 204
    assert client.post('/api/v3/sync/encrypted/cutover', headers=headers, json=dict(expected_cutover_epoch=1)).status_code == 200
    amk = list(os.urandom(32));native(tmp_path, a, ia, user, 'begin')
    meta, _ = publish(client, tmp_path, token, a, ia, user, amk)
    assert sync(client, tmp_path, token, a, ia, user, amk)['view']['state'] == 'active'
    bootstrap(tmp_path, b, ib, 'bootstrap_import', bootstrap_id=boot, remote_high_water=1,
        display_name=meta['metadata']['name'], authenticated_metadata=meta['metadata'])
    assert sync(client, tmp_path, token, b, ib, user, amk)['view']['state'] == 'active'
    for path, identity in ((a, ia), (b, ib)):
        support(client, headers, identity['device_id'])
        assert game(tmp_path, path, identity, user, 'init') is True
        assert client.put('/api/v3/sync/encrypted/game-reader-capabilities', headers=headers,
            json=declaration(identity['device_id'])).status_code == 204
    with sqlite3.connect(b) as db:
        register_remote_apply_authorization_guard(db)
        db.execute("UPDATE projects SET payload_json=json_set(payload_json,'$.total',0,'$.progress_entries',json('[]'))")
    progress(tmp_path, a, ia, user, 'begin');emit_progress(client, tmp_path, token, a, ia, user, amk)
    for path, identity in ((a, ia), (b, ib)):
        assert receive_progress(client, tmp_path, token, path, identity, user, amk) == ['applied']
    source = dict(gamer=json.loads((ROOT/'frontend/src/cloud/__fixtures__/gameLegacyDefaultsV1.json').read_text()))
    source['gamer']['coins'] = 100
    source['global_streak'] = dict(global_streaks=['2026-10-01','2026-10-02','2026-10-03'],
                                   max_global_streak=3,global_streak_status='Go')
    assert game(tmp_path,a,ia,user,'init',source=source) is True
    command(tmp_path,a,ia,user,'begin',now=NOW)
    assert len(emit_game(client,tmp_path,token,a,ia,user,amk)) == 2
    for path,identity in ((a,ia),(b,ib)):
        receive_mixed(client,tmp_path,token,path,identity,user,amk)
    assert projections(a) == projections(b)
    before = game(tmp_path,a,ia,user,'state')
    assert before['gamer']['health'] == 100
    assert game(tmp_path,a,ia,user,'loss',writing_day='2026-10-04',now='2026-10-04T00:00:00.000000Z') is False
    assert game(tmp_path,a,ia,user,'loss',writing_day='2026-10-05',now='2026-10-05T00:00:00.000000Z') is True
    pending=command(tmp_path,a,ia,user,'pending',sealed=False)
    assert len(pending)==1 and pending[0]['event']['action']['project_action_ids']==[]
    requests=emit_game(client,tmp_path,token,a,ia,user,amk)
    assert len(requests)==1
    for path,identity in ((a,ia),(b,ib)):
        receive_mixed(client,tmp_path,token,path,identity,user,amk)
        assert game(tmp_path,path,identity,user,'state')['gamer']['health']==85
        assert game(tmp_path,path,identity,user,'state')['notifications']['new'][0]['fields']['notification_id'].startswith('streak-loss:')
        assert game(tmp_path,path,identity,user,'loss',writing_day='2026-10-06',now='2026-10-06T00:00:00.000000Z') is False
    assert projections(a)==projections(b)
    assert command(tmp_path,b,ib,user,'pending',sealed=False)==[]
    for endpoint,body in requests:
        response=client.post(endpoint,headers=headers,json=body)
        assert response.status_code==200 and response.json()['results'][0]['duplicate']
    for path,identity in ((a,ia),(b,ib)):
        receive_mixed(client,tmp_path,token,path,identity,user,amk)
        assert game(tmp_path,path,identity,user,'state')['gamer']['health']==85
    verify_web_shop(client, engine, tmp_path, token, user, amk, ((a, ia), (b, ib)))
    heads=progress(tmp_path,a,ia,user,'view')['owners'][0]['tips']
    assert 'entry' in progress(tmp_path,a,ia,user,'manual',expected=heads,total=120)
    assert game(tmp_path,a,ia,user,'process')['processed']==1
    assert game(tmp_path,a,ia,user,'state')['gamer']['health']==85
    with sqlite3.connect(a) as db:
        assert db.execute("SELECT COUNT(*) FROM cloud_game_events WHERE json_extract(CAST(substr(canonical_frame,21) AS TEXT),'$.action.kind')='global_streak'").fetchone()[0]==1
