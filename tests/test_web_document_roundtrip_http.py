"""Locally authorized Chromium acceptance with isolated PostgreSQL and Desktop SQLite."""
import json
import os
import sqlite3
from datetime import datetime, timezone

from test_cloud_auth import ROOT, cloud_client, create_user, login, migrated_database
from test_cloud_c15_headless_cross_runtime import PROJECT_ID, _crypto_bridge, _headers
from test_cloud_c18_authority_cross_runtime import provision, bootstrap, native, publish, sync
from test_cloud_c18_structural_cross_runtime import structural
from test_cloud_c18_catalog_integration import native as native_for_project
from test_cloud_c18_structural_integration import publish_pending, pull_structure
from test_cloud_c18_content_note_gate import capabilities
from test_cloud_c18_document_acceptance import document, emit as emit_documents, support as support_documents
from test_cloud_c18_document_acceptance import receive as receive_documents
from test_web_catalog_recovery_http import run_browser
from nfprogress.core.sqlite.connection import register_remote_apply_authorization_guard


def test_web_document_roundtrip_http(cloud_client, tmp_path):
    client, engine = cloud_client
    user = str(create_user(engine))
    token = login(client).json()["access_token"]
    headers = _headers(token)
    db, identity = provision(tmp_path, "catalog-recovery", user, True)
    document(tmp_path, db, identity, user, 'init')
    with sqlite3.connect(db) as connection:
        register_remote_apply_authorization_guard(connection)
        connection.execute("UPDATE projects SET status='активен' WHERE id=?", (PROJECT_ID,))
        connection.execute("UPDATE projects SET payload_json=json_set(payload_json,'$.stages_enabled',json('true')) WHERE id=?", (PROJECT_ID,))
    stage = json.loads((ROOT / "frontend/src/cloud/__fixtures__/stageCodecV1.json").read_text())["event"]["stage"]
    with sqlite3.connect(db) as connection:
        details = dict(stage, name="Stage S1", work_method="app")
        connection.execute("INSERT INTO stages(id,project_id,name,goal,infinite,unit,status,created_at,payload_json) VALUES(?,?,?,?,?,?,?,?,?)",
            ("S1", PROJECT_ID, details["name"], details["goal"], int(details["infinite"]), details["unit"], details["status"], details["created_at"], json.dumps(details)))
        connection.execute("INSERT INTO stage_order VALUES(?,?,?)", ("S1", PROJECT_ID, 0))
    assert client.put(f"/api/v1/sync/devices/{identity['device_id']}", headers=headers).status_code == 200
    boot = bootstrap(tmp_path, db, identity, "bootstrap_prepare")["bootstrap_id"]
    assert client.post(f"/api/v1/cloud/projects/{PROJECT_ID}/bootstrap", headers=headers,
        json={"bootstrap_id": boot, "device_id": identity["device_id"]}).status_code == 200
    captured = bootstrap(tmp_path, db, identity, "bootstrap_prepare_capture", bootstrap_id=boot, remote_high_water=0)
    assert captured["record"]["initial_event_count"] == 0
    assert captured["event_ids"] == []
    assert client.post(f"/api/v1/cloud/projects/{PROJECT_ID}/bootstrap/complete", headers=headers,
        json={"bootstrap_id": boot, "device_id": identity["device_id"], "initial_event_count": 0, "initial_max_server_sequence": 0}).status_code == 200
    bootstrap(tmp_path, db, identity, "bootstrap_confirm_active", bootstrap_id=boot, remote_high_water=0)
    assert client.post("/api/v2/sync/encrypted/cutover", headers=headers, json={"expected_cutover_epoch": 0}).status_code == 200
    assert client.post("/api/v3/sync/encrypted/reader-ready", headers=headers,
        json={"device_id": identity["device_id"], "reader_transport_version": 3}).status_code == 204
    assert client.post("/api/v3/sync/encrypted/cutover", headers=headers, json={"expected_cutover_epoch": 1}).status_code == 200
    amk = list(os.urandom(32))
    native(tmp_path, db, identity, user, "begin")
    publish(client, tmp_path, token, db, identity, user, amk)
    assert sync(client, tmp_path, token, db, identity, user, amk)["view"]["state"] == "active"

    structural(tmp_path, db, identity, user, "begin")
    assert len(publish_pending(client, tmp_path, token, db, identity, user, amk)) == 1
    pull_structure(client, tmp_path, token, db, identity, user, amk)
    assert len(publish_pending(client, tmp_path, token, db, identity, user, amk)) == 1
    pull_structure(client, tmp_path, token, db, identity, user, amk)
    pull_structure(client, tmp_path, token, db, identity, user, amk)


    assert client.put("/api/v3/sync/encrypted/note-reader-capabilities", headers=headers,
                      json=capabilities(identity['device_id'])).status_code == 204
    support_documents(client, headers, identity['device_id'])
    formatted = {"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"Desktop manuscript","marks":[{"type":"bold"}]}]}]}
    saved = document(tmp_path, db, identity, user, 'save', stage_id='S1', content=formatted)
    assert 'error' not in saved, saved
    document(tmp_path, db, identity, user, 'begin')
    assert len(emit_documents(client, tmp_path, token, db, identity, user, amk)) == 1
    assert receive_documents(client, tmp_path, token, db, identity, user, amk) == ['applied']

    run_browser(client, tmp_path, {'documentRoundTrip': True, 'projectId': PROJECT_ID, 'amk': amk})
    assert receive_documents(client, tmp_path, token, db, identity, user, amk, limit=1) == ['applied']
    with sqlite3.connect(db) as connection:
        current = connection.execute('SELECT content_json FROM documents WHERE id=?', (saved['document_id'],)).fetchone()[0]
    assert 'Desktop manuscript plus Web' in current and 'bold' in current


    with sqlite3.connect(db) as connection:
        cursor = connection.execute('SELECT pull_cursor FROM cloud_sync_state').fetchone()[0]
    response = client.get('/api/v3/sync/encrypted/pull', headers=headers, params={
        'device_id': identity['device_id'], 'since': cursor, 'limit': 100,
        'protocol_version': 3, 'encrypted_sync_version': 3,
    })
    assert response.status_code == 200, response.text
    items = response.json()['items']
    assert [item['event']['entity_type'] for item in items] == [
        'stage', 'stage_order', 'project_metadata', 'document', 'stage', 'stage_order', 'project_metadata', 'document',
    ]
    web_project = items[2]['event']['project_id']
    for item in items:
        row = dict(item['event'])
        family, project = row['entity_type'], row['project_id']
        action = {'project_metadata': 'metadata', 'document': 'document'}.get(family, 'structural')
        opened = _crypto_bridge(tmp_path, dict(action=f'{action}_open', canonical_user_id=user, amk=amk, item=item))
        if family == 'project_metadata' and row['revision'] == 1:
            meta = opened['decoded']['metadata']
            native_for_project(tmp_path, db, identity, user, action='bootstrap_import', project_id=project,
                bootstrap_id=opened['decoded']['header']['bootstrap_id'], remote_high_water=row['server_sequence'],
                display_name=meta['name'], authenticated_metadata=meta)
        row['source_device_id'] = row.pop('device_id')
        row['envelope'] = item['object']
        row['updated_at'] = datetime.fromisoformat(row['updated_at']).astimezone(timezone.utc).isoformat(timespec='microseconds').replace('+00:00', 'Z')
        command = dict(account_id=identity['local_account_id'], device_id=identity['device_id'], canonical_user_id=user,
            expected_cursor=cursor, next_cursor=row['server_sequence'], has_more=False, items=[row])
        if action == 'structural':
            structural(tmp_path, db, identity, user, 'persist', project_id=project, command=command)
            outcome = structural(tmp_path, db, identity, user, 'apply', project_id=project, event_id=row['event_id'], opened=opened)
            assert outcome == 'applied', outcome
        else:
            native_for_project(tmp_path, db, identity, user, action='metadata_authority' if action == 'metadata' else 'document',
                project_id=project, step='receive', command=command, opened=opened)
        cursor = row['server_sequence']

    with sqlite3.connect(db) as connection:
        assert connection.execute("SELECT name, json_extract(payload_json, '$.stages_enabled') FROM projects WHERE id=?",
                                  (web_project,)).fetchone() == ('New Web manuscript project', 1)
        stages = connection.execute('SELECT s.name,s.goal,o.position FROM stages s JOIN stage_order o ON s.id=o.stage_id '
                                    'WHERE s.project_id=? ORDER BY o.position', (PROJECT_ID,)).fetchall()
        assert stages == [('Stage S1', 1234.5, 0), ('Web stage in Desktop project', 4000.0, 1)]
        texts = connection.execute('SELECT s.name,d.content_json FROM documents d LEFT JOIN stages s ON s.id=d.stage_id '
                                   'WHERE d.project_id=? ORDER BY s.name', (web_project,)).fetchall()
        assert len(texts) == 2 and texts[0][0] is None and 'First project text' in texts[0][1]
        assert texts[1][0] == 'First Web stage' and 'First stage text' in texts[1][1]
