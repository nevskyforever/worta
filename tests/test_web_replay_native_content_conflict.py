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
        return Response({'results': results})

    def get(self, path, *, headers, params):
        assert path == '/api/v3/sync/encrypted/pull'
        since = params['since']
        return Response({'items': self.items[since:], 'next_cursor': len(self.items), 'has_more': False})


def native_content_resolution_history(tmp_path, canonical_user=None, successor=False, chain=False, repeated=False):
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

    content(tmp_path, a, ia, user, 'create', note_id='native-conflict', stage_id=None, content_format='plain')
    content(tmp_path, a, ia, user, 'begin')
    assert len(emit(transport, tmp_path, 'local-only', a, ia, user, amk)) == 1
    for path, identity in ((a, ia), (b, ib)):
        assert len(receive(transport, tmp_path, 'local-only', path, identity, user, amk)) == 1

    for path, identity, text in ((a, ia, 'NATIVE_PRIVATE_A'), (b, ib, 'NATIVE_PRIVATE_B')):
        content(tmp_path, path, identity, user, 'edit', note_id='native-conflict', patch={'content': text})
    for path, identity in ((a, ia), (b, ib)):
        assert len(emit(transport, tmp_path, 'local-only', path, identity, user, amk)) == 1
    for path, identity in ((a, ia), (b, ib)):
        receive(transport, tmp_path, 'local-only', path, identity, user, amk)
        groups = content(tmp_path, path, identity, user, 'conflicts')
        assert len(groups) == 1
        assert {version['event_id'] for version in groups[0]['versions']} == {
            item['event']['event_id'] for item in transport.items[-2:]}

    assert len(transport.items) == 4
    assert [item['event']['revision'] for item in transport.items[1:]] == [1, 2, 2]
    assert [item['event']['device_id'] for item in transport.items[-2:]] == [ia['device_id'], ib['device_id']]
    assert all(b'NATIVE_PRIVATE_' not in json.dumps(item).encode() for item in transport.items)
    anchor = {'registry': {'current_cursor': 4, 'projects': [{'project_id': PROJECT_ID, 'bootstrap_id': boot,
        'origin_device_id': ia['device_id'], 'state': 'active', 'initial_event_count': 0, 'initial_max_server_sequence': 0}]},
        'transport': {'supported_transport_version': 2, 'writer_transport_version': 3, 'cutover_epoch': 1}}
    fixture = {'scope': {'accountId': str(uuid4()), 'canonicalUserId': user, 'deviceId': str(uuid4())},
               'key': amk, 'anchor': anchor, 'items': transport.items,
               'expectedContents': ['NATIVE_PRIVATE_A', 'NATIVE_PRIVATE_B']}
    path = tmp_path / 'native-browser-fixture.json'
    path.write_text(json.dumps(fixture), encoding='utf-8'); path.chmod(0o600)
    root = Path(__file__).resolve().parents[1]
    result = subprocess.run(['node', 'scripts/browser-replay-conflict-local.mjs', str(path)], cwd=root / 'frontend',
                            capture_output=True, text=True, timeout=120)
    assert result.returncode == 0, result.stdout + result.stderr
    assert json.loads(result.stdout.strip().splitlines()[-1])['pass']

    # Exercise the real Desktop choice only after both devices have retained
    # the peer conflict; then deliver the genuine emitted wire to Chromium.
    decision = content(tmp_path, a, ia, user, 'conflicts')[0]
    selected = decision['versions'][0]['event_id']
    selected_note = decision['versions'][0]['note']
    assert content(tmp_path, a, ia, user, 'choose', decision=decision, selected=selected, now=NOW) == 'Applied'
    pending = content(tmp_path, a, ia, user, 'pending')
    assert len(pending) == 1
    resolution = json.loads(bytes(pending[0]['frame'][20:]))['event']
    assert resolution['mutation'] == 'resolution'
    assert resolution['resolution']['strategy'] == 'choose_version'
    assert resolution['resolution']['selected_event_id'] == selected
    assert set(resolution['resolution']['resolved_event_ids']) == {
        item['event']['event_id'] for item in transport.items[-2:]}
    assert resolution['result']['note'] == selected_note
    assert len(emit(transport, tmp_path, 'local-only', a, ia, user, amk)) == 1
    assert b'NATIVE_PRIVATE_' not in json.dumps(transport.items[-1]).encode()
    for db_path, identity in ((a, ia), (b, ib)):
        receive(transport, tmp_path, 'local-only', db_path, identity, user, amk)
        assert content(tmp_path, db_path, identity, user, 'conflicts') == []
        with sqlite3.connect(db_path) as db:
            saved = json.loads(db.execute("SELECT payload_json FROM notes WHERE id='native-conflict'").fetchone()[0])
        assert saved['content'] == selected_note['content']

    fixture['anchor']['registry']['current_cursor'] = 5
    fixture['items'] = transport.items
    fixture['selectedNote'] = selected_note
    path.write_text(json.dumps(fixture), encoding='utf-8')
    result = subprocess.run(['node', 'scripts/browser-replay-resolution-local.mjs', str(path)], cwd=root / 'frontend',
                            capture_output=True, text=True, timeout=120)
    assert result.returncode == 0, result.stdout + result.stderr
    assert json.loads(result.stdout.strip().splitlines()[-1])['pass']

    if successor:
        content(tmp_path, b, ib, user, 'edit', note_id='native-conflict', patch={'content': 'NATIVE_PRIVATE_AFTER_RESOLUTION'})
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
        for path_, identity_, text in ((a, ia, 'NATIVE_PRIVATE_SECOND_A'), (b, ib, 'NATIVE_PRIVATE_SECOND_B')):
            content(tmp_path, path_, identity_, user, 'edit', note_id='native-conflict', patch={'content': text})
        for path_, identity_ in ((a, ia), (b, ib)):
            assert len(emit(transport, tmp_path, 'local-only', path_, identity_, user, amk)) == 1
        for path_, identity_ in ((a, ia), (b, ib)):
            receive(transport, tmp_path, 'local-only', path_, identity_, user, amk)
            assert len(content(tmp_path, path_, identity_, user, 'conflicts')) == 1
        decision = content(tmp_path, a, ia, user, 'conflicts')[0]
        selected = decision['versions'][-1]
        assert content(tmp_path, a, ia, user, 'choose', decision=decision, selected=selected['event_id'], now=NOW) == 'Applied'
        assert len(emit(transport, tmp_path, 'local-only', a, ia, user, amk)) == 1
        for path_, identity_ in ((a, ia), (b, ib)):
            receive(transport, tmp_path, 'local-only', path_, identity_, user, amk)
            assert content(tmp_path, path_, identity_, user, 'conflicts') == []
        fixture['anchor']['registry']['current_cursor'] = len(transport.items)
        fixture['items'] = transport.items
        fixture['repeated'] = True
        fixture['successor'] = True
        fixture['selectedNote'] = selected['note']
        path.write_text(json.dumps(fixture), encoding='utf-8')
        result = subprocess.run(['node', 'scripts/browser-replay-resolution-local.mjs', str(path)], cwd=root / 'frontend',
                                capture_output=True, text=True, timeout=180)
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
