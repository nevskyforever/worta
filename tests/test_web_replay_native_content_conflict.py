"""Two real SQLite writers through a local transport into Chromium replay.

The transport is isolated test plumbing. It never declares browser capabilities
and is not a production reader or a server-to-browser acceptance.
"""
from __future__ import annotations

import json
import sqlite3
import subprocess
from pathlib import Path
from uuid import uuid4

from nfprogress.core.sqlite.connection import register_remote_apply_authorization_guard
from test_cloud_c15_headless_cross_runtime import PROJECT_ID
from test_cloud_c18_authority_cross_runtime import provision, bootstrap, native, publish, sync
from test_cloud_c18_content_note_acceptance import NOW, content, emit, receive
from test_cloud_c18_structural_cross_runtime import structural
from test_cloud_c18_structural_integration import publish_pending, pull_structure
from test_cloud_c18_catalog_integration import native as catalog_native
from test_cloud_c15_headless_cross_runtime import _crypto_bridge


class Response:
    def __init__(self, value):
        self.value = value
        self.status_code = 200
        self.text = json.dumps(value)

    def json(self):
        return self.value


class LocalTransport:
    def __init__(self):
        self.items = []
        self.responses = []

    def post(self, path, *, headers, json):
        assert path == '/api/v3/sync/encrypted/push'
        results = []
        for item in json['items']:
            previous = next((v for v in self.items if v['event']['event_id'] == item['event']['event_id']), None)
            if previous:
                assert previous['object'] == item['object']
                assert previous['event']['device_id'] == json['device_id']
                sequence = previous['event']['server_sequence']
            else:
                sequence = len(self.items) + 1
                self.items.append({'event': {**item['event'], 'device_id': json['device_id'], 'server_sequence': sequence}, 'object': item['object']})
            results.append({'event_id': item['event']['event_id'], 'server_sequence': sequence, 'duplicate': previous is not None})
        response = {'protocol_version': 3, 'encrypted_sync_version': 3, 'current_cursor': len(self.items), 'results': results}
        self.responses.append(response)
        return Response(response)

    def get(self, path, *, headers, params):
        assert path == '/api/v3/sync/encrypted/pull'
        since = params['since']
        return Response({'items': self.items[since:], 'next_cursor': len(self.items), 'has_more': False})


def test_native_two_project_bootstraps_share_account_transport(tmp_path):
    """Create two independent project authorities with native sealed Metadata."""
    transport, user, amk = LocalTransport(), str(uuid4()), list(range(32))
    path, identity = provision(tmp_path, 'two-project-author', user, False)
    with sqlite3.connect(path) as db:
        for index, project in enumerate(('isolation-A', 'isolation-B')):
            db.execute("INSERT INTO projects(id,name,infinite,unit,status,payload_json) VALUES(?,?,1,'symbols','активен','{}')", (project, project))
            db.execute('INSERT INTO project_order VALUES(?,?)', (project, index))
    boots = {}
    for project in ('isolation-A', 'isolation-B'):
        boot = catalog_native(tmp_path, path, identity, user, action='bootstrap_prepare', project_id=project)['bootstrap_id']
        boots[project] = boot
        catalog_native(tmp_path, path, identity, user, action='bootstrap_prepare_capture', project_id=project, bootstrap_id=boot, remote_high_water=0)
        catalog_native(tmp_path, path, identity, user, action='bootstrap_confirm_active', project_id=project, bootstrap_id=boot, remote_high_water=0)
        catalog_native(tmp_path, path, identity, user, action='metadata_authority', project_id=project, step='begin')
        event = catalog_native(tmp_path, path, identity, user, action='metadata_authority', project_id=project, step='read')['unsealed'][0]
        sealed = _crypto_bridge(tmp_path, dict(action='metadata_seal', payload=event, amk=amk))
        catalog_native(tmp_path, path, identity, user, action='metadata_authority', project_id=project, step='seal', event_id=event['header']['event_id'], nonce=sealed['nonce'], ciphertext=sealed['ciphertext'])
        header = event['header']
        wire = {key: header[key] for key in ('event_id', 'project_id', 'entity_id', 'revision', 'updated_at')}
        wire.update(entity_type='project_metadata', operation='upsert', deleted_at=None)
        response = transport.post('/api/v3/sync/encrypted/push', headers={}, json=dict(protocol_version=3, encrypted_sync_version=3, device_id=identity['device_id'], items=[dict(event=wire, object=sealed['object'])]))
        assert response.status_code == 200
    assert boots['isolation-A'] != boots['isolation-B']
    assert [item['event']['project_id'] for item in transport.items] == ['isolation-A', 'isolation-B']
    # Apply the published metadata through the native receive owner before
    # either project's structural writer is allowed to create Stage history.
    cursor = 0
    for item in transport.items:
        row = dict(item['event']); row['source_device_id'] = row.pop('device_id'); row['envelope'] = item['object']
        opened = _crypto_bridge(tmp_path, dict(action='metadata_open', canonical_user_id=user, amk=amk, item=item))
        catalog_native(tmp_path, path, identity, user, action='metadata_authority', project_id=row['project_id'], step='receive',
            command=dict(account_id=identity['local_account_id'], device_id=identity['device_id'], canonical_user_id=user,
                         expected_cursor=cursor, next_cursor=row['server_sequence'], has_more=False, items=[row]), opened=opened)
        cursor = row['server_sequence']
    source = json.loads((Path(__file__).resolve().parents[1] / 'frontend/src/cloud/__fixtures__/stageCodecV1.json').read_text())['event']['stage']
    for project in ('isolation-A', 'isolation-B'):
        stage_id = 'stage-' + project
        with sqlite3.connect(path) as db:
            db.execute('INSERT INTO stages(id,project_id,name,goal,infinite,unit,status,created_at,payload_json) VALUES(?,?,?,?,?,?,?,?,?)',
                       (stage_id, project, source['name'], source['goal'], int(source['infinite']), source['unit'], source['status'], source['created_at'], json.dumps(source)))
            db.execute('INSERT INTO stage_order VALUES(?,?,0)', (stage_id, project))
        structural(tmp_path, path, identity, user, 'begin', project_id=project)
        emitted = publish_pending(transport, tmp_path, 'local-only', path, identity, user, amk, project_id=project)
        assert len(emitted) == 1 and emitted[0]['header']['entity_type'] == 'stage'
        pull_structure(transport, tmp_path, 'local-only', path, identity, user, amk)
        structural(tmp_path, path, identity, user, 'advance', project_id=project)
        order = publish_pending(transport, tmp_path, 'local-only', path, identity, user, amk, project_id=project)
        assert len(order) == 1 and order[0]['header']['entity_type'] == 'stage_order'
        pull_structure(transport, tmp_path, 'local-only', path, identity, user, amk)
        structural(tmp_path, path, identity, user, 'advance', project_id=project)
    assert [(item['event']['project_id'], item['event']['entity_type']) for item in transport.items[2:]] == [
        ('isolation-A', 'stage'), ('isolation-A', 'stage_order'),
        ('isolation-B', 'stage'), ('isolation-B', 'stage_order')]
    fixture = {'scope': {'accountId': str(uuid4()), 'canonicalUserId': user, 'deviceId': str(uuid4())}, 'key': amk,
        'anchor': {'registry': {'current_cursor': len(transport.items), 'projects': [
            {'project_id': project, 'bootstrap_id': boots[project], 'origin_device_id': identity['device_id'],
             'state': 'active', 'initial_event_count': 0, 'initial_max_server_sequence': 0}
            for project in ('isolation-A', 'isolation-B')]},
            'transport': {'supported_transport_version': 2, 'writer_transport_version': 3, 'cutover_epoch': 1}},
        'items': transport.items}
    fixture_path = tmp_path / 'two-project-browser.json'
    fixture_path.write_text(json.dumps(fixture), encoding='utf-8'); fixture_path.chmod(0o600)
    result = subprocess.run(['node', 'scripts/browser-two-project-recovery-local.mjs', str(fixture_path)],
        cwd=Path(__file__).resolve().parents[1] / 'frontend', capture_output=True, text=True, timeout=180)
    assert result.returncode == 0, result.stdout + result.stderr


def native_stage_prefix(tmp_path, transport, a, ia, b, ib, user, amk):
    """Create and publish a Stage through the actual SQLite structural writer."""
    source = json.loads((Path(__file__).resolve().parents[1] / 'frontend/src/cloud/__fixtures__/stageCodecV1.json').read_text())['event']['stage']
    with sqlite3.connect(a) as db:
        db.execute('INSERT INTO stages(id,project_id,name,goal,infinite,unit,status,created_at,payload_json) VALUES(?,?,?,?,?,?,?,?,?)',
                   ('S1', PROJECT_ID, source['name'], source['goal'], int(source['infinite']), source['unit'], source['status'], source['created_at'], json.dumps(source)))
        db.execute('INSERT INTO stage_order VALUES(?,?,0)', ('S1', PROJECT_ID))
    structural(tmp_path, a, ia, user, 'begin')
    stage = publish_pending(transport, tmp_path, 'local-only', a, ia, user, amk)
    assert len(stage) == 1 and stage[0]['header']['entity_type'] == 'stage'
    for path, identity in ((a, ia), (b, ib)):
        pull_structure(transport, tmp_path, 'local-only', path, identity, user, amk)
    order = publish_pending(transport, tmp_path, 'local-only', a, ia, user, amk)
    assert len(order) == 1 and order[0]['header']['entity_type'] == 'stage_order'
    for path, identity in ((a, ia), (b, ib)):
        pull_structure(transport, tmp_path, 'local-only', path, identity, user, amk)
    return stage[0], order[0]


def test_native_stage_prefix_writer(tmp_path):
    transport, user, amk = LocalTransport(), str(uuid4()), list(range(32))
    a, ia = provision(tmp_path, 'stage-a', user, True)
    b, ib = provision(tmp_path, 'stage-b', user, False)
    boot = bootstrap(tmp_path, a, ia, 'bootstrap_prepare')['bootstrap_id']
    bootstrap(tmp_path, a, ia, 'bootstrap_prepare_capture', bootstrap_id=boot, remote_high_water=0)
    bootstrap(tmp_path, a, ia, 'bootstrap_confirm_active', bootstrap_id=boot, remote_high_water=0)
    native(tmp_path, a, ia, user, 'begin')
    metadata, _ = publish(transport, tmp_path, 'local-only', a, ia, user, amk)
    assert sync(transport, tmp_path, 'local-only', a, ia, user, amk)['view']['state'] == 'active'
    bootstrap(tmp_path, b, ib, 'bootstrap_import', bootstrap_id=boot, remote_high_water=1,
              display_name=metadata['metadata']['name'], authenticated_metadata=metadata['metadata'])
    assert sync(transport, tmp_path, 'local-only', b, ib, user, amk)['view']['state'] == 'active'
    native_stage_prefix(tmp_path, transport, a, ia, b, ib, user, amk)
    assert [item['event']['entity_type'] for item in transport.items] == ['project_metadata', 'stage', 'stage_order']


def native_content_resolution_history(tmp_path, canonical_user=None, successor=False, chain=False, repeated=False, generations=2, mixed=False, own_resolution=False, resurrection=False, repeat_resurrection=False, stage_format=None, prefix_cas=False):
    transport = LocalTransport()
    user, amk = canonical_user or str(uuid4()), list(range(32))
    a, ia = provision(tmp_path, 'content-a', user, True)
    b, ib = provision(tmp_path, 'content-b', user, False)
    assert a != b and ia['device_id'] != ib['device_id']
    with sqlite3.connect(a) as db:
        register_remote_apply_authorization_guard(db)
        db.execute("UPDATE projects SET status='активен' WHERE id=?", (PROJECT_ID,))
    boot = bootstrap(tmp_path, a, ia, 'bootstrap_prepare')['bootstrap_id']
    bootstrap(tmp_path, a, ia, 'bootstrap_prepare_capture', bootstrap_id=boot, remote_high_water=0)
    bootstrap(tmp_path, a, ia, 'bootstrap_confirm_active', bootstrap_id=boot, remote_high_water=0)
    native(tmp_path, a, ia, user, 'begin')
    metadata, _ = publish(transport, tmp_path, 'local-only', a, ia, user, amk)
    assert sync(transport, tmp_path, 'local-only', a, ia, user, amk)['view']['state'] == 'active'
    bootstrap(tmp_path, b, ib, 'bootstrap_import', bootstrap_id=boot, remote_high_water=1,
              display_name=metadata['metadata']['name'], authenticated_metadata=metadata['metadata'])
    assert sync(transport, tmp_path, 'local-only', b, ib, user, amk)['view']['state'] == 'active'

    peers = [(a, ia), (b, ib)]
    if own_resolution:
        c, ic = provision(tmp_path, 'content-resolution-author', user, False)
        bootstrap(tmp_path, c, ic, 'bootstrap_import', bootstrap_id=boot, remote_high_water=1,
                  display_name=metadata['metadata']['name'], authenticated_metadata=metadata['metadata'])
        assert sync(transport, tmp_path, 'local-only', c, ic, user, amk)['view']['state'] == 'active'
        peers.append((c, ic))

    if stage_format:
        native_stage_prefix(tmp_path, transport, a, ia, b, ib, user, amk)
    stage_id = 'S1' if stage_format else None
    content(tmp_path, a, ia, user, 'create', note_id='native-conflict', stage_id=stage_id, content_format=stage_format or 'plain')
    content(tmp_path, a, ia, user, 'begin')
    assert len(emit(transport, tmp_path, 'local-only', a, ia, user, amk)) == 1
    for path, identity in peers:
        assert len(receive(transport, tmp_path, 'local-only', path, identity, user, amk)) == 1

    for path, identity, text in ((a, ia, 'NATIVE_PRIVATE_A'), (b, ib, 'NATIVE_PRIVATE_B')):
        content(tmp_path, path, identity, user, 'edit', note_id='native-conflict', stage_id=stage_id, patch={'content': text})
    for path, identity in ((a, ia), (b, ib)):
        assert len(emit(transport, tmp_path, 'local-only', path, identity, user, amk)) == 1
    for path, identity in peers:
        receive(transport, tmp_path, 'local-only', path, identity, user, amk)
        groups = content(tmp_path, path, identity, user, 'conflicts')
        assert len(groups) == 1
        assert {version['event_id'] for version in groups[0]['versions']} == {
            item['event']['event_id'] for item in transport.items[-2:]}

    assert len(transport.items) == (6 if stage_format else 4)
    assert [item['event']['revision'] for item in transport.items[-3:]] == [1, 2, 2]
    assert [item['event']['device_id'] for item in transport.items[-2:]] == [ia['device_id'], ib['device_id']]
    assert all(b'NATIVE_PRIVATE_' not in json.dumps(item).encode() for item in transport.items)
    anchor = {'registry': {'current_cursor': len(transport.items), 'projects': [{'project_id': PROJECT_ID, 'bootstrap_id': boot,
        'origin_device_id': ia['device_id'], 'state': 'active', 'initial_event_count': 0, 'initial_max_server_sequence': 0}]},
        'transport': {'supported_transport_version': 2, 'writer_transport_version': 3, 'cutover_epoch': 1}}
    fixture = {'scope': {'accountId': str(uuid4()), 'canonicalUserId': user, 'deviceId': str(uuid4())},
               'key': amk, 'anchor': anchor, 'items': transport.items,
               'expectedContents': ['NATIVE_PRIVATE_A', 'NATIVE_PRIVATE_B'], 'stageRoute': bool(stage_format), 'prefixCas': prefix_cas}
    path = tmp_path / 'native-browser-fixture.json'
    path.write_text(json.dumps(fixture), encoding='utf-8'); path.chmod(0o600)
    root = Path(__file__).resolve().parents[1]
    if not stage_format:
        result = subprocess.run(['node', 'scripts/browser-replay-conflict-local.mjs', str(path)], cwd=root / 'frontend',
                                capture_output=True, text=True, timeout=120)
        assert result.returncode == 0, result.stdout + result.stderr
        assert json.loads(result.stdout.strip().splitlines()[-1])['pass']

    # Exercise the real Desktop choice only after both devices have retained
    # the peer conflict; then deliver the genuine emitted wire to Chromium.
    chooser, chooser_identity = (c, ic) if own_resolution else (a, ia)
    decision = content(tmp_path, chooser, chooser_identity, user, 'conflicts')[0]
    selected = decision['versions'][0]['event_id']
    selected_note = decision['versions'][0]['note']
    assert content(tmp_path, chooser, chooser_identity, user, 'choose', decision=decision, selected=selected, now=NOW) == 'Applied'
    pending = content(tmp_path, chooser, chooser_identity, user, 'pending')
    assert len(pending) == 1
    resolution = json.loads(bytes(pending[0]['frame'][20:]))['event']
    assert resolution['mutation'] == 'resolution'
    assert resolution['resolution']['strategy'] == 'choose_version'
    assert resolution['resolution']['selected_event_id'] == selected
    assert set(resolution['resolution']['resolved_event_ids']) == {
        item['event']['event_id'] for item in transport.items[-2:]}
    assert resolution['result']['note'] == selected_note
    publication = emit(transport, tmp_path, 'local-only', chooser, chooser_identity, user, amk)
    assert len(publication) == 1
    if own_resolution:
        fixture['scope'] = {'accountId': ic['local_account_id'], 'canonicalUserId': user, 'deviceId': ic['device_id']}
        fixture['ownResolution'] = {'rawBody': json.dumps(publication[0]), 'response': transport.responses[-1]}
    assert b'NATIVE_PRIVATE_' not in json.dumps(transport.items[-1]).encode()
    for db_path, identity in peers:
        receive(transport, tmp_path, 'local-only', db_path, identity, user, amk)
        assert content(tmp_path, db_path, identity, user, 'conflicts') == []
        with sqlite3.connect(db_path) as db:
            saved = json.loads(db.execute("SELECT payload_json FROM notes WHERE id='native-conflict'").fetchone()[0])
        assert saved['content'] == selected_note['content']

    fixture['anchor']['registry']['current_cursor'] = len(transport.items)
    fixture['items'] = transport.items
    fixture['selectedNote'] = selected_note
    path.write_text(json.dumps(fixture), encoding='utf-8')
    if not stage_format:
        result = subprocess.run(['node', 'scripts/browser-own-resolution-local.mjs' if own_resolution else 'scripts/browser-replay-resolution-local.mjs', str(path)], cwd=root / 'frontend',
                                capture_output=True, text=True, timeout=120)
        assert result.returncode == 0, result.stdout + result.stderr
        assert json.loads(result.stdout.strip().splitlines()[-1])['pass']

    if successor:
        content(tmp_path, b, ib, user, 'edit', note_id='native-conflict', stage_id=stage_id, patch={'content': 'NATIVE_PRIVATE_AFTER_RESOLUTION'})
        assert len(emit(transport, tmp_path, 'local-only', b, ib, user, amk)) == 1
        for db_path, identity in ((a, ia), (b, ib)):
            receive(transport, tmp_path, 'local-only', db_path, identity, user, amk)
            assert content(tmp_path, db_path, identity, user, 'conflicts') == []
        fixture['anchor']['registry']['current_cursor'] = 6
        fixture['items'] = transport.items
        fixture['successor'] = True
        fixture['selectedNote'] = {'content': 'NATIVE_PRIVATE_AFTER_RESOLUTION'}
        path.write_text(json.dumps(fixture), encoding='utf-8')
        result = subprocess.run(['node', 'scripts/browser-replay-resolution-local.mjs', str(path)], cwd=root / 'frontend',
                                capture_output=True, text=True, timeout=120)
        assert result.returncode == 0, result.stdout + result.stderr
        assert json.loads(result.stdout.strip().splitlines()[-1])['pass']

    if chain:
        assert successor
        for db_path, identity, action, patch in ((a, ia, 'edit', {'content': 'NATIVE_PRIVATE_CHAIN'}), (b, ib, 'delete', None)):
            kwargs = {'patch': patch} if patch else {'stage_id': None}
            content(tmp_path, db_path, identity, user, action, note_id='native-conflict', **kwargs)
            assert len(emit(transport, tmp_path, 'local-only', db_path, identity, user, amk)) == 1
            for path_, identity_ in ((a, ia), (b, ib)):
                receive(transport, tmp_path, 'local-only', path_, identity_, user, amk)
                assert content(tmp_path, path_, identity_, user, 'conflicts') == []
        fixture['anchor']['registry']['current_cursor'] = len(transport.items)
        fixture['items'] = transport.items
        fixture['interruptRevision'] = 5
        fixture['selectedNote'] = {'deleted': True}
        path.write_text(json.dumps(fixture), encoding='utf-8')
        result = subprocess.run(['node', 'scripts/browser-replay-resolution-local.mjs', str(path)], cwd=root / 'frontend',
                                capture_output=True, text=True, timeout=180)
        assert result.returncode == 0, result.stdout + result.stderr
        assert json.loads(result.stdout.strip().splitlines()[-1])['pass']

    if repeated:
        assert generations >= 2
        roots = [{'event_id': transport.items[-1]['event']['event_id'], 'sequence': len(transport.items), 'source_before': 3}]
        source_revision = 4
        for generation in range(2, generations + 1):
            if mixed:
                content(tmp_path, a, ia, user, 'edit', note_id='native-conflict', stage_id=stage_id, patch={'content': f'NATIVE_LINEAR_{generation}'})
                assert len(emit(transport, tmp_path, 'local-only', a, ia, user, amk)) == 1
                for path_, identity_ in ((a, ia), (b, ib)):
                    receive(transport, tmp_path, 'local-only', path_, identity_, user, amk)
                source_revision += 1
            if mixed:
                content(tmp_path, a, ia, user, 'delete', note_id='native-conflict', stage_id=stage_id)
            else:
                content(tmp_path, a, ia, user, 'edit', note_id='native-conflict', stage_id=stage_id, patch={'content': f'NATIVE_GENERATION_{generation}_A'})
            content(tmp_path, b, ib, user, 'edit', note_id='native-conflict', stage_id=stage_id, patch={'content': f'NATIVE_GENERATION_{generation}_B'})
            for path_, identity_ in ((a, ia), (b, ib)):
                assert len(emit(transport, tmp_path, 'local-only', path_, identity_, user, amk)) == 1
            source_revision += 1
            for path_, identity_ in ((a, ia), (b, ib)):
                receive(transport, tmp_path, 'local-only', path_, identity_, user, amk)
                assert len(content(tmp_path, path_, identity_, user, 'conflicts')) == 1
            decision = content(tmp_path, a, ia, user, 'conflicts')[0]
            selected = next(v for v in decision['versions'] if v['operation'] == ('delete' if resurrection else 'upsert'))
            assert content(tmp_path, a, ia, user, 'choose', decision=decision, selected=selected['event_id'], now=NOW) == 'Applied'
            assert len(emit(transport, tmp_path, 'local-only', a, ia, user, amk)) == 1
            roots.append({'event_id': transport.items[-1]['event']['event_id'], 'sequence': len(transport.items), 'source_before': source_revision})
            source_revision += 1
            for path_, identity_ in ((a, ia), (b, ib)):
                receive(transport, tmp_path, 'local-only', path_, identity_, user, amk)
                assert content(tmp_path, path_, identity_, user, 'conflicts') == []
        if resurrection:
            content(tmp_path, a, ia, user, 'create', note_id='native-conflict', stage_id=stage_id, content_format=stage_format or 'plain')
            pending = content(tmp_path, a, ia, user, 'pending')
            assert len(pending) == 1
            restored = json.loads(bytes(pending[0]['frame'][20:]))['event']
            assert restored['mutation'] == 'update'
            assert restored['header']['parent_event_id'] == roots[-1]['event_id']
            assert len(emit(transport, tmp_path, 'local-only', a, ia, user, amk)) == 1
            resurrection_event = transport.items[-1]['event']['event_id']
            resurrection_sequence = len(transport.items)
            for path_, identity_ in ((a, ia), (b, ib)):
                receive(transport, tmp_path, 'local-only', path_, identity_, user, amk)
            content(tmp_path, a, ia, user, 'edit', note_id='native-conflict', stage_id=stage_id, patch={'content': 'NATIVE_RESURRECTED_UPDATE'})
            assert len(emit(transport, tmp_path, 'local-only', a, ia, user, amk)) == 1
            for path_, identity_ in ((a, ia), (b, ib)):
                receive(transport, tmp_path, 'local-only', path_, identity_, user, amk)
            if repeat_resurrection:
                content(tmp_path, a, ia, user, 'delete', note_id='native-conflict', stage_id=stage_id)
                content(tmp_path, b, ib, user, 'edit', note_id='native-conflict', stage_id=stage_id, patch={'content': 'NATIVE_SECOND_FORK'})
                for path_, identity_ in ((a, ia), (b, ib)):
                    assert len(emit(transport, tmp_path, 'local-only', path_, identity_, user, amk)) == 1
                for path_, identity_ in ((a, ia), (b, ib)):
                    receive(transport, tmp_path, 'local-only', path_, identity_, user, amk)
                decision = content(tmp_path, a, ia, user, 'conflicts')[0]
                deleted = next(v for v in decision['versions'] if v['operation'] == 'delete')
                assert content(tmp_path, a, ia, user, 'choose', decision=decision, selected=deleted['event_id'], now=NOW) == 'Applied'
                assert len(emit(transport, tmp_path, 'local-only', a, ia, user, amk)) == 1
                resolution_id = transport.items[-1]['event']['event_id']
                for path_, identity_ in ((a, ia), (b, ib)):
                    receive(transport, tmp_path, 'local-only', path_, identity_, user, amk)
                content(tmp_path, a, ia, user, 'create', note_id='native-conflict', stage_id=stage_id, content_format=stage_format or 'plain')
                restored_again = json.loads(bytes(content(tmp_path, a, ia, user, 'pending')[0]['frame'][20:]))['event']
                assert restored_again['mutation'] == 'update' and restored_again['header']['parent_event_id'] == resolution_id
                assert len(emit(transport, tmp_path, 'local-only', a, ia, user, amk)) == 1
                for path_, identity_ in ((a, ia), (b, ib)):
                    receive(transport, tmp_path, 'local-only', path_, identity_, user, amk)
                content(tmp_path, a, ia, user, 'edit', note_id='native-conflict', stage_id=stage_id, patch={'content': 'NATIVE_RESURRECTED_UPDATE'})
                assert len(emit(transport, tmp_path, 'local-only', a, ia, user, amk)) == 1
                generations += 1
        elif mixed:
            content(tmp_path, a, ia, user, 'delete', note_id='native-conflict', stage_id=stage_id)
            assert len(emit(transport, tmp_path, 'local-only', a, ia, user, amk)) == 1
            for path_, identity_ in ((a, ia), (b, ib)):
                receive(transport, tmp_path, 'local-only', path_, identity_, user, amk)
            fixture['finalDeleted'] = True
        fixture['anchor']['registry']['current_cursor'] = len(transport.items)
        fixture['items'] = transport.items
        fixture['repeated'] = True
        fixture['successor'] = True
        fixture['selectedNote'] = {'content': 'NATIVE_RESURRECTED_UPDATE'} if resurrection else {'deleted': True} if mixed else selected['note']
        fixture['finalContent'] = resurrection
        fixture['expectedResolutionCount'] = generations
        interrupted = ({'event_id': resurrection_event, 'sequence': resurrection_sequence, 'source_before': source_revision}
                       if resurrection else roots[len(roots) // 2])
        fixture['interruptEventId'] = interrupted['event_id']
        fixture['interruptSequence'] = interrupted['sequence']
        fixture['sourceBeforeAbort'] = interrupted['source_before']
        path.write_text(json.dumps(fixture), encoding='utf-8')
        result = subprocess.run(['node', 'scripts/browser-replay-resolution-local.mjs', str(path)], cwd=root / 'frontend',
                                capture_output=True, text=True, timeout=300)
        assert result.returncode == 0, result.stdout + result.stderr
        assert json.loads(result.stdout.strip().splitlines()[-1])['pass']

    return fixture


def test_native_content_genesis_and_peer_conflict_in_chromium(tmp_path):
    native_content_resolution_history(tmp_path)


def test_native_post_resolution_successor_dispatch_in_chromium(tmp_path):
    native_content_resolution_history(tmp_path, successor=True)


def test_native_post_resolution_chain_and_delete_dispatch_in_chromium(tmp_path):
    native_content_resolution_history(tmp_path, successor=True, chain=True)


def test_native_two_resolution_generations_dispatch_in_chromium(tmp_path):
    native_content_resolution_history(tmp_path, repeated=True)


def test_native_repeating_resolution_dispatch_in_chromium(tmp_path):
    native_content_resolution_history(tmp_path, repeated=True, generations=4, mixed=True)


def test_native_resolution_tombstone_resurrection_in_chromium(tmp_path):
    native_content_resolution_history(tmp_path, repeated=True, mixed=True, resurrection=True)


def test_native_repeated_resolution_resurrection_in_chromium(tmp_path):
    native_content_resolution_history(tmp_path, repeated=True, mixed=True, resurrection=True, repeat_resurrection=True)


def test_native_parent_browser_content_author_self_echo_in_chromium(tmp_path):
    """Real Native SQLite history seeds existing browser owners; no fake authorship."""
    native_content_resolution_history(tmp_path)
    root = Path(__file__).resolve().parents[1]
    result = subprocess.run(
        ['node', 'scripts/browser-content-author-owner-local.mjs', str(tmp_path / 'native-browser-fixture.json')],
        cwd=root / 'frontend', capture_output=True, text=True, timeout=180,
    )
    assert result.returncode == 0, result.stdout + result.stderr
    assert json.loads(result.stdout.strip().splitlines()[-1])['pass']


def test_native_resolution_browser_replay_author_self_echo(tmp_path):
    fixture = native_content_resolution_history(tmp_path)
    fixture['authorSelfEcho'] = True
    path = tmp_path / 'native-browser-fixture.json'
    path.write_text(json.dumps(fixture), encoding='utf-8')
    root = Path(__file__).resolve().parents[1]
    result = subprocess.run(['node', 'scripts/browser-replay-resolution-local.mjs', str(path)], cwd=root / 'frontend', capture_output=True, text=True, timeout=180)
    assert result.returncode == 0, result.stdout + result.stderr
    assert json.loads(result.stdout.strip().splitlines()[-1])['pass']


def test_native_own_resolution_author_receipt_self_echo(tmp_path):
    native_content_resolution_history(tmp_path, own_resolution=True)


def test_native_stage_plain_resurrection_in_chromium(tmp_path):
    native_content_resolution_history(tmp_path, repeated=True, mixed=True, resurrection=True, stage_format='plain')


def test_native_stage_html_resurrection_in_chromium(tmp_path):
    native_content_resolution_history(tmp_path, repeated=True, mixed=True, resurrection=True, stage_format='html')


def test_native_prefix_cas_competing_replay_in_chromium(tmp_path):
    native_content_resolution_history(tmp_path, repeated=True, mixed=True, resurrection=True, stage_format='plain', prefix_cas=True)
