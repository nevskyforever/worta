//! Test-only JSON bridge used by the cross-runtime C15 acceptance proof.
//! It is compiled only by `cargo test` and exposes no production command.

use std::path::{Path, PathBuf};

use rusqlite::OptionalExtension;
use serde_json::{json, Value};

use crate::account_binding::{provision_cloud_identity, CloudIdentity};
use crate::note_sync::{
    apply_prepared_note_conflict_resolution, apply_verified_received_note_ipc,
    apply_verified_received_resolution_v2_ipc, capture_initial_note_sync_intents,
    commit_note_resolution_upload_acceptance, commit_note_sync_ack, commit_note_sync_inbound_page,
    commit_note_sync_upload_acceptance, commit_sealed_note_resolution_event,
    commit_sealed_note_sync_event, confirm_cloud_project_registration, import_remote_cloud_project,
    list_sealed_note_resolution_uploads, list_unsealed_note_resolution_intents,
    mark_cloud_project_bootstrap_completing, mark_cloud_project_bootstrap_ready,
    prepare_cloud_project_bootstrap, prepare_note_conflict_resolution, prepare_note_sync_ack,
    read_initial_note_cohort_status, read_note_resolution_readiness,
    reconcile_verified_received_resolution_self_echo_ipc, ApplyVerifiedReceivedNoteIpcCommand,
    ApplyVerifiedReceivedResolutionV2IpcCommand, CloudProjectBootstrapScopeCommand,
    CommitNoteResolutionUploadAcceptanceCommand, CommitNoteSyncAckCommand,
    CommitNoteSyncInboundPageCommand, CommitNoteSyncUploadAcceptanceCommand,
    CommitSealedNoteResolutionCommand, CommitSealedNoteSyncEventCommand,
    ConfirmCloudProjectRegistrationCommand, EncryptedNoteSyncEnvelope,
    ImportRemoteCloudProjectCommand, InboundNoteSyncItem, ListSealedNoteResolutionUploadsCommand,
    NoteSyncUploadReceipt, PrepareCloudProjectBootstrapCommand,
    PrepareNoteConflictResolutionCommand, PrepareNoteSyncAckCommand,
    ReconcileVerifiedReceivedResolutionSelfEchoIpcCommand,
};
use crate::sqlite::{open_database, open_privileged_remote_apply_database};

fn required_string<'a>(value: &'a Value, key: &str) -> &'a str {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("missing string field {key}"))
}

fn required_i64(value: &Value, key: &str) -> i64 {
    value
        .get(key)
        .and_then(Value::as_i64)
        .unwrap_or_else(|| panic!("missing integer field {key}"))
}

fn bytes(value: &Value, key: &str) -> Vec<u8> {
    value
        .get(key)
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("missing byte array {key}"))
        .iter()
        .map(|item| {
            item.as_u64()
                .filter(|byte| *byte <= 255)
                .expect("invalid byte") as u8
        })
        .collect()
}

fn database_path(request: &Value) -> PathBuf {
    PathBuf::from(required_string(request, "database_path"))
}

fn write_response(value: &Value) {
    let output = std::env::var("NFPROGRESS_C15_NATIVE_BRIDGE_RESPONSE")
        .expect("NFPROGRESS_C15_NATIVE_BRIDGE_RESPONSE is required");
    std::fs::write(
        output,
        serde_json::to_vec(value).expect("serialize bridge response"),
    )
    .expect("write bridge response");
}

fn provision(request: &Value) -> Value {
    let path = database_path(request);
    let root = Path::new(required_string(request, "data_root"));
    std::fs::create_dir_all(root).expect("create device root");
    let mut connection = open_database(&path).expect("open device database");
    crate::initialize_fresh_desktop_database(&connection, root)
        .expect("initialize desktop database");
    let identity: CloudIdentity = provision_cloud_identity(
        &mut connection,
        required_string(request, "canonical_user_id"),
    )
    .expect("provision cloud identity");
    let project_id = required_string(request, "project_id");
    let create_project = request
        .get("create_project")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    if create_project {
        connection
            .execute(
                "INSERT INTO projects(id,name,goal,infinite,unit,status,payload_json)
             VALUES(?1,'Cross-runtime project',NULL,1,'symbols','active','{}')",
                [project_id],
            )
            .expect("create project fixture");
        connection
            .execute(
                "INSERT INTO project_order(project_id,position) VALUES(?1,0)",
                [project_id],
            )
            .expect("create project order fixture");
    }
    if create_project
        && request
            .get("bind_project")
            .and_then(Value::as_bool)
            .unwrap_or(true)
    {
        connection.execute(
            "INSERT INTO cloud_sync_project_bindings(project_id,account_id,created_at,updated_at)
             VALUES(?1,?2,'2026-09-23T00:00:00.000000Z','2026-09-23T00:00:00.000000Z')",
            rusqlite::params![project_id, identity.local_account_id],
        ).expect("create explicit cloud project binding fixture");
    }
    if let Some(notes) = request.get("notes").and_then(Value::as_array) {
        for note in notes {
            connection
                .execute(
                    "INSERT INTO notes(id,project_id,stage_id,updated_at,payload_json)
                 VALUES(?1,?2,NULL,?3,?4)",
                    rusqlite::params![
                        required_string(note, "id"),
                        project_id,
                        required_string(note, "updated_at"),
                        serde_json::to_string(note).unwrap(),
                    ],
                )
                .expect("create Note fixture");
        }
    }
    json!({
        "local_account_id": identity.local_account_id,
        "device_id": identity.device_id,
    })
}

fn bootstrap_scope(request: &Value) -> CloudProjectBootstrapScopeCommand {
    CloudProjectBootstrapScopeCommand {
        project_id: required_string(request, "project_id").to_string(),
        account_id: required_string(request, "local_account_id").to_string(),
        device_id: required_string(request, "device_id").to_string(),
        bootstrap_id: required_string(request, "bootstrap_id").to_string(),
    }
}

fn bootstrap_prepare(request: &Value) -> Value {
    let path = database_path(request);
    let mut connection = open_database(&path).expect("open bootstrap database");
    serde_json::to_value(
        prepare_cloud_project_bootstrap(
            &mut connection,
            &PrepareCloudProjectBootstrapCommand {
                project_id: required_string(request, "project_id").to_string(),
                account_id: required_string(request, "local_account_id").to_string(),
                device_id: required_string(request, "device_id").to_string(),
                mode: crate::note_sync::CloudProjectBootstrapMode::UploadExisting,
            },
        )
        .expect("prepare durable bootstrap token"),
    )
    .unwrap()
}

fn bootstrap_prepare_capture(request: &Value) -> Value {
    let path = database_path(request);
    let mut connection = open_database(&path).expect("open bootstrap database");
    let prepared = prepare_cloud_project_bootstrap(
        &mut connection,
        &PrepareCloudProjectBootstrapCommand {
            project_id: required_string(request, "project_id").to_string(),
            account_id: required_string(request, "local_account_id").to_string(),
            device_id: required_string(request, "device_id").to_string(),
            mode: crate::note_sync::CloudProjectBootstrapMode::UploadExisting,
        },
    )
    .expect("prepare durable bootstrap token");
    assert_eq!(
        prepared.bootstrap_id,
        required_string(request, "bootstrap_id")
    );
    let registered = confirm_cloud_project_registration(
        &mut connection,
        &ConfirmCloudProjectRegistrationCommand {
            project_id: prepared.project_id.clone(),
            account_id: prepared.account_id.clone(),
            device_id: prepared.device_id.clone(),
            bootstrap_id: prepared.bootstrap_id.clone(),
            remote_state: "initializing".to_string(),
            remote_high_water: required_i64(request, "remote_high_water"),
        },
    )
    .expect("confirm server registration");
    let captured = capture_initial_note_sync_intents(
        &mut connection,
        &CloudProjectBootstrapScopeCommand {
            project_id: registered.project_id,
            account_id: registered.account_id,
            device_id: registered.device_id,
            bootstrap_id: registered.bootstrap_id,
        },
    )
    .expect("capture initial Note cohort");
    let mut statement = connection.prepare(
        "SELECT event.event_id,event.project_id,event.entity_id,event.entity_type,event.operation,
                event.revision,event.updated_at,event.deleted_at,intent.mutation_generation,intent.snapshot_json
         FROM cloud_sync_outbox event JOIN cloud_sync_note_intents intent USING(event_id)
         WHERE event.account_id=?1 AND event.device_id=?2 AND event.local_ordinal<=?3
         ORDER BY event.local_ordinal",
    ).expect("prepare captured cohort read");
    let events: Vec<Value> = statement.query_map(
        rusqlite::params![captured.account_id, captured.device_id, captured.initial_local_ordinal_hi],
        |row| Ok(json!({
            "event_id":row.get::<_,String>(0)?, "project_id":row.get::<_,String>(1)?,
            "entity_id":row.get::<_,String>(2)?, "entity_type":row.get::<_,String>(3)?,
            "operation":row.get::<_,String>(4)?, "revision":row.get::<_,i64>(5)?,
            "updated_at":row.get::<_,String>(6)?, "deleted_at":row.get::<_,Option<String>>(7)?,
            "mutation_generation":row.get::<_,i64>(8)?,
            "note":serde_json::from_str::<Value>(&row.get::<_,String>(9)?).unwrap(),
        })),
    ).expect("read captured cohort").collect::<Result<_,_>>().expect("collect captured cohort");
    drop(statement);
    let mut ids_statement = connection
        .prepare(
            "SELECT event_id FROM cloud_sync_outbox WHERE account_id=?1 AND device_id=?2
         AND local_ordinal<=?3 ORDER BY local_ordinal",
        )
        .expect("prepare cohort identity read");
    let event_ids: Vec<String> = ids_statement
        .query_map(
            rusqlite::params![
                captured.account_id,
                captured.device_id,
                captured.initial_local_ordinal_hi
            ],
            |row| row.get(0),
        )
        .expect("read cohort identities")
        .collect::<Result<_, _>>()
        .expect("collect cohort identities");
    json!({"record":captured,"events":events,"event_ids":event_ids})
}

fn bootstrap_commit_upload(request: &Value) -> Value {
    let path = database_path(request);
    let mut connection = open_database(&path).expect("open bootstrap receipt database");
    for accepted in request
        .get("accepted")
        .and_then(Value::as_array)
        .expect("accepted receipts")
    {
        let object = accepted.get("object").expect("sealed object");
        commit_sealed_note_sync_event(
            &mut connection,
            &CommitSealedNoteSyncEventCommand {
                event_id: required_string(accepted, "event_id").to_string(),
                expected_mutation_generation: required_i64(accepted, "mutation_generation"),
                envelope: EncryptedNoteSyncEnvelope {
                    crypto_version: required_i64(object, "crypto_version"),
                    aad_version: required_i64(object, "aad_version"),
                    nonce: required_string(object, "nonce").to_string(),
                    ciphertext: required_string(object, "ciphertext").to_string(),
                },
            },
        )
        .expect("commit immutable sealed envelope");
        commit_note_sync_upload_acceptance(
            &mut connection,
            &CommitNoteSyncUploadAcceptanceCommand {
                account_id: required_string(request, "local_account_id").to_string(),
                device_id: required_string(request, "device_id").to_string(),
                receipts: vec![NoteSyncUploadReceipt {
                    event_id: required_string(accepted, "event_id").to_string(),
                    server_sequence: required_i64(accepted, "server_sequence"),
                    duplicate: false,
                }],
            },
        )
        .expect("commit durable server receipt");
    }
    let cohort = read_initial_note_cohort_status(&mut connection, &bootstrap_scope(request))
        .expect("read initial cohort");
    let completing = if cohort.complete {
        Some(
            mark_cloud_project_bootstrap_completing(&mut connection, &bootstrap_scope(request))
                .expect("mark completing"),
        )
    } else {
        None
    };
    json!({"cohort":cohort,"completing":completing})
}

fn bootstrap_confirm_active(request: &Value) -> Value {
    let path = database_path(request);
    let mut connection = open_database(&path).expect("open active bootstrap database");
    serde_json::to_value(
        confirm_cloud_project_registration(
            &mut connection,
            &ConfirmCloudProjectRegistrationCommand {
                project_id: required_string(request, "project_id").to_string(),
                account_id: required_string(request, "local_account_id").to_string(),
                device_id: required_string(request, "device_id").to_string(),
                bootstrap_id: required_string(request, "bootstrap_id").to_string(),
                remote_state: "active".to_string(),
                remote_high_water: required_i64(request, "remote_high_water"),
            },
        )
        .expect("confirm active server registry"),
    )
    .unwrap()
}

fn bootstrap_import(request: &Value) -> Value {
    let path = database_path(request);
    let mut connection = open_database(&path).expect("open import database");
    serde_json::to_value(
        import_remote_cloud_project(
            &mut connection,
            &ImportRemoteCloudProjectCommand {
                project_id: required_string(request, "project_id").to_string(),
                display_name: required_string(request, "display_name").to_string(),
                account_id: required_string(request, "local_account_id").to_string(),
                device_id: required_string(request, "device_id").to_string(),
                bootstrap_id: required_string(request, "bootstrap_id").to_string(),
                remote_high_water: required_i64(request, "remote_high_water"),
                authenticated_metadata: request.get("authenticated_metadata").cloned(),
            },
        )
        .expect("import active remote project"),
    )
    .unwrap()
}

fn bootstrap_mark_ready(request: &Value) -> Value {
    let path = database_path(request);
    let mut connection = open_database(&path).expect("open ready bootstrap database");
    serde_json::to_value(
        mark_cloud_project_bootstrap_ready(&mut connection, &bootstrap_scope(request))
            .expect("mark bootstrap ready"),
    )
    .unwrap()
}

fn receive_apply_prepare(request: &Value) -> Value {
    let path = database_path(request);
    let account_id = required_string(request, "local_account_id").to_string();
    let device_id = required_string(request, "device_id").to_string();
    let canonical_user_id = required_string(request, "canonical_user_id").to_string();
    let item = request.get("item").expect("missing pulled item");
    let event = item.get("event").expect("missing pulled event");
    let object = item.get("object").expect("missing pulled object");
    let event_id = required_string(event, "event_id").to_string();
    let server_sequence = required_i64(event, "server_sequence");
    let source_device_id = required_string(event, "device_id").to_string();
    let next_cursor = required_i64(request, "next_cursor");
    let expected_cursor = request
        .get("expected_cursor")
        .and_then(Value::as_i64)
        .unwrap_or(0);

    let inbound = CommitNoteSyncInboundPageCommand {
        account_id: account_id.clone(),
        device_id: device_id.clone(),
        canonical_user_id: canonical_user_id.clone(),
        expected_cursor,
        next_cursor,
        has_more: false,
        items: vec![InboundNoteSyncItem {
            event_id: event_id.clone(),
            server_sequence,
            source_device_id: source_device_id.clone(),
            project_id: required_string(event, "project_id").to_string(),
            entity_id: required_string(event, "entity_id").to_string(),
            entity_type: required_string(event, "entity_type").to_string(),
            operation: required_string(event, "operation").to_string(),
            revision: required_i64(event, "revision"),
            updated_at: required_string(event, "updated_at").to_string(),
            deleted_at: event
                .get("deleted_at")
                .and_then(Value::as_str)
                .map(str::to_string),
            envelope: Some(EncryptedNoteSyncEnvelope {
                crypto_version: required_i64(object, "crypto_version"),
                aad_version: required_i64(object, "aad_version"),
                nonce: required_string(object, "nonce").to_string(),
                ciphertext: required_string(object, "ciphertext").to_string(),
            }),
        }],
    };
    let mut connection = open_database(&path).expect("open inbox database");
    let inbound_result = commit_note_sync_inbound_page(&mut connection, &inbound)
        .expect("commit encrypted inbound page");
    drop(connection);

    let opened = request
        .get("opened")
        .expect("missing TypeScript opened payload");
    let mut privileged =
        open_privileged_remote_apply_database(&path).expect("open protected apply database");
    let apply_result = apply_verified_received_note_ipc(
        &mut privileged,
        ApplyVerifiedReceivedNoteIpcCommand {
            account_id: account_id.clone(),
            canonical_user_id: canonical_user_id.clone(),
            pulling_device_id: device_id.clone(),
            event_id: event_id.clone(),
            server_sequence,
            source_device_id,
            crypto_version: required_i64(opened, "crypto_version"),
            aad_version: required_i64(opened, "aad_version"),
            nonce: bytes(opened, "nonce"),
            ciphertext: bytes(opened, "ciphertext"),
            plaintext: bytes(opened, "plaintext"),
        },
    )
    .expect("protected remote apply");
    let note_payload: Option<String> = privileged
        .connection()
        .query_row(
            "SELECT payload_json FROM notes WHERE id=?1",
            [required_string(event, "entity_id")],
            |row| row.get(0),
        )
        .optional()
        .expect("read applied Note");
    let head: Option<(String, i64)> = privileged
        .connection()
        .query_row(
            "SELECT head_event_id,head_sync_revision FROM cloud_sync_entities
         WHERE account_id=?1 AND project_id=?2 AND entity_id=?3 AND entity_type='note'",
            rusqlite::params![
                account_id,
                required_string(event, "project_id"),
                required_string(event, "entity_id")
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .expect("read applied Note head");
    let inbox_state: String = privileged
        .connection()
        .query_row(
            "SELECT state FROM cloud_sync_inbox WHERE account_id=?1 AND event_id=?2",
            rusqlite::params![account_id, event_id],
            |row| row.get(0),
        )
        .expect("read applied inbox state");
    drop(privileged);

    let mut connection = open_database(&path).expect("reopen ACK database");
    let candidate = prepare_note_sync_ack(
        &mut connection,
        &PrepareNoteSyncAckCommand {
            account_id,
            device_id,
            canonical_user_id,
        },
    )
    .expect("prepare durable ACK candidate");
    json!({
        "inbound": inbound_result,
        "apply_result": apply_result,
        "note": note_payload.map(|payload| serde_json::from_str::<Value>(&payload).expect("parse applied Note")),
        "head_event_id": head.as_ref().map(|value| value.0.clone()),
        "head_revision": head.as_ref().map(|value| value.1),
        "inbox_state": inbox_state,
        "ack": candidate,
    })
}

fn commit_ack(request: &Value) -> Value {
    let path = database_path(request);
    let account_id = required_string(request, "local_account_id").to_string();
    let mut connection = open_database(&path).expect("open ACK database");
    let result = commit_note_sync_ack(
        &mut connection,
        &CommitNoteSyncAckCommand {
            account_id: account_id.clone(),
            device_id: required_string(request, "device_id").to_string(),
            canonical_user_id: required_string(request, "canonical_user_id").to_string(),
            expected_old_ack_cursor: required_i64(request, "expected_old_ack_cursor"),
            acknowledged_cursor: required_i64(request, "acknowledged_cursor"),
        },
    )
    .expect("commit confirmed remote ACK");
    let (pull_cursor, ack_cursor): (i64, i64) = connection
        .query_row(
            "SELECT pull_cursor,ack_cursor FROM cloud_sync_state WHERE account_id=?1",
            [&account_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read durable cursors");
    let note_count: i64 = connection
        .query_row(
            "SELECT count(*) FROM notes WHERE project_id=?1",
            [required_string(request, "project_id")],
            |row| row.get(0),
        )
        .optional()
        .expect("read Note count")
        .unwrap_or(0);
    json!({
        "result": result,
        "pull_cursor": pull_cursor,
        "ack_cursor": ack_cursor,
        "note_count": note_count,
    })
}

fn local_note_edit(request: &Value) -> Value {
    let mut connection = open_database(&database_path(request)).expect("open local Note database");
    crate::update_note_in_connection(
        &mut connection,
        required_string(request, "project_id"),
        required_string(request, "entity_id"),
        request.get("patch").expect("local Note patch"),
        None,
    )
    .expect("update Note through production local mutation path");
    let (
        event_id,
        project_id,
        entity_id,
        operation,
        revision,
        updated_at,
        deleted_at,
        parent_event_id,
        generation,
        snapshot,
    ): (
        String,
        String,
        String,
        String,
        i64,
        String,
        Option<String>,
        Option<String>,
        i64,
        String,
    ) = connection
        .query_row(
            "SELECT event.event_id,event.project_id,event.entity_id,event.operation,
                    event.revision,event.updated_at,event.deleted_at,event.parent_event_id,
                    intent.mutation_generation,intent.snapshot_json
             FROM cloud_sync_outbox AS event JOIN cloud_sync_note_intents AS intent USING(event_id)
             WHERE event.account_id=?1 AND event.project_id=?2 AND event.entity_id=?3
               AND event.lifecycle='unsealed'",
            rusqlite::params![
                required_string(request, "local_account_id"),
                required_string(request, "project_id"),
                required_string(request, "entity_id")
            ],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                ))
            },
        )
        .expect("read production Note intent");
    json!({"event":{"event_id":event_id,"project_id":project_id,"entity_id":entity_id,
        "entity_type":"note","operation":operation,"revision":revision,
        "updated_at":updated_at,"deleted_at":deleted_at},
        "parent_event_id":parent_event_id,"mutation_generation":generation,
        "snapshot_json":snapshot,
        "note":serde_json::from_str::<Value>(&snapshot).expect("read Note snapshot")})
}

fn seal_local_note(request: &Value) -> Value {
    let object = request.get("object").expect("sealed local Note object");
    let mut connection =
        open_database(&database_path(request)).expect("open local Note sealing database");
    json!(commit_sealed_note_sync_event(
        &mut connection,
        &CommitSealedNoteSyncEventCommand {
            event_id: required_string(request, "event_id").into(),
            expected_mutation_generation: required_i64(request, "mutation_generation"),
            envelope: EncryptedNoteSyncEnvelope {
                crypto_version: required_i64(object, "crypto_version"),
                aad_version: required_i64(object, "aad_version"),
                nonce: required_string(object, "nonce").into(),
                ciphertext: required_string(object, "ciphertext").into(),
            },
        }
    )
    .expect("commit production sealed Note event"))
}

fn ordinary_receipt(request: &Value) -> Value {
    let mut connection =
        open_database(&database_path(request)).expect("open local Note receipt database");
    json!(commit_note_sync_upload_acceptance(
        &mut connection,
        &CommitNoteSyncUploadAcceptanceCommand {
            account_id: required_string(request, "local_account_id").into(),
            device_id: required_string(request, "device_id").into(),
            receipts: vec![NoteSyncUploadReceipt {
                event_id: required_string(request, "event_id").into(),
                server_sequence: required_i64(request, "server_sequence"),
                duplicate: request
                    .get("duplicate")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            }],
        }
    )
    .expect("commit production Note upload acceptance"))
}

fn prepare_apply_resolution(request: &Value) -> Value {
    let path = database_path(request);
    let command = PrepareNoteConflictResolutionCommand {
        account_id: required_string(request, "local_account_id").into(),
        canonical_user_id: required_string(request, "canonical_user_id").into(),
        device_id: required_string(request, "device_id").into(),
        canonical_payload: bytes(request, "canonical_payload"),
    };
    let mut connection = open_database(&path).expect("open resolution preparation database");
    let prepared = prepare_note_conflict_resolution(&mut connection, &command)
        .expect("prepare real Note conflict resolution");
    drop(connection);
    let mut privileged = open_privileged_remote_apply_database(&path)
        .expect("open protected resolution application database");
    let applied = apply_prepared_note_conflict_resolution(&mut privileged, &command)
        .expect("apply prepared Note conflict resolution");
    json!({"prepared":format!("{prepared:?}"),"applied":format!("{applied:?}")})
}

fn list_resolution_intents(request: &Value) -> Value {
    let mut connection =
        open_database(&database_path(request)).expect("open resolution intent database");
    json!(list_unsealed_note_resolution_intents(
        &mut connection,
        required_string(request, "local_account_id"),
        8,
    )
    .expect("list durable resolution intents"))
}

fn seal_resolution(request: &Value) -> Value {
    let object = request.get("object").expect("sealed resolution object");
    let mut connection =
        open_database(&database_path(request)).expect("open resolution seal database");
    let result = commit_sealed_note_resolution_event(
        &mut connection,
        &CommitSealedNoteResolutionCommand {
            event_id: required_string(request, "event_id").into(),
            account_id: required_string(request, "local_account_id").into(),
            canonical_user_id: required_string(request, "canonical_user_id").into(),
            device_id: required_string(request, "device_id").into(),
            project_id: required_string(request, "project_id").into(),
            entity_id: required_string(request, "entity_id").into(),
            expected_canonical_payload: required_string(request, "canonical_payload").into(),
            envelope: EncryptedNoteSyncEnvelope {
                crypto_version: required_i64(object, "crypto_version"),
                aad_version: required_i64(object, "aad_version"),
                nonce: required_string(object, "nonce").into(),
                ciphertext: required_string(object, "ciphertext").into(),
            },
        },
    )
    .expect("commit sealed resolution object");
    json!(result)
}

fn resolution_uploads(request: &Value) -> Value {
    let mut connection =
        open_database(&database_path(request)).expect("open resolution upload database");
    let ready = read_note_resolution_readiness(
        &mut connection,
        required_string(request, "local_account_id"),
        required_string(request, "event_id"),
    )
    .expect("read fresh resolution dependencies");
    let uploads = list_sealed_note_resolution_uploads(
        &mut connection,
        &ListSealedNoteResolutionUploadsCommand {
            account_id: required_string(request, "local_account_id").into(),
            device_id: required_string(request, "device_id").into(),
            canonical_user_id: required_string(request, "canonical_user_id").into(),
            limit: 8,
        },
    )
    .expect("list freshly ready resolution uploads");
    json!({"readiness":ready,"uploads":uploads})
}

fn resolution_receipt(request: &Value) -> Value {
    let mut connection =
        open_database(&database_path(request)).expect("open resolution receipt database");
    json!(commit_note_resolution_upload_acceptance(
        &mut connection,
        &CommitNoteResolutionUploadAcceptanceCommand {
            account_id: required_string(request, "local_account_id").into(),
            device_id: required_string(request, "device_id").into(),
            canonical_user_id: required_string(request, "canonical_user_id").into(),
            receipts: vec![NoteSyncUploadReceipt {
                event_id: required_string(request, "event_id").into(),
                server_sequence: required_i64(request, "server_sequence"),
                duplicate: request
                    .get("duplicate")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            }],
        }
    )
    .expect("commit exact resolution upload acceptance"))
}

fn receive_resolution(request: &Value) -> Value {
    let path = database_path(request);
    let account_id = required_string(request, "local_account_id").to_string();
    let device_id = required_string(request, "device_id").to_string();
    let canonical_user_id = required_string(request, "canonical_user_id").to_string();
    let item = request.get("item").expect("pulled resolution item");
    let event = item.get("event").expect("pulled resolution event");
    let object = item.get("object").expect("pulled resolution object");
    let event_id = required_string(event, "event_id").to_string();
    let server_sequence = required_i64(event, "server_sequence");
    let source_device_id = required_string(event, "device_id").to_string();
    let mut connection = open_database(&path).expect("open resolution inbox database");
    commit_note_sync_inbound_page(
        &mut connection,
        &CommitNoteSyncInboundPageCommand {
            account_id: account_id.clone(),
            device_id: device_id.clone(),
            canonical_user_id: canonical_user_id.clone(),
            expected_cursor: required_i64(request, "expected_cursor"),
            next_cursor: required_i64(request, "next_cursor"),
            has_more: false,
            items: vec![InboundNoteSyncItem {
                event_id: event_id.clone(),
                server_sequence,
                source_device_id: source_device_id.clone(),
                project_id: required_string(event, "project_id").into(),
                entity_id: required_string(event, "entity_id").into(),
                entity_type: required_string(event, "entity_type").into(),
                operation: required_string(event, "operation").into(),
                revision: required_i64(event, "revision"),
                updated_at: required_string(event, "updated_at").into(),
                deleted_at: event
                    .get("deleted_at")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                envelope: Some(EncryptedNoteSyncEnvelope {
                    crypto_version: required_i64(object, "crypto_version"),
                    aad_version: required_i64(object, "aad_version"),
                    nonce: required_string(object, "nonce").into(),
                    ciphertext: required_string(object, "ciphertext").into(),
                }),
            }],
        },
    )
    .expect("commit resolution inbox page");
    drop(connection);
    let opened = request
        .get("opened")
        .expect("authenticated opened resolution");
    let mut privileged = open_privileged_remote_apply_database(&path)
        .expect("open protected resolution apply database");
    let result = if source_device_id == device_id {
        serde_json::to_value(
            reconcile_verified_received_resolution_self_echo_ipc(
                &mut privileged,
                ReconcileVerifiedReceivedResolutionSelfEchoIpcCommand {
                    account_id: account_id.clone(),
                    canonical_user_id: canonical_user_id.clone(),
                    pulling_device_id: device_id.clone(),
                    event_id: event_id.clone(),
                    server_sequence,
                    source_device_id: source_device_id.clone(),
                    crypto_version: required_i64(opened, "crypto_version"),
                    aad_version: required_i64(opened, "aad_version"),
                    nonce: bytes(opened, "nonce"),
                    ciphertext: bytes(opened, "ciphertext"),
                    plaintext: bytes(opened, "plaintext"),
                },
            )
            .expect("reconcile resolution self echo"),
        )
        .unwrap()
    } else {
        serde_json::to_value(
            apply_verified_received_resolution_v2_ipc(
                &mut privileged,
                ApplyVerifiedReceivedResolutionV2IpcCommand {
                    account_id: account_id.clone(),
                    canonical_user_id: canonical_user_id.clone(),
                    pulling_device_id: device_id.clone(),
                    event_id: event_id.clone(),
                    server_sequence,
                    source_device_id: source_device_id.clone(),
                    crypto_version: required_i64(opened, "crypto_version"),
                    aad_version: required_i64(opened, "aad_version"),
                    nonce: bytes(opened, "nonce"),
                    ciphertext: bytes(opened, "ciphertext"),
                    plaintext: bytes(opened, "plaintext"),
                },
            )
            .expect("apply verified peer resolution"),
        )
        .unwrap()
    };
    drop(privileged);
    let mut connection = open_database(&path).expect("reopen resolution ACK database");
    let ack = prepare_note_sync_ack(
        &mut connection,
        &PrepareNoteSyncAckCommand {
            account_id,
            device_id,
            canonical_user_id,
        },
    )
    .expect("prepare resolution ACK candidate");
    json!({"result":result,"ack":ack})
}

fn cover_bridge(request: &Value) -> Value {
    let mut db = open_database(&database_path(request)).unwrap();
    let scope = crate::project_metadata_sync::MetadataScope {
        account_id: required_string(request, "local_account_id").into(),
        canonical_user_id: required_string(request, "canonical_user_id").into(),
        device_id: required_string(request, "device_id").into(),
    };
    match crate::project_cover_sync::command(
        &mut db,
        &scope,
        required_string(request, "project_id"),
        required_string(request, "step"),
        request.get("data").unwrap_or(&json!({})),
        "2026-10-06T00:00:00.000000Z",
    ) {
        Ok(value) => value,
        Err(error) => json!({"error":error.to_string()}),
    }
}

fn catalog_bridge(request: &Value) -> Value {
    use crate::account_catalog as catalog;
    let mut db = open_database(&database_path(request)).expect("catalog database");
    let scope = crate::project_metadata_sync::MetadataScope {
        account_id: required_string(request, "local_account_id").into(),
        canonical_user_id: required_string(request, "canonical_user_id").into(),
        device_id: required_string(request, "device_id").into(),
    };
    let now = "2026-10-02T00:00:00.000000Z";
    match required_string(request, "step") {
        "begin" => catalog::begin(&mut db, &scope, now).unwrap(),
        "authority" => catalog::authority(&db, &scope).unwrap(),
        "pending" => {
            json!(
                catalog::pending(&db, &scope, request["sealed"].as_bool().unwrap_or(false))
                    .unwrap()
            )
        }
        "normal" => json!(catalog::normal(
            &mut db,
            required_string(request, "entity_type"),
            required_string(request, "entity_id"),
            request["payload"].clone(),
            now
        )
        .unwrap()),
        "decide" => {
            let decision = serde_json::from_value(request["decision"].clone()).unwrap();
            json!(catalog::decide(&mut db, &scope, &decision, now).unwrap())
        }
        "seal" => {
            catalog::seal(
                &mut db,
                &scope,
                required_string(request, "event_id"),
                &bytes(request, "frame"),
                &bytes(request, "nonce"),
                &bytes(request, "ciphertext"),
            )
            .unwrap();
            json!(true)
        }
        "receipt" => {
            catalog::receipt(
                &mut db,
                &scope,
                required_string(request, "event_id"),
                required_i64(request, "server_sequence"),
            )
            .unwrap();
            json!(true)
        }
        "persist" => {
            let command = serde_json::from_value(request["command"].clone()).unwrap();
            json!(crate::note_sync::commit_mixed_sync_inbound_page(&mut db, &command).unwrap())
        }
        "apply" => {
            let opened = &request["opened"];
            json!(catalog::apply(
                &mut db,
                &scope,
                required_string(request, "event_id"),
                &bytes(opened, "frame"),
                &bytes(opened, "nonce"),
                &bytes(opened, "ciphertext")
            )
            .unwrap())
        }
        "ack" => serde_json::to_value(
            prepare_note_sync_ack(
                &mut db,
                &PrepareNoteSyncAckCommand {
                    account_id: scope.account_id,
                    device_id: scope.device_id,
                    canonical_user_id: scope.canonical_user_id,
                },
            )
            .unwrap(),
        )
        .unwrap(),
        _ => panic!("unknown catalog bridge step"),
    }
}

fn map_bridge(request: &Value) -> Value {
    use crate::{map_sync as maps, project_metadata_sync as metadata};
    let path = database_path(request);
    let mut db = open_database(&path).unwrap();
    let scope = metadata::MetadataScope {
        account_id: required_string(request, "local_account_id").into(),
        canonical_user_id: required_string(request, "canonical_user_id").into(),
        device_id: required_string(request, "device_id").into(),
    };
    metadata::assert_runtime_scope(
        &db,
        &scope.account_id,
        &scope.canonical_user_id,
        &scope.device_id,
    )
    .unwrap();
    let p = required_string(request, "project_id");
    let now = "2026-10-03T00:00:00.000000Z";
    match required_string(request, "step") {
        "begin" => maps::begin(&mut db, &scope, p, now).unwrap(),
        "view" => maps::view(&db, &scope, p).unwrap(),
        "combined" => {
            let repo = crate::project_repository::ProjectsRepository::new(&mut db);
            let project = repo.get_project(p).unwrap().unwrap();
            let stages = repo.list_stages(p).unwrap();
            let data = crate::compose_combined_map(&project, &stages).unwrap();
            drop(repo);
            json!({"data":data,"expected":maps::expected(&db,p,None,true).unwrap()})
        }
        "expected" => maps::expected(
            &db,
            p,
            request["stage_id"].as_str(),
            request["combined"].as_bool().unwrap_or(false),
        )
        .unwrap(),
        "pending" => {
            maps::advance(&mut db, &scope.account_id, now).unwrap();
            json!(maps::pending(
                &db,
                &scope.account_id,
                &scope.device_id,
                request["sealed"].as_bool().unwrap_or(false)
            )
            .unwrap())
        }
        "seal" => {
            maps::seal(
                &mut db,
                &scope.account_id,
                required_string(request, "event_id"),
                &bytes(request, "frame"),
                &bytes(request, "nonce"),
                &bytes(request, "ciphertext"),
            )
            .unwrap();
            json!(true)
        }
        "receipt" => {
            maps::receipt(
                &mut db,
                &scope.account_id,
                &scope.device_id,
                required_string(request, "event_id"),
                required_i64(request, "server_sequence"),
                request["duplicate"].as_bool().unwrap_or(false),
                now,
            )
            .unwrap();
            json!(true)
        }
        "edit" => {
            let command = crate::MapCommand {
                project_id: p.into(),
                stage_id: request["stage_id"].as_str().map(str::to_string),
                data: request["data"].clone(),
                expected_heads: request.get("expected").cloned(),
            };
            let data = crate::mindmap::normalize(command.data.clone()).unwrap();
            match crate::save_map_in_connection(&mut db, &command, &data) {
                Ok(()) => json!(true),
                Err(code) => json!({"error":code}),
            }
        }
        "note_edit" => json!(crate::update_note_in_connection(
            &mut db,
            p,
            required_string(request, "note_id"),
            &request["patch"],
            request["stage_id"].as_str()
        )
        .unwrap()),
        "note_delete" => {
            crate::delete_note_in_connection(
                &mut db,
                p,
                required_string(request, "note_id"),
                request["stage_id"].as_str(),
            )
            .unwrap();
            json!(true)
        }
        "note_order" => {
            let ids: Vec<String> = serde_json::from_value(request["note_ids"].clone()).unwrap();
            crate::reorder_notes_in_connection(&mut db, p, &ids, request["stage_id"].as_str())
                .unwrap();
            json!(true)
        }
        "decide" => {
            let d = serde_json::from_value(request["decision"].clone()).unwrap();
            match maps::decide(&mut db, &scope, &d, now) {
                Ok(id) => json!(id),
                Err(e) => json!({"error":e.to_string()}),
            }
        }
        "import" => {
            let d = serde_json::from_value(request["decision"].clone()).unwrap();
            drop(db);
            let mut db = crate::sqlite::open_privileged_remote_apply_database(&path).unwrap();
            json!(maps::import_choice(
                &mut db,
                &scope,
                &d,
                request["keep_local"].as_bool().unwrap_or(false),
                now
            )
            .unwrap())
        }
        "receive" => {
            let cmd: CommitNoteSyncInboundPageCommand =
                serde_json::from_value(request["command"].clone()).unwrap();
            crate::note_sync::commit_v3_sync_inbound_page(&mut db, &cmd).unwrap();
            let opened = &request["opened"];
            drop(db);
            let mut db = crate::sqlite::open_privileged_remote_apply_database(&path).unwrap();
            match maps::apply(
                &mut db,
                &scope,
                &bytes(opened, "frame"),
                &bytes(opened, "nonce"),
                &bytes(opened, "ciphertext"),
            ) {
                Ok(result) => json!(result),
                Err(e) => json!({"error":e.to_string()}),
            }
        }
        "apply" => {
            let opened = &request["opened"];
            drop(db);
            let mut db = crate::sqlite::open_privileged_remote_apply_database(&path).unwrap();
            json!(maps::apply(
                &mut db,
                &scope,
                &bytes(opened, "frame"),
                &bytes(opened, "nonce"),
                &bytes(opened, "ciphertext")
            )
            .unwrap())
        }
        "ack" => json!(prepare_note_sync_ack(
            &mut db,
            &PrepareNoteSyncAckCommand {
                account_id: scope.account_id,
                device_id: scope.device_id,
                canonical_user_id: scope.canonical_user_id
            }
        )
        .unwrap()),
        _ => panic!("unknown map bridge step"),
    }
}

fn progress_bridge(request: &Value) -> Value {
    use crate::{progress_sync as progress, project_metadata_sync as metadata};
    let path = database_path(request);
    let mut db = open_database(&path).unwrap();
    let scope = metadata::MetadataScope {
        account_id: required_string(request, "local_account_id").into(),
        canonical_user_id: required_string(request, "canonical_user_id").into(),
        device_id: required_string(request, "device_id").into(),
    };
    metadata::assert_runtime_scope(
        &db,
        &scope.account_id,
        &scope.canonical_user_id,
        &scope.device_id,
    )
    .unwrap();
    let p = required_string(request, "project_id");
    let now = "2026-10-03T00:00:00.000000Z";
    let result = (|| -> Result<Value, String> {
        Ok(match required_string(request, "step") {
            "writing_day" => json!(crate::streaks::logical_writing_day(&db)?),
            "begin" => {
                progress::capture(&mut db, &scope, p, now)?;
                progress::view(&db, &scope, p)?
            }
            "view" => progress::view(&db, &scope, p)?,
            "pending" => {
                progress::continue_capture(&mut db, &scope.account_id, now)?;
                json!(progress::pending(
                    &db,
                    &scope.account_id,
                    &scope.device_id,
                    request["sealed"].as_bool().unwrap_or(false)
                )?)
            }
            "seal" => {
                progress::seal(
                    &mut db,
                    &scope.account_id,
                    &scope.device_id,
                    required_string(request, "event_id"),
                    &bytes(request, "frame"),
                    &bytes(request, "nonce"),
                    &bytes(request, "ciphertext"),
                )?;
                json!(true)
            }
            "receipt" => {
                progress::receipt(
                    &mut db,
                    &scope.account_id,
                    &scope.device_id,
                    required_string(request, "event_id"),
                    required_i64(request, "server_sequence"),
                    request["duplicate"].as_bool().unwrap_or(false),
                    now,
                )?;
                json!(true)
            }
            "manual" => {
                drop(db);
                std::env::set_var("NFPROGRESS_DATA_DIR", path.parent().unwrap());
                let expected: Vec<String> = serde_json::from_value(request["expected"].clone())
                    .map_err(|_| "test_expected".to_string())?;
                crate::add_progress_sqlite(
                    p.into(),
                    request["stage_id"].as_str().map(str::to_string),
                    request["total"].as_f64().ok_or("test_total")?,
                    Some(expected),
                )?
            }
            "document_progress" => {
                drop(db);
                std::env::set_var("NFPROGRESS_DATA_DIR", path.parent().unwrap());
                crate::documents::record_document_progress(
                    crate::documents::DocumentProgressCommand {
                        project_id: p.into(),
                        stage_id: request["stage_id"].as_str().map(str::to_string),
                        content: None,
                        expected_heads: None,
                    },
                )?
            }
            "external_sync" => {
                drop(db);
                std::env::set_var("NFPROGRESS_DATA_DIR", path.parent().unwrap());
                serde_json::to_value(crate::documents::run_sync(
                    crate::documents::SyncScopeCommand {
                        project_id: p.into(),
                        stage_id: request["stage_id"].as_str().map(str::to_string),
                    },
                )?)
                .map_err(|_| "test_sync_result")?
            }
            "append" => {
                let tx = db
                    .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                    .map_err(|_| "test_storage".to_string())?;
                let expected: Vec<String> = serde_json::from_value(request["expected"].clone())
                    .map_err(|_| "test_expected".to_string())?;
                let v = progress::normal(
                    &tx,
                    p,
                    request["stage_id"].as_str(),
                    &request["entry"],
                    Some(&expected),
                    now,
                )?;
                tx.commit().map_err(|_| "test_storage".to_string())?;
                json!(v)
            }
            "decide" => json!(progress::decide(
                &mut db,
                &scope,
                &serde_json::from_value(request["decision"].clone())
                    .map_err(|_| "test_decision".to_string())?,
                now
            )?),
            "receive" => {
                crate::note_sync::commit_v3_sync_inbound_page(
                    &mut db,
                    &serde_json::from_value(request["command"].clone()).unwrap(),
                )
                .map_err(|e| e.to_string())?;
                drop(db);
                let mut privileged =
                    open_privileged_remote_apply_database(&path).map_err(|e| e.to_string())?;
                json!(progress::apply(
                    &mut privileged,
                    &scope,
                    &bytes(&request["opened"], "frame"),
                    &bytes(&request["opened"], "nonce"),
                    &bytes(&request["opened"], "ciphertext")
                )?)
            }
            "apply" => {
                drop(db);
                let mut privileged =
                    open_privileged_remote_apply_database(&path).map_err(|e| e.to_string())?;
                json!(progress::apply(
                    &mut privileged,
                    &scope,
                    &bytes(request, "frame"),
                    &bytes(request, "nonce"),
                    &bytes(request, "ciphertext")
                )?)
            }
            "rebuild" => {
                drop(db);
                let mut privileged =
                    open_privileged_remote_apply_database(&path).map_err(|e| e.to_string())?;
                progress::rebuild(&mut privileged, &scope, p, request["stage_id"].as_str())?;
                json!(true)
            }
            "ack" => json!(prepare_note_sync_ack(
                &mut db,
                &PrepareNoteSyncAckCommand {
                    account_id: scope.account_id,
                    device_id: scope.device_id,
                    canonical_user_id: scope.canonical_user_id
                }
            )
            .map_err(|e| e.to_string())?),
            _ => return Err("test_unknown_progress_step".into()),
        })
    })();
    match result {
        Ok(v) => v,
        Err(error) => json!({"error":error}),
    }
}

fn game_bridge(request: &Value) -> Value {
    let path = database_path(request);
    let mut db = open_database(&path).unwrap();
    // Ordinary completion refreshes Progress through the app's profile path.
    // Pin that path to this isolated device, as the ordinary Progress bridge does.
    std::env::set_var("NFPROGRESS_DATA_DIR", path.parent().unwrap());
    let scope = crate::project_metadata_sync::MetadataScope {
        account_id: required_string(request, "local_account_id").into(),
        canonical_user_id: required_string(request, "canonical_user_id").into(),
        device_id: required_string(request, "device_id").into(),
    };
    let result = (|| -> Result<Value, String> {
        match required_string(request, "step") {
            "init" => {
                let source = if request["source"].is_object() {
                    request["source"].clone()
                } else {
                    json!({"gamer":serde_json::from_str::<Value>(include_str!("../../src/cloud/__fixtures__/gameLegacyDefaultsV1.json")).unwrap()})
                };
                db.execute(
                    "UPDATE storage_ownership SET owner='sqlite' WHERE subsystem='game'",
                    [],
                )
                .map_err(|e| e.to_string())?;
                db.execute("INSERT INTO game_state(id,schema_version,payload_json,updated_at) VALUES(1,2,?1,'2026-10-04T00:00:00.000000Z') ON CONFLICT(id) DO UPDATE SET payload_json=excluded.payload_json",[source.to_string()]).map_err(|e|e.to_string())?;
                Ok(json!(true))
            }
            "process" => Ok(json!(crate::game::process_pending_events(&mut db, 100)?)),
            "loss" => {
                let tx = db.transaction().map_err(|e| e.to_string())?;
                let result = crate::game::refresh_global_loss(
                    &tx,
                    required_string(request, "writing_day"),
                    required_string(request, "now"),
                )?;
                tx.commit().map_err(|e| e.to_string())?;
                Ok(json!(result))
            }
            "state" => {
                let raw: String = db
                    .query_row("SELECT payload_json FROM game_state WHERE id=1", [], |r| {
                        r.get(0)
                    })
                    .map_err(|e| e.to_string())?;
                serde_json::from_str(&raw).map_err(|_| "test_game_json".into())
            }
            "complete" => {
                let p = required_string(request, "project_id").to_string();
                if let Some(s) = request["stage_id"].as_str() {
                    crate::complete_stage_sqlite(
                        &mut db,
                        crate::StageIdCommand {
                            project_id: p,
                            stage_id: s.into(),
                        },
                    )
                } else {
                    crate::complete_project_sqlite(
                        &mut db,
                        crate::ProjectIdCommand { project_id: p },
                    )
                }
            }
            "inventory" => serde_json::to_value(
                crate::game::GameApplicationService::inventory_sqlite(
                    &mut db,
                    required_string(request, "category").into(),
                    required_string(request, "item_id").into(),
                    required_i64(request, "count"),
                    required_string(request, "operation"),
                )
                .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string()),
            "receive" => {
                crate::note_sync::commit_mixed_sync_inbound_page(
                    &mut db,
                    &serde_json::from_value(request["command"].clone())
                        .map_err(|_| "test_game_inbox")?,
                )
                .map_err(|e| e.to_string())?;
                Ok(json!(true))
            }
            "note_apply" | "content_note_apply" => {
                let row = &request["event"];
                let opened = &request["opened"];
                drop(db);
                let mut privileged =
                    open_privileged_remote_apply_database(&path).map_err(|e| e.to_string())?;
                let command = crate::note_sync::ApplyVerifiedReceivedNoteIpcCommand {
                    account_id: scope.account_id,
                    canonical_user_id: scope.canonical_user_id,
                    pulling_device_id: scope.device_id,
                    event_id: required_string(row, "event_id").into(),
                    server_sequence: required_i64(row, "server_sequence"),
                    source_device_id: required_string(row, "device_id").into(),
                    crypto_version: if required_string(request, "step") == "content_note_apply" {
                        1
                    } else {
                        required_i64(opened, "crypto_version")
                    },
                    aad_version: if required_string(request, "step") == "content_note_apply" {
                        1
                    } else {
                        required_i64(opened, "aad_version")
                    },
                    nonce: bytes(opened, "nonce"),
                    ciphertext: bytes(opened, "ciphertext"),
                    plaintext: bytes(
                        opened,
                        if required_string(request, "step") == "content_note_apply" {
                            "frame"
                        } else {
                            "plaintext"
                        },
                    ),
                };
                let result = if required_string(request, "step") == "content_note_apply" {
                    json!(crate::note_sync::apply_verified_received_content_note_ipc(
                        &mut privileged,
                        command
                    )
                    .map_err(|e| e.to_string())?)
                } else {
                    json!(crate::note_sync::apply_verified_received_note_ipc(
                        &mut privileged,
                        command
                    )
                    .map_err(|e| e.to_string())?)
                };
                Ok(result)
            }
            "command" => {
                drop(db);
                std::env::set_var("NFPROGRESS_DATA_DIR", path.parent().unwrap());
                crate::game_sync_command(
                    scope,
                    serde_json::from_value(request["request"].clone())
                        .map_err(|_| "test_game_command")?,
                )
            }
            _ => Err("test_game_step".into()),
        }
    })();
    match result {
        Ok(v) => v,
        Err(error) => json!({"error":error}),
    }
}

fn document_bridge(request: &Value) -> Value {
    use crate::{document_sync as docs, project_metadata_sync as metadata};
    let path = database_path(request);
    let mut db = open_database(&path).unwrap();
    let scope = metadata::MetadataScope {
        account_id: required_string(request, "local_account_id").into(),
        canonical_user_id: required_string(request, "canonical_user_id").into(),
        device_id: required_string(request, "device_id").into(),
    };
    metadata::assert_runtime_scope(
        &db,
        &scope.account_id,
        &scope.canonical_user_id,
        &scope.device_id,
    )
    .unwrap();
    let p = required_string(request, "project_id");
    let now = "2026-10-03T00:00:00.000000Z";
    let id = request["document_id"].as_str().unwrap_or("");
    match required_string(request, "step") {
        "init" => {
            crate::documents::migrate_legacy_documents(&mut db, path.parent().unwrap()).unwrap();
            json!(true)
        }
        "begin" => docs::begin(&mut db, &scope, p, now).unwrap(),
        "view" => docs::view(&db, &scope, p).unwrap(),
        "expected" => docs::expected(&db, p, id).unwrap(),
        "pending" => {
            docs::advance(&mut db, &scope.account_id, now).unwrap();
            json!(docs::pending(
                &db,
                &scope.account_id,
                &scope.device_id,
                request["sealed"].as_bool().unwrap_or(false)
            )
            .unwrap())
        }
        "seal" => {
            docs::seal(
                &mut db,
                &scope.account_id,
                required_string(request, "event_id"),
                &bytes(request, "frame"),
                &bytes(request, "nonce"),
                &bytes(request, "ciphertext"),
            )
            .unwrap();
            json!(true)
        }
        "receipt" => {
            docs::receipt(
                &mut db,
                &scope.account_id,
                &scope.device_id,
                required_string(request, "event_id"),
                required_i64(request, "server_sequence"),
                request["duplicate"].as_bool().unwrap_or(false),
                now,
            )
            .unwrap();
            json!(true)
        }
        "edit" => {
            let tx = db.transaction().unwrap();
            let result = docs::normal(
                &tx,
                p,
                id,
                request["document"].clone(),
                request.get("expected"),
                now,
            );
            match result {
                Ok(v) => {
                    tx.commit().unwrap();
                    json!(v)
                }
                Err(e) => json!({"error":e.to_string()}),
            }
        }
        "configure_source" | "prepare_progress" | "confirm_progress" | "refresh_progress"
        | "background" => {
            drop(db);
            std::env::set_var("NFPROGRESS_DATA_DIR", path.parent().unwrap());
            let c = crate::documents::SyncScopeCommand {
                project_id: p.into(),
                stage_id: request["stage_id"].as_str().map(str::to_string),
            };
            let result:Result<Value,String>=match required_string(request,"step"){
 "configure_source"=>crate::documents::configure_sync(serde_json::from_value(json!({"projectId":p,"stageId":request["stage_id"],"type":request["source_type"],"path":request["source_path"],"itemId":request["item_id"]})).unwrap()).map(|v|serde_json::to_value(v).unwrap()),
 "prepare_progress"=>crate::documents::run_sync(c).map(|v|serde_json::to_value(v).unwrap()),
 "confirm_progress"=>crate::documents::confirm_external_progress(c).map(|v|serde_json::to_value(v).unwrap()),
 "refresh_progress"=>crate::documents::refresh_external_progress(c).map(|v|serde_json::to_value(v).unwrap()),
 _=>crate::documents::run_all_sync().map(|v|serde_json::to_value(v).unwrap())};
            match result {
                Ok(v) => v,
                Err(e) => json!({"error":e}),
            }
        }
        "reattach" | "resolve_binding" | "poll_binding" => {
            drop(db);
            std::env::set_var("NFPROGRESS_DATA_DIR", path.parent().unwrap());
            let result=match required_string(request,"step"){
 "reattach"=>crate::documents::bind_document_file(serde_json::from_value(json!({"projectId":p,"stageId":request["stage_id"],"path":request["path"]})).unwrap()),
 "resolve_binding"=>crate::documents::resolve_external_binding(serde_json::from_value(json!({"projectId":p,"stageId":request["stage_id"],"choice":request["choice"]})).unwrap()),
 _=>crate::documents::poll_external_binding(serde_json::from_value(json!({"projectId":p,"stageId":request["stage_id"]})).unwrap())};
            match result {
                Ok(v) => v,
                Err(e) => json!({"error":e}),
            }
        }
        "save" | "external" | "rename" => {
            drop(db);
            std::env::set_var("NFPROGRESS_DATA_DIR", path.parent().unwrap());
            let document_scope = json!({"projectId":p,"stageId":request["stage_id"],"expectedHeads":request["expected"]});
            let mut command = document_scope;
            command["content"] = request["content"].clone();
            command["title"] = request["title"].clone();
            command["sourceHash"] = request["source_hash"].clone();
            let result = match required_string(request, "step") {
                "save" => {
                    command.as_object_mut().unwrap().remove("title");
                    command.as_object_mut().unwrap().remove("sourceHash");
                    crate::documents::save_document(serde_json::from_value(command).unwrap())
                }
                "rename" => {
                    command.as_object_mut().unwrap().remove("content");
                    command.as_object_mut().unwrap().remove("sourceHash");
                    crate::documents::rename_document(serde_json::from_value(command).unwrap())
                }
                _ => {
                    command.as_object_mut().unwrap().remove("title");
                    crate::documents::accept_external(serde_json::from_value(command).unwrap())
                }
            };
            match result {
                Ok(v) => v,
                Err(e) => json!({"error":e}),
            }
        }
        "move" => match docs::assert_project_scope(&db, &scope, p).and_then(|_| {
            docs::move_scope(
                &mut db,
                p,
                id,
                request["stage_id"].as_str(),
                &request["expected"],
                now,
            )
        }) {
            Ok(()) => json!(true),
            Err(e) => json!({"error":e.to_string()}),
        },
        "delete" => {
            let tx = db.transaction().unwrap();
            match docs::normal(&tx, p, id, Value::Null, request.get("expected"), now) {
                Ok(v) => {
                    tx.commit().unwrap();
                    json!(v)
                }
                Err(e) => json!({"error":e.to_string()}),
            }
        }
        "decide" => match docs::decide(
            &mut db,
            &scope,
            &serde_json::from_value(request["decision"].clone()).unwrap(),
            now,
        ) {
            Ok(v) => json!(v),
            Err(e) => json!({"error":e.to_string()}),
        },
        "persist" => {
            json!(crate::note_sync::commit_v3_sync_inbound_page(
                &mut db,
                &serde_json::from_value(request["command"].clone()).unwrap()
            )
            .unwrap())
        }
        "receive" => {
            crate::note_sync::commit_v3_sync_inbound_page(
                &mut db,
                &serde_json::from_value(request["command"].clone()).unwrap(),
            )
            .unwrap();
            drop(db);
            let mut privileged = open_privileged_remote_apply_database(&path).unwrap();
            json!(docs::apply(
                &mut privileged,
                &scope,
                &bytes(&request["opened"], "frame"),
                &bytes(&request["opened"], "nonce"),
                &bytes(&request["opened"], "ciphertext")
            )
            .unwrap())
        }
        "apply" => {
            drop(db);
            let mut privileged = open_privileged_remote_apply_database(&path).unwrap();
            match docs::apply(
                &mut privileged,
                &scope,
                &bytes(request, "frame"),
                &bytes(request, "nonce"),
                &bytes(request, "ciphertext"),
            ) {
                Ok(v) => json!(v),
                Err(e) => json!({"error":e.to_string()}),
            }
        }
        "ack" => json!(prepare_note_sync_ack(
            &mut db,
            &PrepareNoteSyncAckCommand {
                account_id: scope.account_id,
                device_id: scope.device_id,
                canonical_user_id: scope.canonical_user_id
            }
        )
        .unwrap()),
        _ => panic!("unknown document bridge step"),
    }
}

fn content_note_bridge(request: &Value) -> Value {
    use crate::{content_note_writer as writer, project_metadata_sync as metadata};
    let path = database_path(request);
    let mut db = open_database(&path).unwrap();
    let scope = metadata::MetadataScope {
        account_id: required_string(request, "local_account_id").into(),
        canonical_user_id: required_string(request, "canonical_user_id").into(),
        device_id: required_string(request, "device_id").into(),
    };
    let p = required_string(request, "project_id");
    match required_string(request, "step") {
        "begin" => writer::begin(&mut db, &scope, p, "2026-10-02T00:00:00.000000Z").unwrap(),
        "view" => writer::view(&db, &scope, p).unwrap(),
        "conflicts" => json!(writer::conflicts(&db, &scope, p).unwrap()),
        "import_choice" => {
            let command = writer::prepare_import_choice(
                &mut db,
                &scope,
                p,
                &request["decision"],
                required_string(request, "selected"),
                required_string(request, "now"),
            )
            .unwrap();
            drop(db);
            let mut privileged =
                crate::sqlite::open_privileged_remote_apply_database(&path).unwrap();
            json!(crate::note_sync::apply_verified_received_content_note_ipc(
                &mut privileged,
                command
            )
            .unwrap())
        }
        "choose" => {
            let command = writer::prepare_choice(
                &mut db,
                &scope,
                p,
                &request["decision"],
                required_string(request, "selected"),
                required_string(request, "now"),
            );
            match command {
                Err(code) => json!({"error":code}),
                Ok(command) => {
                    drop(db);
                    let mut privileged =
                        crate::sqlite::open_privileged_remote_apply_database(&path).unwrap();
                    json!(format!(
                        "{:?}",
                        crate::note_sync::apply_prepared_note_conflict_resolution(
                            &mut privileged,
                            &command
                        )
                        .unwrap()
                    ))
                }
            }
        }
        "pending" => {
            json!(
                writer::pending(&db, &scope, request["sealed"].as_bool().unwrap_or(false)).unwrap()
            )
        }
        "seal" => {
            writer::seal(
                &mut db,
                &scope,
                required_string(request, "event_id"),
                &bytes(request, "frame"),
                serde_json::from_value(request["envelope"].clone()).unwrap(),
            )
            .unwrap();
            json!(true)
        }
        "receipt" => {
            writer::receipt(
                &mut db,
                &scope,
                required_string(request, "event_id"),
                request["server_sequence"].as_i64().unwrap(),
                request["duplicate"].as_bool().unwrap(),
            )
            .unwrap();
            json!(true)
        }
        "create" => json!(crate::create_note_with_format_in_connection(
            &mut db,
            p,
            request["stage_id"].as_str(),
            required_string(request, "note_id"),
            required_string(request, "content_format")
        )
        .unwrap()),
        "edit" => {
            let stage = request["stage_id"].as_str();
            json!(crate::update_note_in_connection(
                &mut db,
                p,
                required_string(request, "note_id"),
                &request["patch"],
                stage
            )
            .unwrap())
        }
        "delete" => {
            crate::delete_note_in_connection(
                &mut db,
                p,
                required_string(request, "note_id"),
                request["stage_id"].as_str(),
            )
            .unwrap();
            json!(true)
        }
        "receive" => {
            let cmd: CommitNoteSyncInboundPageCommand =
                serde_json::from_value(request["command"].clone()).unwrap();
            crate::note_sync::commit_v3_sync_inbound_page(&mut db, &cmd).unwrap();
            let c = &request["command"]["items"][0];
            let opened = &request["opened"];
            let command = crate::note_sync::ApplyVerifiedReceivedNoteIpcCommand {
                account_id: scope.account_id,
                canonical_user_id: scope.canonical_user_id,
                pulling_device_id: scope.device_id,
                event_id: required_string(c, "event_id").into(),
                server_sequence: c["server_sequence"].as_i64().unwrap(),
                source_device_id: required_string(c, "source_device_id").into(),
                crypto_version: 1,
                aad_version: 1,
                nonce: bytes(opened, "nonce"),
                ciphertext: bytes(opened, "ciphertext"),
                plaintext: bytes(opened, "frame"),
            };
            drop(db);
            let mut privileged =
                crate::sqlite::open_privileged_remote_apply_database(&path).unwrap();
            json!(crate::note_sync::apply_verified_received_content_note_ipc(
                &mut privileged,
                command
            )
            .unwrap())
        }
        "ack" => json!(prepare_note_sync_ack(
            &mut db,
            &PrepareNoteSyncAckCommand {
                account_id: scope.account_id,
                canonical_user_id: scope.canonical_user_id,
                device_id: scope.device_id
            }
        )
        .unwrap()),
        _ => panic!("unknown content Note bridge step"),
    }
}

fn structural_bridge(request: &Value) -> Value {
    use crate::stage_sync as stages;
    let mut db = open_database(&database_path(request)).expect("structural database");
    let account = required_string(request, "local_account_id");
    let project = required_string(request, "project_id");
    crate::project_metadata_sync::assert_runtime_scope(
        &db,
        account,
        required_string(request, "canonical_user_id"),
        required_string(request, "device_id"),
    )
    .unwrap();
    match required_string(request, "step") {
        "persist" => {
            let cmd: CommitNoteSyncInboundPageCommand =
                serde_json::from_value(request["command"].clone()).unwrap();
            json!(crate::note_sync::commit_v3_sync_inbound_page(&mut db, &cmd).unwrap())
        }
        "apply" => {
            let opened = &request["opened"];
            json!(stages::apply_received(
                &mut db,
                account,
                required_string(request, "event_id"),
                &bytes(opened, "frame"),
                &bytes(opened, "nonce"),
                &bytes(opened, "ciphertext")
            )
            .unwrap())
        }
        "begin" => {
            json!(stages::begin(&mut db, account, project, "2026-10-01T00:00:00.000000Z").unwrap())
        }
        "authority" => json!(stages::authority(&db, account, project).unwrap()),
        "advance" => {
            stages::advance(&mut db, account, project).unwrap();
            json!(true)
        }
        "pending" => json!(stages::pending_events(
            &db,
            account,
            required_string(request, "device_id"),
            request["sealed"].as_bool().unwrap_or(false)
        )
        .unwrap()),
        "decide" => {
            let d: stages::Decision = serde_json::from_value(request["decision"].clone()).unwrap();
            json!(
                stages::decide(&mut db, account, project, &d, "2026-10-01T00:00:00.000000Z")
                    .unwrap()
            )
        }
        "edit" => json!(stages::normal_edit(
            &mut db,
            project,
            required_string(request, "stage_id"),
            request["proposed"].clone(),
            "2026-10-01T00:00:00.000000Z"
        )
        .unwrap()),
        "order" => {
            let ids: Vec<String> = serde_json::from_value(request["stage_ids"].clone()).unwrap();
            json!(
                stages::normal_order(&mut db, project, &ids, "2026-10-01T00:00:00.000000Z")
                    .unwrap()
            )
        }
        "receipt" => {
            stages::commit_receipt(
                &mut db,
                account,
                required_string(request, "device_id"),
                required_string(request, "event_id"),
                request["server_sequence"].as_i64().unwrap(),
                request["duplicate"].as_bool().unwrap(),
                "2026-10-01T00:00:00.000000Z",
            )
            .unwrap();
            json!(true)
        }
        "capture" => json!(stages::capture_candidate(
            &mut db,
            account,
            project,
            required_string(request, "stage_id"),
            "2026-10-01T00:00:00.000000Z"
        )
        .unwrap()),
        "prepare" => {
            let e: stages::Event = serde_json::from_value(request["event"].clone()).unwrap();
            let expected: Vec<String> =
                serde_json::from_value(request["expected_tips"].clone()).unwrap();
            stages::prepare(&mut db, account, &e, &expected).unwrap();
            json!(stages::frame(&e).unwrap())
        }
        "seal" => {
            stages::seal(
                &mut db,
                account,
                required_string(request, "event_id"),
                &bytes(request, "frame"),
                &bytes(request, "nonce"),
                &bytes(request, "ciphertext"),
            )
            .unwrap();
            json!(true)
        }
        "receive" => {
            let cmd: CommitNoteSyncInboundPageCommand =
                serde_json::from_value(request["command"].clone()).unwrap();
            crate::note_sync::commit_v3_sync_inbound_page(&mut db, &cmd).unwrap();
            let opened = &request["opened"];
            json!(stages::apply_received(
                &mut db,
                account,
                required_string(&request["command"]["items"][0], "event_id"),
                &bytes(opened, "frame"),
                &bytes(opened, "nonce"),
                &bytes(opened, "ciphertext")
            )
            .unwrap())
        }
        "retry" => json!(stages::retry(&mut db, account, 8).unwrap()),
        "ack" => serde_json::to_value(
            prepare_note_sync_ack(
                &mut db,
                &PrepareNoteSyncAckCommand {
                    account_id: account.into(),
                    device_id: required_string(request, "device_id").into(),
                    canonical_user_id: required_string(request, "canonical_user_id").into(),
                },
            )
            .unwrap(),
        )
        .unwrap(),
        "read" => {
            let mut q=db.prepare("SELECT entity_type,entity_id,event_id,state,blocker FROM cloud_sync_structural_events WHERE account_id=?1 ORDER BY event_id").unwrap();
            let rows=q.query_map([account],|r|Ok(json!({"type":r.get::<_,String>(0)?,"id":r.get::<_,String>(1)?,"event_id":r.get::<_,String>(2)?,"state":r.get::<_,String>(3)?,"blocker":r.get::<_,Option<String>>(4)?}))).unwrap().collect::<Result<Vec<_>,_>>().unwrap();
            json!(rows)
        }
        _ => panic!("unsupported structural bridge step"),
    }
}

fn metadata_authority_bridge(request: &Value) -> Value {
    use crate::project_metadata_sync as metadata;
    let mut db = open_database(&database_path(request)).expect("metadata device database");
    let account = required_string(request, "local_account_id");
    let user = required_string(request, "canonical_user_id");
    let device = required_string(request, "device_id");
    let project = required_string(request, "project_id");
    let now = "2026-09-29T00:00:00.000000Z";
    metadata::assert_runtime_scope(&db, account, user, device).unwrap();
    if required_string(request, "step") == "import_read" {
        return serde_json::to_value(
            metadata::read_metadata_import(
                &mut db,
                account,
                project,
                required_string(request, "bootstrap_id"),
            )
            .unwrap(),
        )
        .unwrap();
    }
    if required_string(request, "step") == "import_page" {
        let page = serde_json::from_value::<metadata::MetadataImportPage>(request["page"].clone())
            .unwrap();
        return serde_json::to_value(
            metadata::commit_metadata_import_page(
                &mut db,
                account,
                user,
                project,
                required_string(request, "bootstrap_id"),
                &page,
            )
            .unwrap(),
        )
        .unwrap();
    }
    let result = match required_string(request, "step") {
        "begin" => {
            let candidate =
                metadata::capture_legacy_candidate(&mut db, account, project, now).unwrap();
            json!(metadata::prepare_metadata_genesis(&mut db, account, &candidate, now).unwrap())
        }
        "adopt" => {
            let view = metadata::authority_view(&db, account, project).unwrap();
            serde_json::to_value(
                metadata::adopt_authenticated_metadata(
                    &mut db,
                    account,
                    project,
                    view.head_event_id.as_deref().unwrap(),
                    view.local.as_ref().unwrap(),
                    now,
                )
                .unwrap(),
            )
            .unwrap()
        }
        "change" => {
            let view = metadata::authority_view(&db, account, project).unwrap();
            let tips = view
                .branches
                .iter()
                .map(|b| b.event_id.clone())
                .collect::<Vec<_>>();
            let id = metadata::prepare_authoritative_change(
                &mut db,
                account,
                project,
                device,
                required_string(request, "kind"),
                request.get("selected").and_then(Value::as_str),
                request.get("proposed"),
                view.local.as_ref().unwrap(),
                &tips,
                now,
            )
            .unwrap();
            json!(id)
        }
        "normal_edit" => {
            let proposed = request.get("proposed").unwrap();
            json!(metadata::capture_normal_edit(&mut db, project, proposed, now).unwrap())
        }
        "seal" => {
            metadata::commit_sealed_genesis(
                &mut db,
                account,
                device,
                required_string(request, "event_id"),
                &bytes(request, "nonce"),
                &bytes(request, "ciphertext"),
            )
            .unwrap();
            json!(true)
        }
        "receive" => {
            let command: CommitNoteSyncInboundPageCommand =
                serde_json::from_value(request["command"].clone()).unwrap();
            crate::note_sync::commit_v3_sync_inbound_page(&mut db, &command).unwrap();
            let opened = &request["opened"];
            json!(metadata::preserve_authenticated_event_checked(
                &mut db,
                account,
                project,
                &bytes(opened, "plaintext"),
                now,
                Some((&bytes(opened, "nonce"), &bytes(opened, "ciphertext")))
            )
            .unwrap())
        }
        "apply" => {
            let opened = &request["opened"];
            json!(metadata::preserve_authenticated_event_checked(
                &mut db,
                account,
                project,
                &bytes(opened, "plaintext"),
                now,
                Some((&bytes(opened, "nonce"), &bytes(opened, "ciphertext")))
            )
            .unwrap())
        }
        "read" => Value::Null,
        _ => panic!("unsupported metadata bridge step"),
    };
    let view = metadata::authority_view(&db, account, project).unwrap();
    let events = metadata::unsealed_genesis(&db, account, device).unwrap();
    let payload: String = db
        .query_row(
            "SELECT payload_json FROM projects WHERE id=?1",
            [project],
            |r| r.get(0),
        )
        .unwrap();
    json!({"result":result,"view":view,"unsealed":events,"payload":serde_json::from_str::<Value>(&payload).unwrap()})
}

#[test]
#[ignore = "invoked only by tests/test_c15_headless_cross_runtime.py"]
fn c15_headless_native_bridge() {
    let request_path = std::env::var("NFPROGRESS_C15_NATIVE_BRIDGE_REQUEST")
        .expect("NFPROGRESS_C15_NATIVE_BRIDGE_REQUEST is required");
    let request: Value =
        serde_json::from_slice(&std::fs::read(request_path).expect("read bridge request"))
            .expect("parse bridge request");
    let response = match required_string(&request, "action") {
        "cover" => cover_bridge(&request),
        "metadata_authority" => metadata_authority_bridge(&request),
        "structural" => structural_bridge(&request),
        "content_note" => content_note_bridge(&request),
        "document" => document_bridge(&request),
        "progress" => progress_bridge(&request),
        "game" => game_bridge(&request),
        "map" => map_bridge(&request),
        "catalog" => catalog_bridge(&request),
        "provision" => provision(&request),
        "bootstrap_prepare" => bootstrap_prepare(&request),
        "bootstrap_prepare_capture" => bootstrap_prepare_capture(&request),
        "bootstrap_commit_upload" => bootstrap_commit_upload(&request),
        "bootstrap_confirm_active" => bootstrap_confirm_active(&request),
        "bootstrap_import" => bootstrap_import(&request),
        "bootstrap_mark_ready" => bootstrap_mark_ready(&request),
        "receive_apply_prepare" => receive_apply_prepare(&request),
        "local_note_edit" => local_note_edit(&request),
        "seal_local_note" => seal_local_note(&request),
        "ordinary_receipt" => ordinary_receipt(&request),
        "prepare_apply_resolution" => prepare_apply_resolution(&request),
        "list_resolution_intents" => list_resolution_intents(&request),
        "seal_resolution" => seal_resolution(&request),
        "resolution_uploads" => resolution_uploads(&request),
        "resolution_receipt" => resolution_receipt(&request),
        "receive_resolution" => receive_resolution(&request),
        "commit_ack" => commit_ack(&request),
        action => panic!("unsupported native bridge action {action}"),
    };
    write_response(&response);
}
