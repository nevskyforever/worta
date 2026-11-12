"""Real Web UI opens one encrypted cloud project from the catalog."""
import json
import os
import socket
import sqlite3
import subprocess
import threading
from pathlib import Path

import uvicorn
import pytest

from test_cloud_auth import ROOT, cloud_client, create_user, login, migrated_database
from test_cloud_c15_headless_cross_runtime import PROJECT_ID, _crypto_bridge, _headers
from test_cloud_c18_authority_cross_runtime import provision, bootstrap, native, publish, sync
from test_cloud_c18_structural_cross_runtime import structural, event, structural_publish, structural_sync
from test_cloud_c18_structural_integration import publish_pending, pull_structure
from test_cloud_c18_content_note_acceptance import content, note, emit, receive
from test_cloud_c18_content_note_gate import capabilities
from test_cloud_c18_document_acceptance import document, emit as emit_documents, support as support_documents
from nfprogress.core.sqlite.connection import register_remote_apply_authorization_guard


def test_web_catalog_recovery_http(cloud_client, tmp_path):
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
    assert client.put(f"/api/v1/sync/devices/{identity['device_id']}", headers=headers).status_code == 200
    boot = bootstrap(tmp_path, db, identity, "bootstrap_prepare")["bootstrap_id"]
    assert client.post(f"/api/v1/cloud/projects/{PROJECT_ID}/bootstrap", headers=headers,
        json={"bootstrap_id": boot, "device_id": identity["device_id"]}).status_code == 200
    bootstrap(tmp_path, db, identity, "bootstrap_prepare_capture", bootstrap_id=boot, remote_high_water=0)
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

    stage = json.loads((ROOT / "frontend/src/cloud/__fixtures__/stageCodecV1.json").read_text())["event"]["stage"]
    with sqlite3.connect(db) as connection:
        details = dict(stage, name="Stage S1", work_method="app")
        connection.execute("INSERT INTO stages(id,project_id,name,goal,infinite,unit,status,created_at,payload_json) VALUES(?,?,?,?,?,?,?,?,?)",
            ("S1", PROJECT_ID, details["name"], details["goal"], int(details["infinite"]), details["unit"], details["status"], details["created_at"], json.dumps(details)))
        connection.execute("INSERT INTO stage_order VALUES(?,?,?)", ("S1", PROJECT_ID, 0))
    structural(tmp_path, db, identity, user, "begin")
    assert len(publish_pending(client, tmp_path, token, db, identity, user, amk)) == 1
    pull_structure(client, tmp_path, token, db, identity, user, amk)
    assert len(publish_pending(client, tmp_path, token, db, identity, user, amk)) == 1
    pull_structure(client, tmp_path, token, db, identity, user, amk)
    pull_structure(client, tmp_path, token, db, identity, user, amk)

    content(tmp_path, db, identity, user, "create", note_id="native-note", stage_id="S1", content_format="plain")
    content(tmp_path, db, identity, user, "begin")
    pending = content(tmp_path, db, identity, user, "pending")
    assert len(pending) == 1
    item = pending[0]
    sealed = _crypto_bridge(tmp_path, {"action": "content_note_seal", "frame": item["frame"], "amk": amk})
    header = json.loads(bytes(item["frame"][20:]))["event"]["header"]
    wire = {key: header[key] for key in ("event_id", "project_id", "entity_id", "revision", "updated_at")}
    wire.update(entity_type="note", operation="event", deleted_at=None)
    response = client.post("/api/v3/sync/encrypted/push", headers=headers, json={
        "protocol_version": 3, "encrypted_sync_version": 3, "device_id": identity["device_id"],
        "items": [{"event": wire, "object": sealed["object"]}],
    })
    assert response.status_code == 409
    assert response.json()["detail"]["code"] == "content_note_readers_not_ready"
    response = client.put("/api/v3/sync/encrypted/note-reader-capabilities", headers=headers,
        json=capabilities(identity["device_id"]))
    assert response.status_code == 204, response.text
    response = client.post("/api/v3/sync/encrypted/push", headers=headers, json={
        "protocol_version": 3, "encrypted_sync_version": 3, "device_id": identity["device_id"],
        "items": [{"event": wire, "object": sealed["object"]}],
    })
    assert response.status_code == 200, response.text
    sequence = response.json()["results"][0]["server_sequence"]
    content(tmp_path, db, identity, user, "seal", event_id=item["event_id"], frame=item["frame"], envelope=sealed["object"])
    content(tmp_path, db, identity, user, "receipt", event_id=item["event_id"], server_sequence=sequence, duplicate=False)
    receive(client, tmp_path, token, db, identity, user, amk)
    with sqlite3.connect(db) as connection:
        expected_content = json.loads(connection.execute("SELECT payload_json FROM notes WHERE id='native-note'").fetchone()[0])["content"]

    support_documents(client, headers, identity['device_id'])
    manuscript = 'Formatted manuscript from Desktop'
    saved = document(tmp_path, db, identity, user, 'save', stage_id='S1', content={
        'type': 'doc', 'content': [{'type': 'paragraph', 'content': [
            {'type': 'text', 'text': manuscript, 'marks': [{'type': 'bold'},
                {'type': 'link', 'attrs': {'href': 'https://example.invalid/private'}}]},
        ]}],
    })
    assert 'error' not in saved, saved
    document(tmp_path, db, identity, user, 'begin')
    assert len(emit_documents(client, tmp_path, token, db, identity, user, amk)) == 1

    run_browser(client, tmp_path, {
        "amk": amk,
        "projectId": PROJECT_ID,
        "sequence": sequence,
        "expectedContent": expected_content,
        "contentFormat": "plain",
        "stageId": "S1",
        "stages": ["S1"],
        "expectedProjectName": "Cross-runtime project",
        "manuscript": manuscript,
    })


def test_web_created_empty_project_reopens_in_fresh_browser(cloud_client, tmp_path):
    client, engine = cloud_client
    create_user(engine)
    run_browser(client, tmp_path, {"webCreate": True, "expectedProjectName": "Облачный черновик"})


def run_browser(client, tmp_path, fixture):
    fixture_path = tmp_path / "catalog-recovery-browser.json"
    fixture_path.write_text(json.dumps(fixture), encoding="utf-8")
    fixture_path.chmod(0o600)

    api_socket = socket.socket()
    api_socket.bind(("127.0.0.1", 0))
    port = api_socket.getsockname()[1]
    server = uvicorn.Server(uvicorn.Config(client.app, lifespan="off", access_log=False, log_level="warning"))
    thread = threading.Thread(target=server.run, kwargs={"sockets": [api_socket]}, daemon=True)
    thread.start()
    try:
        profile_path = tmp_path / "persistent-browser"
        result = subprocess.run(
            ["node", "scripts/browser-catalog-recovery-http.mjs", str(fixture_path), f"http://127.0.0.1:{port}", str(profile_path)],
            cwd=ROOT / "frontend", capture_output=True, text=True, timeout=180)
        assert result.returncode == 0, result.stdout + result.stderr
        assert json.loads(result.stdout.strip().splitlines()[-1])["pass"]
    finally:
        server.should_exit = True
        thread.join(timeout=15)
        api_socket.close()
