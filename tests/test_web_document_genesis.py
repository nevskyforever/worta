"""Native Document genesis through browser replay ownerPort, without HTTP/UI."""
import json
import sqlite3
import subprocess
from pathlib import Path
from uuid import uuid4
from test_cloud_c15_headless_cross_runtime import PROJECT_ID
from test_cloud_c18_authority_cross_runtime import provision, bootstrap, native, publish, sync
from test_cloud_c18_document_acceptance import document, content, emit
from test_web_replay_native_content_conflict import LocalTransport


def test_native_document_genesis_indexeddb(tmp_path):
    transport, user, key = LocalTransport(), str(uuid4()), list(range(32))
    db, identity = provision(tmp_path, 'document-native', user, True)
    from nfprogress.core.sqlite.connection import register_remote_apply_authorization_guard
    document(tmp_path, db, identity, user, 'init')
    with sqlite3.connect(db) as connection:
        register_remote_apply_authorization_guard(connection)
        connection.execute("UPDATE projects SET status='активен',payload_json=json_set(payload_json,'$.work_method','app')")
    boot = bootstrap(tmp_path, db, identity, 'bootstrap_prepare')['bootstrap_id']
    bootstrap(tmp_path, db, identity, 'bootstrap_prepare_capture', bootstrap_id=boot, remote_high_water=0)
    bootstrap(tmp_path, db, identity, 'bootstrap_confirm_active', bootstrap_id=boot, remote_high_water=0)
    native(tmp_path, db, identity, user, 'begin')
    publish(transport, tmp_path, 'local-only', db, identity, user, key)
    assert sync(transport, tmp_path, 'local-only', db, identity, user, key)['view']['state'] == 'active'
    text = 'Native Document genesis for browser owner'
    saved = document(tmp_path, db, identity, user, 'save', content=content(text))
    assert 'error' not in saved
    document(tmp_path, db, identity, user, 'begin')
    expected_document = document(tmp_path, db, identity, user, 'pending')[0]['event']['document']
    assert len(emit(transport, tmp_path, 'local-only', db, identity, user, key)) == 1
    assert [item['event']['entity_type'] for item in transport.items] == ['project_metadata', 'document']
    fixture = {'scope': {'accountId': str(uuid4()), 'deviceId': str(uuid4()), 'canonicalUserId': user}, 'key': key,
        'anchor': {'registry': {'current_cursor': 1, 'projects': [{'project_id': PROJECT_ID, 'bootstrap_id': boot,
            'origin_device_id': identity['device_id'], 'state': 'active', 'initial_event_count': 0, 'initial_max_server_sequence': 0}]},
            'transport': {'supported_transport_version': 2, 'writer_transport_version': 3, 'cutover_epoch': 1}},
        'items': transport.items, 'expected': content(text), 'expectedDocument': expected_document, 'project': PROJECT_ID}
    path = tmp_path / 'document-fixture.json'
    path.write_text(json.dumps(fixture)); path.chmod(0o600)
    result = subprocess.run(['node', 'scripts/browser-document-owner.mjs', str(path)], cwd=Path(__file__).resolve().parents[1] / 'frontend', capture_output=True, text=True, timeout=180)
    assert result.returncode == 0, result.stdout + result.stderr
