//! Durable Note sync-intent and sealing-lifecycle primitives.
//!
//! Callers prepare plaintext intents in the same SQLite transaction that
//! mutates `notes`. Sealing later replaces that sidecar with an opaque object
//! and advances the existing outbox event atomically; uploads remain outside
//! this module.

use std::fmt::Write as _;

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};

use crate::note_sync_plaintext::{
    decode_note_sync_plaintext, decode_note_sync_resolution_v2, eligibility, Eligibility,
    NoteSyncPlaintext, NoteSyncResolutionV2Result, NoteSyncResolutionV2Strategy, NoteSyncRoute,
};
use crate::sqlite::{OwnedRemoteApplyAuthorization, PrivilegedRemoteApplyConnection, StorageError};

const MAX_SYNC_INTEGER: i64 = 9_007_199_254_740_991;
const MAX_UNSEALED_INTENT_LIST_LIMIT: u32 = 200;
const MAX_SEALED_OUTBOX_LIST_LIMIT: u32 = 200;
const MAX_ENCRYPTED_SYNC_CIPHERTEXT_BYTES: usize = 8_388_624;
const MAX_ENCRYPTED_SYNC_BATCH_CIPHERTEXT_BYTES: usize = 16_777_216;
const MAX_ENCRYPTED_SYNC_WIRE_BODY_BYTES: usize = 33_554_432;
const MAX_RECEIVED_INBOX_LIST_LIMIT: u32 = 32;
const MAX_SEALED_RESOLUTION_UPLOAD_LIST_LIMIT: u32 = 100;
const SUPPORTED_CRYPTO_VERSION: i64 = 1;
const SUPPORTED_AAD_VERSION: i64 = 1;

#[derive(Debug)]
pub(crate) enum NoteSyncError {
    Database(rusqlite::Error),
    InvalidSnapshot(&'static str),
    RevisionOverflow,
    LocalOrdinalOverflow,
    MutationGenerationOverflow,
    SealAttemptOverflow,
    ConflictingHeads,
    InvalidListLimit,
    InvalidOutboxRead(&'static str),
    InvalidEnvelope(&'static str),
    InvalidSealState(&'static str),
    MissingEvent,
    MissingIntent,
    UnexpectedLifecycle,
    SealedObjectMissing,
    ConflictingEncryptedObject,
    ConflictingUploadReceipt,
    Random(String),
}

#[cfg(test)]
mod cloud_project_bootstrap_tests {
    use super::*;
    use crate::sqlite::apply_migrations;

    const ACCOUNT: &str = "bootstrap-account";
    const DEVICE: &str = "123e4567-e89b-42d3-a456-426614174001";

    fn note(id: &str, content: &str) -> String {
        serde_json::json!({
            "id":id,"project_id":"p","stage_id":null,"source_type":"project",
            "source_map_id":null,"source_node_id":null,"content_format":"html",
            "title":"","content":content,"checklist":[],"color":"default",
            "pinned":false,"archived":false,"sort_order":0,"tags":[],
            "created_at":"2026-09-24T00:00:00.000000Z",
            "updated_at":"2026-09-24T00:00:00.000000Z","revision":0,"metadata":{}
        })
        .to_string()
    }

    fn database(notes: &[(&str, &str)]) -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        apply_migrations(&connection).unwrap();
        connection
            .execute(
                "INSERT INTO mirror_state(id,source_format,source_schema_version,sync_status)
             VALUES(1,'test','1','healthy')",
                [],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE storage_ownership SET owner='sqlite' WHERE subsystem='notes'",
                [],
            )
            .unwrap();
        connection.execute(
            "INSERT INTO cloud_sync_state(account_id,device_id,pull_cursor,ack_cursor,created_at,updated_at) VALUES(?1,?2,0,0,'now','now')",
            rusqlite::params![ACCOUNT, DEVICE],
        ).unwrap();
        connection.execute(
            "INSERT INTO projects(id,name,goal,infinite,unit,status,payload_json) VALUES('p','Project',NULL,1,'symbols','активен','{}')", [],
        ).unwrap();
        connection
            .execute(
                "INSERT INTO project_order(project_id,position) VALUES('p',0)",
                [],
            )
            .unwrap();
        for (id, content) in notes {
            connection.execute(
                "INSERT INTO notes(id,project_id,stage_id,updated_at,payload_json) VALUES(?1,'p',NULL,'2026-09-24T00:00:00.000000Z',?2)",
                rusqlite::params![id, note(id, content)],
            ).unwrap();
        }
        connection
    }

    fn prepare_command() -> PrepareCloudProjectBootstrapCommand {
        PrepareCloudProjectBootstrapCommand {
            project_id: "p".into(),
            account_id: ACCOUNT.into(),
            device_id: DEVICE.into(),
            mode: CloudProjectBootstrapMode::UploadExisting,
        }
    }

    fn scope(record: &CloudProjectBootstrapRecord) -> CloudProjectBootstrapScopeCommand {
        CloudProjectBootstrapScopeCommand {
            project_id: record.project_id.clone(),
            account_id: record.account_id.clone(),
            device_id: record.device_id.clone(),
            bootstrap_id: record.bootstrap_id.clone(),
        }
    }

    fn registered(
        connection: &mut Connection,
        notes: &[(&str, &str)],
    ) -> CloudProjectBootstrapRecord {
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM notes", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            notes.len() as i64
        );
        let prepared = prepare_cloud_project_bootstrap(connection, &prepare_command()).unwrap();
        confirm_cloud_project_registration(
            connection,
            &ConfirmCloudProjectRegistrationCommand {
                project_id: "p".into(),
                account_id: ACCOUNT.into(),
                device_id: DEVICE.into(),
                bootstrap_id: prepared.bootstrap_id,
                remote_state: "initializing".into(),
                remote_high_water: 0,
            },
        )
        .unwrap()
    }

    #[test]
    fn bootstrap_capture_is_atomic_complete_and_idempotent() {
        let notes = [("a", "first"), ("b", "second")];
        let mut connection = database(&notes);
        let registered = registered(&mut connection, &notes);
        let captured =
            capture_initial_note_sync_intents(&mut connection, &scope(&registered)).unwrap();
        assert_eq!(
            (captured.phase.as_str(), captured.initial_event_count),
            ("captured", 2)
        );
        assert!(captured.initial_local_ordinal_hi >= 2);
        let event_ids: Vec<String> = connection
            .prepare("SELECT event_id FROM cloud_sync_outbox ORDER BY local_ordinal")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(event_ids.len(), 2);
        assert_eq!(
            capture_initial_note_sync_intents(&mut connection, &scope(&registered)).unwrap(),
            captured
        );
        let replayed: Vec<String> = connection
            .prepare("SELECT event_id FROM cloud_sync_outbox ORDER BY local_ordinal")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(replayed, event_ids);
    }

    #[test]
    fn file_backed_restart_reuses_bootstrap_token_and_initial_event_ids() {
        let root = std::env::temp_dir().join(format!(
            "worta-c16-bootstrap-restart-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("nfprogress.db");
        let mut connection = crate::sqlite::open_database(&path).unwrap();
        connection
            .execute(
                "INSERT INTO mirror_state(id,source_format,source_schema_version,sync_status)
             VALUES(1,'test','1','healthy')",
                [],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE storage_ownership SET owner='sqlite' WHERE subsystem='notes'",
                [],
            )
            .unwrap();
        connection.execute(
            "INSERT INTO cloud_sync_state(account_id,device_id,pull_cursor,ack_cursor,created_at,updated_at)
             VALUES(?1,?2,0,0,'now','now')", rusqlite::params![ACCOUNT, DEVICE],
        ).unwrap();
        connection
            .execute(
                "INSERT INTO projects(id,name,goal,infinite,unit,status,payload_json)
             VALUES('p','Project',NULL,1,'symbols','активен','{}')",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO project_order(project_id,position) VALUES('p',0)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO notes(id,project_id,stage_id,updated_at,payload_json)
             VALUES('a','p',NULL,'2026-09-24T00:00:00.000000Z',?1)",
                [note("a", "first")],
            )
            .unwrap();
        let registered = registered(&mut connection, &[("a", "first")]);
        let captured =
            capture_initial_note_sync_intents(&mut connection, &scope(&registered)).unwrap();
        let event_id: String = connection
            .query_row("SELECT event_id FROM cloud_sync_outbox", [], |row| {
                row.get(0)
            })
            .unwrap();
        drop(connection);

        let mut reopened = crate::sqlite::open_database(&path).unwrap();
        let prepared = prepare_cloud_project_bootstrap(&mut reopened, &prepare_command()).unwrap();
        let replayed = capture_initial_note_sync_intents(&mut reopened, &scope(&prepared)).unwrap();
        let replay_event_id: String = reopened
            .query_row("SELECT event_id FROM cloud_sync_outbox", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(prepared.bootstrap_id, captured.bootstrap_id);
        assert_eq!(replayed.initial_event_count, 1);
        assert_eq!(replay_event_id, event_id);
        drop(reopened);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn bootstrap_capture_rolls_back_binding_and_every_intent_on_failure() {
        let notes = [("a", "first"), ("b", "second")];
        let mut connection = database(&notes);
        let registered = registered(&mut connection, &notes);
        connection
            .execute_batch(
                "CREATE TRIGGER bootstrap_test_failure BEFORE INSERT ON cloud_sync_note_intents
             WHEN NEW.snapshot_json LIKE '%second%' BEGIN SELECT RAISE(ABORT,'injected'); END;",
            )
            .unwrap();
        assert!(capture_initial_note_sync_intents(&mut connection, &scope(&registered)).is_err());
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM cloud_sync_project_bindings",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM cloud_sync_note_intents", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            0
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT phase FROM cloud_sync_project_bootstraps",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            "registered"
        );
    }

    #[test]
    fn edits_and_deletes_during_bootstrap_preserve_ordered_durable_work() {
        let mut connection = database(&[("a", "first"), ("b", "second")]);
        let registered = registered(&mut connection, &[("a", "first"), ("b", "second")]);
        let captured =
            capture_initial_note_sync_intents(&mut connection, &scope(&registered)).unwrap();

        crate::update_note_in_connection(
            &mut connection,
            "p",
            "a",
            &serde_json::json!({"content":"latest"}),
            None,
        )
        .unwrap();
        let coalesced: (i64, String) = connection
            .query_row(
                "SELECT count(*),max(intent.snapshot_json) FROM cloud_sync_outbox event
             JOIN cloud_sync_note_intents intent USING(event_id) WHERE event.entity_id='a'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(coalesced.0, 1);
        assert!(coalesced.1.contains("latest"));

        let (event_id, generation): (String, i64) = connection
            .query_row(
                "SELECT event.event_id,intent.mutation_generation FROM cloud_sync_outbox event
             JOIN cloud_sync_note_intents intent USING(event_id) WHERE event.entity_id='b'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        commit_sealed_note_sync_event(
            &mut connection,
            &CommitSealedNoteSyncEventCommand {
                event_id: event_id.clone(),
                expected_mutation_generation: generation,
                envelope: EncryptedNoteSyncEnvelope {
                    crypto_version: 1,
                    aad_version: 1,
                    nonce: encode_canonical_base64url(&[3_u8; 24]),
                    ciphertext: encode_canonical_base64url(&[4_u8; 16]),
                },
            },
        )
        .unwrap();
        crate::delete_note_in_connection(&mut connection, "p", "b", None).unwrap();
        let follow_up: (String, Option<String>, i64, String) = connection
            .query_row(
                "SELECT event_id,parent_event_id,local_ordinal,operation FROM cloud_sync_outbox
             WHERE entity_id='b' ORDER BY local_ordinal DESC LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_ne!(follow_up.0, event_id);
        assert_eq!(follow_up.1.as_deref(), Some(event_id.as_str()));
        assert!(follow_up.2 > captured.initial_local_ordinal_hi);
        assert_eq!(follow_up.3, "delete");
        assert_eq!(captured.initial_event_count, 2);
    }

    #[test]
    fn unsupported_note_blocks_before_token_and_before_capture() {
        let mut connection = database(&[("a", "first")]);
        connection.execute(
            "UPDATE notes SET payload_json=json_set(payload_json,'$.stage_id','stage') WHERE id='a'", [],
        ).unwrap();
        assert!(prepare_cloud_project_bootstrap(&mut connection, &prepare_command()).is_err());
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM cloud_sync_project_bootstraps",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );

        connection
            .execute(
                "UPDATE notes SET payload_json=?1 WHERE id='a'",
                [note("a", "first")],
            )
            .unwrap();
        let registered = registered(&mut connection, &[("a", "first")]);
        connection.execute(
            "UPDATE notes SET payload_json=json_set(payload_json,'$.content_format','plain') WHERE id='a'", [],
        ).unwrap();
        assert!(capture_initial_note_sync_intents(&mut connection, &scope(&registered)).is_err());
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM cloud_sync_project_bindings",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
    }

    #[test]
    fn accepted_cohort_completion_ready_and_pause_resume_are_durable() {
        let mut connection = database(&[("a", "first")]);
        let registered = registered(&mut connection, &[("a", "first")]);
        let captured =
            capture_initial_note_sync_intents(&mut connection, &scope(&registered)).unwrap();
        let (event_id, generation): (String, i64) = connection.query_row(
            "SELECT event.event_id,intent.mutation_generation FROM cloud_sync_outbox event JOIN cloud_sync_note_intents intent ON intent.event_id=event.event_id",
            [], |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap();
        commit_sealed_note_sync_event(
            &mut connection,
            &CommitSealedNoteSyncEventCommand {
                event_id: event_id.clone(),
                expected_mutation_generation: generation,
                envelope: EncryptedNoteSyncEnvelope {
                    crypto_version: 1,
                    aad_version: 1,
                    nonce: encode_canonical_base64url(&[1_u8; 24]),
                    ciphertext: encode_canonical_base64url(&[2_u8; 16]),
                },
            },
        )
        .unwrap();
        commit_note_sync_upload_acceptance(
            &mut connection,
            &CommitNoteSyncUploadAcceptanceCommand {
                account_id: ACCOUNT.into(),
                device_id: DEVICE.into(),
                receipts: vec![NoteSyncUploadReceipt {
                    event_id,
                    server_sequence: 1,
                    duplicate: false,
                }],
            },
        )
        .unwrap();
        let completing =
            mark_cloud_project_bootstrap_completing(&mut connection, &scope(&captured)).unwrap();
        assert_eq!(
            (
                completing.phase.as_str(),
                completing.initial_max_server_sequence
            ),
            ("completing", Some(1))
        );
        let active = confirm_cloud_project_registration(
            &mut connection,
            &ConfirmCloudProjectRegistrationCommand {
                project_id: "p".into(),
                account_id: ACCOUNT.into(),
                device_id: DEVICE.into(),
                bootstrap_id: active_id(&completing),
                remote_state: "active".into(),
                remote_high_water: 1,
            },
        )
        .unwrap();
        connection
            .execute(
                "UPDATE cloud_sync_state SET pull_cursor=1,ack_cursor=1 WHERE account_id=?1",
                [ACCOUNT],
            )
            .unwrap();
        let ready = mark_cloud_project_bootstrap_ready(&mut connection, &scope(&active)).unwrap();
        assert_eq!(ready.phase, "ready");
        assert_eq!(
            set_cloud_project_bootstrap_paused(&mut connection, &scope(&ready), true)
                .unwrap()
                .phase,
            "paused"
        );
        assert_eq!(
            set_cloud_project_bootstrap_paused(&mut connection, &scope(&ready), false)
                .unwrap()
                .phase,
            "ready"
        );
    }

    fn active_id(record: &CloudProjectBootstrapRecord) -> String {
        record.bootstrap_id.clone()
    }

    #[test]
    fn remote_import_refuses_same_id_without_lineage() {
        let mut collision = database(&[]);
        let command = ImportRemoteCloudProjectCommand {
            project_id: "p".into(),
            display_name: "Remote".into(),
            account_id: ACCOUNT.into(),
            device_id: DEVICE.into(),
            bootstrap_id: generate_bootstrap_id().unwrap(),
            remote_high_water: 0,
            authenticated_metadata: None,
        };
        assert!(import_remote_cloud_project(&mut collision, &command).is_err());

        let mut clean = Connection::open_in_memory().unwrap();
        apply_migrations(&clean).unwrap();
        clean.execute("INSERT INTO cloud_sync_state(account_id,device_id,pull_cursor,ack_cursor,created_at,updated_at) VALUES(?1,?2,0,0,'now','now')", rusqlite::params![ACCOUNT, DEVICE]).unwrap();
        let imported = import_remote_cloud_project(&mut clean, &command).unwrap();
        assert_eq!(
            (imported.mode.as_str(), imported.phase.as_str()),
            ("import_remote", "captured")
        );
        assert_eq!(
            import_remote_cloud_project(&mut clean, &command).unwrap(),
            imported
        );
    }
}

#[cfg(test)]
mod remote_apply_tests {
    use super::*;
    use crate::note_sync_plaintext::decode_note_sync_plaintext;
    use crate::sqlite::{
        apply_migrations, PrivilegedRemoteApplyConnection, RemoteApplyAuthorization,
    };

    const ACCOUNT: &str = "account";
    const CANONICAL_USER: &str = "123e4567-e89b-42d3-a456-4266141740aa";
    const PULLING_DEVICE: &str = "123e4567-e89b-42d3-a456-426614174001";
    const REMOTE_DEVICE: &str = "123e4567-e89b-42d3-a456-426614174002";
    const CREATE_EVENT: &str = "123e4567-e89b-42d3-a456-426614174100";
    const UPDATE_EVENT: &str = "123e4567-e89b-42d3-a456-426614174101";
    const DELETE_EVENT: &str = "123e4567-e89b-42d3-a456-426614174102";

    fn database() -> PrivilegedRemoteApplyConnection {
        let connection = Connection::open_in_memory().unwrap();
        configured_database(connection)
    }

    fn conflict_database_path(label: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "nfprogress-{label}-{}",
            canonical_uuid_v4().unwrap()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("nfprogress.db");
        (root, path)
    }

    fn configured_database(connection: Connection) -> PrivilegedRemoteApplyConnection {
        apply_migrations(&connection).unwrap();
        connection
            .execute(
                "INSERT INTO projects(id,name,infinite,unit,status,payload_json)
             VALUES('p','Project',0,'symbols','active','{}')",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO project_order(project_id,position) VALUES('p',0)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO cloud_sync_state(
                account_id,device_id,pull_cursor,ack_cursor,created_at,updated_at
             ) VALUES(?1,?2,0,0,'now','now')",
                rusqlite::params![ACCOUNT, PULLING_DEVICE],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO cloud_account_bindings(
                local_account_id,canonical_user_id,created_at,validated_at
             ) VALUES(?1,?2,'now','now')",
                rusqlite::params![ACCOUNT, CANONICAL_USER],
            )
            .unwrap();
        connection.execute(
            "INSERT INTO cloud_sync_project_bindings(project_id,account_id,created_at,updated_at)
             VALUES('p',?1,'now','now')",
            [ACCOUNT],
        ).unwrap();
        PrivilegedRemoteApplyConnection::from_connection(connection).unwrap()
    }

    fn plaintext(
        event_id: &str,
        parent_event_id: Option<&str>,
        revision: i64,
        operation: &str,
        updated_at: &str,
        content: &str,
    ) -> NoteSyncPlaintext {
        let mutation = if operation == "delete" {
            "delete"
        } else if revision == 1 {
            "create"
        } else {
            "update"
        };
        let note = if operation == "delete" {
            serde_json::json!({
                "id":"n","project_id":"p","stage_id":null,"source_type":"project",
                "source_map_id":null,"source_node_id":null,"content_format":"html",
                "deleted_at":updated_at,
            })
        } else {
            serde_json::json!({
                "id":"n","project_id":"p","stage_id":null,"source_type":"project",
                "source_map_id":null,"source_node_id":null,"content_format":"html",
                "title":"Title","content":content,"checklist":[],"color":"default",
                "pinned":false,"archived":false,"sort_order":0,"tags":[],
                "created_at":"2026-01-01T00:00:00.000000Z",
                "updated_at":updated_at,"metadata":{},
            })
        };
        let value = serde_json::json!({
            "version":1,
            "header":{
                "event_id":event_id,"parent_event_id":parent_event_id,"project_id":"p",
                "entity_id":"n","entity_type":"note","operation":operation,
                "revision":revision,"updated_at":updated_at,
                "deleted_at":if operation == "delete" { serde_json::Value::String(updated_at.to_string()) } else { serde_json::Value::Null },
            },
            "mutation":mutation,"note":note,
        });
        decode_note_sync_plaintext(serde_json::to_string(&value).unwrap().as_bytes()).unwrap()
    }

    fn header(plaintext: &NoteSyncPlaintext) -> (&str, &str, i64, &str, Option<&str>) {
        match plaintext {
            NoteSyncPlaintext::Create { header, .. }
            | NoteSyncPlaintext::Update { header, .. }
            | NoteSyncPlaintext::Delete { header, .. } => (
                &header.event_id,
                &header.operation,
                header.revision,
                &header.updated_at,
                header.deleted_at.as_deref(),
            ),
        }
    }

    fn received(
        connection: &PrivilegedRemoteApplyConnection,
        plaintext: NoteSyncPlaintext,
        sequence: i64,
        source_device_id: &str,
    ) -> ApplyVerifiedReceivedNoteCommand {
        let (event_id, operation, revision, updated_at, deleted_at) = header(&plaintext);
        let nonce = vec![sequence as u8; 24];
        let ciphertext = vec![(sequence + 31) as u8; 16];
        connection
            .connection()
            .execute(
                "INSERT INTO cloud_sync_inbox(
                account_id,event_id,server_sequence,device_id,project_id,entity_id,
                entity_type,operation,sync_revision,updated_at,deleted_at,state,received_at
             ) VALUES(?1,?2,?3,?4,'p','n','note',?5,?6,?7,?8,'received','now')",
                rusqlite::params![
                    ACCOUNT,
                    event_id,
                    sequence,
                    source_device_id,
                    operation,
                    revision,
                    updated_at,
                    deleted_at
                ],
            )
            .unwrap();
        connection
            .connection()
            .execute(
                "INSERT INTO cloud_sync_event_objects(
                account_id,event_id,crypto_version,aad_version,nonce,ciphertext,stored_at
             ) VALUES(?1,?2,1,1,?3,?4,'now')",
                rusqlite::params![ACCOUNT, event_id, nonce, ciphertext],
            )
            .unwrap();
        ApplyVerifiedReceivedNoteCommand {
            account_id: ACCOUNT.to_string(),
            canonical_user_id: CANONICAL_USER.to_string(),
            pulling_device_id: PULLING_DEVICE.to_string(),
            event_id: event_id.to_string(),
            server_sequence: sequence,
            source_device_id: source_device_id.to_string(),
            crypto_version: 1,
            aad_version: 1,
            nonce: vec![sequence as u8; 24],
            ciphertext: vec![(sequence + 31) as u8; 16],
            plaintext,
        }
    }

    fn apply_create(
        connection: &mut PrivilegedRemoteApplyConnection,
    ) -> ApplyVerifiedReceivedNoteCommand {
        let command = received(
            connection,
            plaintext(
                CREATE_EVENT,
                None,
                1,
                "upsert",
                "2026-01-01T00:00:00.000000Z",
                "create",
            ),
            1,
            REMOTE_DEVICE,
        );
        assert_eq!(
            apply_verified_received_note(connection, &command).unwrap(),
            ApplyVerifiedReceivedNoteResult::Applied
        );
        command
    }

    fn prepare_local_branch(
        connection: &PrivilegedRemoteApplyConnection,
        operation: NoteSyncOperation,
        content: &str,
    ) -> PreparedNoteIntent {
        let updated_at = "2026-01-02T00:00:00.000000Z";
        let snapshot = if operation == NoteSyncOperation::Delete {
            let current: String = connection
                .connection()
                .query_row("SELECT payload_json FROM notes WHERE id='n'", [], |row| {
                    row.get(0)
                })
                .unwrap();
            build_note_tombstone(&serde_json::from_str(&current).unwrap(), updated_at).unwrap()
        } else {
            match plaintext(
                "123e4567-e89b-42d3-a456-426614174110",
                Some(CREATE_EVENT),
                2,
                "upsert",
                updated_at,
                content,
            ) {
                NoteSyncPlaintext::Update { note, .. } => note_payload_from_record(&note).unwrap(),
                _ => unreachable!(),
            }
        };
        let transaction = connection.connection().unchecked_transaction().unwrap();
        let prepared = prepare_unsealed_note_intent(
            &transaction,
            PrepareNoteIntent {
                project_id: "p",
                entity_id: "n",
                operation,
                updated_at,
                deleted_at: (operation == NoteSyncOperation::Delete).then_some(updated_at),
                snapshot_json: &snapshot,
                state_updated_at: updated_at,
            },
        )
        .unwrap()
        .unwrap();
        match operation {
            NoteSyncOperation::Upsert => {
                transaction.execute(
                    "UPDATE notes SET updated_at=?1,payload_json=?2 WHERE id='n' AND project_id='p'",
                    rusqlite::params![updated_at, snapshot],
                ).unwrap();
            }
            NoteSyncOperation::Delete => {
                transaction
                    .execute("DELETE FROM notes WHERE id='n' AND project_id='p'", [])
                    .unwrap();
            }
        }
        transaction.commit().unwrap();
        prepared
    }

    fn prepare_edit_conflict(connection: &mut PrivilegedRemoteApplyConnection) -> (String, String) {
        apply_create(connection);
        let local = prepare_local_branch(connection, NoteSyncOperation::Upsert, "local edit");
        let remote = received(
            connection,
            plaintext(
                UPDATE_EVENT,
                Some(CREATE_EVENT),
                2,
                "upsert",
                "2026-01-02T00:00:00.000000Z",
                "remote edit",
            ),
            2,
            REMOTE_DEVICE,
        );
        assert_eq!(
            apply_verified_received_note(connection, &remote).unwrap(),
            ApplyVerifiedReceivedNoteResult::Conflict
        );
        (local.event_id, UPDATE_EVENT.into())
    }

    fn seed_three_tip_applying_resolution(
        connection: &mut PrivilegedRemoteApplyConnection,
    ) -> String {
        apply_create(connection);
        let local = prepare_local_branch(connection, NoteSyncOperation::Upsert, "local edit");
        let first = received(
            connection,
            plaintext(
                UPDATE_EVENT,
                Some(CREATE_EVENT),
                2,
                "upsert",
                "2026-01-02T00:00:00.000000Z",
                "remote one",
            ),
            2,
            REMOTE_DEVICE,
        );
        assert_eq!(
            apply_verified_received_note(connection, &first).unwrap(),
            ApplyVerifiedReceivedNoteResult::Conflict,
        );
        let third_event = "123e4567-e89b-42d3-a456-426614174103";
        let third = received(
            connection,
            plaintext(
                third_event,
                Some(CREATE_EVENT),
                2,
                "upsert",
                "2026-01-02T00:00:01.000000Z",
                "remote two",
            ),
            3,
            REMOTE_DEVICE,
        );
        assert_eq!(
            apply_verified_received_note(connection, &third).unwrap(),
            ApplyVerifiedReceivedNoteResult::Conflict,
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT generation FROM cloud_sync_note_conflict_groups",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            2
        );
        assert_eq!(
            connection
                .connection()
                .prepare(
                    "SELECT generation,count(*) FROM cloud_sync_note_conflict_tips
             GROUP BY generation ORDER BY generation",
                )
                .unwrap()
                .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
            vec![(1, 2), (2, 1)]
        );

        let mutation_generation: i64 = connection
            .connection()
            .query_row(
                "SELECT mutation_generation FROM cloud_sync_note_intents WHERE event_id=?1",
                [&local.event_id],
                |row| row.get(0),
            )
            .unwrap();
        commit_sealed_note_sync_event(
            connection.connection_mut_for_test(),
            &CommitSealedNoteSyncEventCommand {
                event_id: local.event_id.clone(),
                expected_mutation_generation: mutation_generation,
                envelope: EncryptedNoteSyncEnvelope {
                    crypto_version: 1,
                    aad_version: 1,
                    nonce: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into(),
                    ciphertext: "AAAAAAAAAAAAAAAAAAAAAA".into(),
                },
            },
        )
        .unwrap();
        commit_note_sync_upload_acceptance(
            connection.connection_mut_for_test(),
            &CommitNoteSyncUploadAcceptanceCommand {
                account_id: ACCOUNT.into(),
                device_id: PULLING_DEVICE.into(),
                receipts: vec![NoteSyncUploadReceipt {
                    event_id: local.event_id.clone(),
                    server_sequence: 4,
                    duplicate: false,
                }],
            },
        )
        .unwrap();
        let (local_operation, local_revision, local_updated_at): (String, i64, String) = connection
            .connection()
            .query_row(
                "SELECT operation,revision,updated_at FROM cloud_sync_outbox WHERE event_id=?1",
                [&local.event_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        connection
            .connection()
            .execute(
                "INSERT INTO cloud_sync_inbox(
                account_id,event_id,server_sequence,device_id,project_id,entity_id,
                entity_type,operation,sync_revision,updated_at,deleted_at,state,
                received_at,applied_at
             ) VALUES(?1,?2,4,?3,'p','n','note',?4,?5,?6,NULL,'applied','now','now')",
                rusqlite::params![
                    ACCOUNT,
                    local.event_id,
                    PULLING_DEVICE,
                    local_operation,
                    local_revision,
                    local_updated_at
                ],
            )
            .unwrap();

        let command = resolution_command(connection, "choose_version", None, None);
        let mut payload: serde_json::Value =
            serde_json::from_slice(&command.canonical_payload).unwrap();
        payload["resolution"]["conflict_group_id"] =
            serde_json::Value::String("123e4567-e89b-42d3-a456-426614174299".into());
        payload["resolution"]["conflict_generation"] = serde_json::Value::from(9);
        let canonical_payload = canonical_json(&payload).unwrap().into_bytes();
        let event_id = payload["header"]["event_id"].as_str().unwrap().to_string();
        let updated_at = payload["header"]["updated_at"]
            .as_str()
            .unwrap()
            .to_string();
        let revision = payload["header"]["revision"].as_i64().unwrap();
        let (local_group_id, local_generation): (String, i64) = connection
            .connection()
            .query_row(
                "SELECT group_id,generation FROM cloud_sync_note_conflict_groups
                 WHERE account_id=?1 AND lifecycle='open'",
                [ACCOUNT],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        let parent_ids: Vec<String> = payload["resolution"]["resolved_event_ids"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap().to_string())
            .collect();
        let parents_json = serde_json::to_string(&parent_ids).unwrap();
        let result_snapshot = canonical_json(&payload["result"]["note"]).unwrap();
        connection
            .connection()
            .execute(
                "INSERT INTO cloud_sync_inbox(
                account_id,event_id,server_sequence,device_id,project_id,entity_id,
                entity_type,operation,sync_revision,updated_at,deleted_at,state,received_at
             ) VALUES(?1,?2,5,?3,'p','n','note','resolution',?4,?5,NULL,'received','now')",
                rusqlite::params![ACCOUNT, event_id, REMOTE_DEVICE, revision, updated_at],
            )
            .unwrap();
        connection
            .connection()
            .execute(
                "INSERT INTO cloud_sync_event_objects VALUES(?1,?2,1,1,?3,?4,'now')",
                rusqlite::params![ACCOUNT, event_id, vec![5_u8; 24], vec![35_u8; 16]],
            )
            .unwrap();
        connection.connection().execute(
            "INSERT INTO cloud_sync_note_applied_resolutions(
                account_id,resolution_event_id,source_device_id,server_sequence,
                project_id,entity_id,revision,event_updated_at,remote_conflict_group_id,
                local_conflict_group_id,remote_conflict_generation,local_conflict_generation,
                parent_event_ids_json,parent_count,strategy,result_operation,canonical_payload,
                result_snapshot_json,clone_entity_id,clone_snapshot_json,lifecycle,applied_at,created_at
             ) VALUES(?1,?2,?3,5,'p','n',?4,?5,?6,?7,9,?8,?9,?10,
                      'choose_version','upsert',?11,?12,NULL,NULL,'applying',NULL,'now')",
            rusqlite::params![ACCOUNT, event_id, REMOTE_DEVICE, revision, updated_at,
                "123e4567-e89b-42d3-a456-426614174299", local_group_id,
                local_generation, parents_json, parent_ids.len() as i64,
                canonical_payload, result_snapshot],
        ).unwrap();

        let mut statement = connection
            .connection()
            .prepare(
                "SELECT version.version_id,version.event_id,version.revision,version.operation,
                    version.snapshot_json,version.source,version.local_mutation_generation,
                    version.server_sequence
             FROM cloud_sync_note_conflict_tips AS tip
             JOIN cloud_sync_note_conflict_versions AS version
               ON version.version_id=tip.version_id
             WHERE tip.group_id=?1 ORDER BY version.event_id",
            )
            .unwrap();
        let tips = statement
            .query_map([&local_group_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Option<i64>>(6)?,
                    row.get::<_, Option<i64>>(7)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        drop(statement);
        for (ordinal, tip) in tips.into_iter().enumerate() {
            let (publication_source, server_sequence) =
                if tip.5 == "local_unsealed" {
                    let sequence = connection.connection().query_row(
                    "SELECT server_sequence FROM cloud_sync_upload_receipts WHERE event_id=?1",
                    [&tip.1], |row| row.get::<_, i64>(0),
                ).unwrap();
                    ("local_accepted", sequence)
                } else {
                    (tip.5.as_str(), tip.7.unwrap())
                };
            connection
                .connection()
                .execute(
                    "INSERT INTO cloud_sync_note_applied_resolution_parents(
                    account_id,resolution_event_id,parent_ordinal,parent_event_id,
                    conflict_version_id,parent_revision,parent_operation,parent_snapshot_json,
                    publication_source,server_sequence,local_mutation_generation,recorded_at
                 ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'now')",
                    rusqlite::params![
                        ACCOUNT,
                        event_id,
                        ordinal as i64,
                        tip.1,
                        tip.0,
                        tip.2,
                        tip.3,
                        tip.4,
                        publication_source,
                        server_sequence,
                        tip.6
                    ],
                )
                .unwrap();
        }
        connection
            .connection()
            .execute(
                "UPDATE cloud_sync_state SET pull_cursor=5 WHERE account_id=?1",
                [ACCOUNT],
            )
            .unwrap();
        event_id
    }

    fn resolution_command(
        connection: &PrivilegedRemoteApplyConnection,
        strategy: &str,
        selected: Option<&str>,
        retained: Option<&str>,
    ) -> PrepareNoteConflictResolutionCommand {
        let (group_id,generation):(String,i64)=connection.connection().query_row("SELECT group_id,generation FROM cloud_sync_note_conflict_groups WHERE lifecycle='open'",[],|row|Ok((row.get(0)?,row.get(1)?))).unwrap();
        let mut statement=connection.connection().prepare("SELECT tip.event_id,version.revision,version.operation,version.snapshot_json FROM cloud_sync_note_conflict_tips AS tip JOIN cloud_sync_note_conflict_versions AS version ON version.version_id=tip.version_id WHERE tip.group_id=?1 ORDER BY tip.event_id").unwrap();
        let tips = statement
            .query_map([&group_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        drop(statement);
        let ids: Vec<String> = tips.iter().map(|tip| tip.0.clone()).collect();
        let revision = tips.iter().map(|tip| tip.1).max().unwrap() + 1;
        let chosen = selected
            .and_then(|id| tips.iter().find(|tip| tip.0 == id))
            .unwrap_or(&tips[0]);
        let chosen_note = wire_snapshot(&chosen.3, &chosen.2).unwrap();
        let mut resolution = serde_json::json!({"conflict_group_id":group_id,"conflict_generation":generation,"resolved_event_ids":ids.clone(),"strategy":strategy});
        let result = match strategy {
            "choose_version" => {
                resolution["selected_event_id"] = serde_json::Value::String(chosen.0.clone());
                serde_json::json!({"operation":chosen.2,"note":chosen_note})
            }
            "manual_merge" => {
                let mut note = chosen_note;
                note["content"] = serde_json::Value::String("manual merge".into());
                serde_json::json!({"operation":"upsert","note":note})
            }
            "delete" => {
                serde_json::json!({"operation":"delete","note":{"id":"n","project_id":"p","stage_id":null,"source_type":"project","source_map_id":null,"source_node_id":null,"content_format":"html","deleted_at":"2026-01-03T00:00:00.000000Z"}})
            }
            "keep_both" => {
                let retained_tip = tips.iter().find(|tip| tip.0 == retained.unwrap()).unwrap();
                let mut retained_note = wire_snapshot(&retained_tip.3, &retained_tip.2).unwrap();
                retained_note["id"] = serde_json::Value::String("n-copy".into());
                retained_note["created_at"] =
                    serde_json::Value::String("2026-01-03T00:00:00.000000Z".into());
                retained_note["updated_at"] =
                    serde_json::Value::String("2026-01-03T00:00:00.000000Z".into());
                resolution["selected_event_id"] = serde_json::Value::String(chosen.0.clone());
                resolution["retained_event_id"] = serde_json::Value::String(retained_tip.0.clone());
                resolution["retained_note"] = retained_note;
                serde_json::json!({"operation":"upsert","note":chosen_note})
            }
            _ => unreachable!(),
        };
        let value = serde_json::json!({"version":2,"header":{"event_id":"123e4567-e89b-42d3-a456-426614174200","parent_event_id":ids[0].clone(),"additional_parent_event_ids":ids[1..].to_vec(),"project_id":"p","entity_id":"n","entity_type":"note","operation":"resolution","revision":revision,"updated_at":"2026-01-03T00:00:00.000000Z"},"mutation":"resolution","resolution":resolution,"result":result});
        PrepareNoteConflictResolutionCommand {
            account_id: ACCOUNT.into(),
            canonical_user_id: CANONICAL_USER.into(),
            device_id: PULLING_DEVICE.into(),
            canonical_payload: canonical_json(&value).unwrap().into_bytes(),
        }
    }

    fn accept_local_conflict_parent(
        connection: &mut PrivilegedRemoteApplyConnection,
        event_id: &str,
        server_sequence: i64,
    ) {
        let mutation_generation: i64 = connection
            .connection()
            .query_row(
                "SELECT mutation_generation FROM cloud_sync_note_intents WHERE event_id=?1",
                [event_id],
                |row| row.get(0),
            )
            .unwrap();
        commit_sealed_note_sync_event(
            connection.connection_mut_for_test(),
            &CommitSealedNoteSyncEventCommand {
                event_id: event_id.into(),
                expected_mutation_generation: mutation_generation,
                envelope: EncryptedNoteSyncEnvelope {
                    crypto_version: 1,
                    aad_version: 1,
                    nonce: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into(),
                    ciphertext: "AAAAAAAAAAAAAAAAAAAAAA".into(),
                },
            },
        )
        .unwrap();
        commit_note_sync_upload_acceptance(
            connection.connection_mut_for_test(),
            &CommitNoteSyncUploadAcceptanceCommand {
                account_id: ACCOUNT.into(),
                device_id: PULLING_DEVICE.into(),
                receipts: vec![NoteSyncUploadReceipt {
                    event_id: event_id.into(),
                    server_sequence,
                    duplicate: false,
                }],
            },
        )
        .unwrap();
        let (operation, revision, updated_at, deleted_at): (String, i64, String, Option<String>) =
            connection
                .connection()
                .query_row(
                    "SELECT operation,revision,updated_at,deleted_at FROM cloud_sync_outbox
                 WHERE event_id=?1",
                    [event_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .unwrap();
        connection
            .connection()
            .execute(
                "INSERT INTO cloud_sync_inbox(
                account_id,event_id,server_sequence,device_id,project_id,entity_id,
                entity_type,operation,sync_revision,updated_at,deleted_at,state,
                received_at,applied_at
             ) VALUES(?1,?2,?3,?4,'p','n','note',?5,?6,?7,?8,'applied','now','now')",
                rusqlite::params![
                    ACCOUNT,
                    event_id,
                    server_sequence,
                    PULLING_DEVICE,
                    operation,
                    revision,
                    updated_at,
                    deleted_at
                ],
            )
            .unwrap();
    }

    fn received_peer_resolution(
        connection: &PrivilegedRemoteApplyConnection,
        prepared: PrepareNoteConflictResolutionCommand,
        server_sequence: i64,
        source_device_id: &str,
    ) -> VerifiedPeerResolutionCommand {
        received_peer_resolution_at(
            connection,
            prepared,
            server_sequence,
            source_device_id,
            None,
        )
    }

    fn received_peer_resolution_at(
        connection: &PrivilegedRemoteApplyConnection,
        mut prepared: PrepareNoteConflictResolutionCommand,
        server_sequence: i64,
        source_device_id: &str,
        inbox_updated_at: Option<&str>,
    ) -> VerifiedPeerResolutionCommand {
        let mut value: serde_json::Value =
            serde_json::from_slice(&prepared.canonical_payload).unwrap();
        value["resolution"]["conflict_group_id"] =
            serde_json::Value::String("123e4567-e89b-42d3-a456-426614174299".into());
        value["resolution"]["conflict_generation"] = serde_json::Value::from(77);
        prepared.canonical_payload = canonical_json(&value).unwrap().into_bytes();
        let resolution = decode_canonical_resolution(&prepared).unwrap();
        let nonce = vec![9_u8; 24];
        let ciphertext = vec![19_u8; 32];
        connection
            .connection()
            .execute(
                "INSERT INTO cloud_sync_inbox(
                account_id,event_id,server_sequence,device_id,project_id,entity_id,
                entity_type,operation,sync_revision,updated_at,deleted_at,state,received_at
             ) VALUES(?1,?2,?3,?4,?5,?6,'note','resolution',?7,?8,NULL,'received','now')",
                rusqlite::params![
                    ACCOUNT,
                    resolution.header.event_id,
                    server_sequence,
                    source_device_id,
                    resolution.header.project_id,
                    resolution.header.entity_id,
                    resolution.header.revision,
                    inbox_updated_at.unwrap_or(&resolution.header.updated_at)
                ],
            )
            .unwrap();
        connection
            .connection()
            .execute(
                "INSERT INTO cloud_sync_event_objects(
                account_id,event_id,crypto_version,aad_version,nonce,ciphertext,stored_at
             ) VALUES(?1,?2,1,1,?3,?4,'now')",
                rusqlite::params![ACCOUNT, resolution.header.event_id, nonce, ciphertext],
            )
            .unwrap();
        connection
            .connection()
            .execute(
                "UPDATE cloud_sync_state SET pull_cursor=?1 WHERE account_id=?2",
                rusqlite::params![server_sequence, ACCOUNT],
            )
            .unwrap();
        VerifiedPeerResolutionCommand {
            account_id: ACCOUNT.into(),
            canonical_user_id: CANONICAL_USER.into(),
            pulling_device_id: PULLING_DEVICE.into(),
            event_id: resolution.header.event_id.clone(),
            server_sequence,
            source_device_id: source_device_id.into(),
            crypto_version: 1,
            aad_version: 1,
            nonce: vec![9_u8; 24],
            ciphertext: vec![19_u8; 32],
            canonical_payload: prepared.canonical_payload,
            resolution,
        }
    }

    fn peer_resolution_fixture(
        strategy: &str,
        selected_remote: bool,
    ) -> (
        PrivilegedRemoteApplyConnection,
        VerifiedPeerResolutionCommand,
        String,
        String,
    ) {
        peer_resolution_fixture_at(strategy, selected_remote, None)
    }

    fn peer_resolution_fixture_at(
        strategy: &str,
        selected_remote: bool,
        inbox_updated_at: Option<&str>,
    ) -> (
        PrivilegedRemoteApplyConnection,
        VerifiedPeerResolutionCommand,
        String,
        String,
    ) {
        let mut connection = database();
        let (local, remote) = prepare_edit_conflict(&mut connection);
        accept_local_conflict_parent(&mut connection, &local, 3);
        let selected = selected_remote
            .then_some(remote.as_str())
            .or(Some(local.as_str()));
        let retained = (strategy == "keep_both").then_some(remote.as_str());
        let prepared = resolution_command(&connection, strategy, selected, retained);
        let command =
            received_peer_resolution_at(&connection, prepared, 4, REMOTE_DEVICE, inbox_updated_at);
        (connection, command, local, remote)
    }

    fn peer_ipc(
        command: &VerifiedPeerResolutionCommand,
    ) -> ApplyVerifiedReceivedResolutionV2IpcCommand {
        ApplyVerifiedReceivedResolutionV2IpcCommand {
            account_id: command.account_id.clone(),
            canonical_user_id: command.canonical_user_id.clone(),
            pulling_device_id: command.pulling_device_id.clone(),
            event_id: command.event_id.clone(),
            server_sequence: command.server_sequence,
            source_device_id: command.source_device_id.clone(),
            crypto_version: command.crypto_version,
            aad_version: command.aad_version,
            nonce: command.nonce.clone(),
            ciphertext: command.ciphertext.clone(),
            plaintext: command.canonical_payload.clone(),
        }
    }

    fn prepared_count(connection: &PrivilegedRemoteApplyConnection) -> i64 {
        connection
            .connection()
            .query_row(
                "SELECT count(*) FROM cloud_sync_note_pending_resolutions",
                [],
                |row| row.get(0),
            )
            .unwrap()
    }

    fn payload_with_event_id(
        command: &PrepareNoteConflictResolutionCommand,
        event_id: &str,
    ) -> PrepareNoteConflictResolutionCommand {
        let mut value: serde_json::Value =
            serde_json::from_slice(&command.canonical_payload).unwrap();
        value["header"]["event_id"] = serde_json::Value::String(event_id.into());
        PrepareNoteConflictResolutionCommand {
            account_id: command.account_id.clone(),
            canonical_user_id: command.canonical_user_id.clone(),
            device_id: command.device_id.clone(),
            canonical_payload: canonical_json(&value).unwrap().into_bytes(),
        }
    }

    fn applied_resolution_for_sealing(
        connection: &mut PrivilegedRemoteApplyConnection,
    ) -> (PrepareNoteConflictResolutionCommand, String, String) {
        let (local, remote) = prepare_edit_conflict(connection);
        let command = resolution_command(connection, "choose_version", Some(&remote), None);
        prepare_note_conflict_resolution(connection.connection_mut_for_test(), &command).unwrap();
        apply_prepared_note_conflict_resolution(connection, &command).unwrap();
        (command, local, remote)
    }

    fn resolution_seal_command(
        command: &PrepareNoteConflictResolutionCommand,
    ) -> CommitSealedNoteResolutionCommand {
        let value: serde_json::Value = serde_json::from_slice(&command.canonical_payload).unwrap();
        CommitSealedNoteResolutionCommand {
            event_id: value["header"]["event_id"].as_str().unwrap().into(),
            account_id: command.account_id.clone(),
            canonical_user_id: command.canonical_user_id.clone(),
            device_id: command.device_id.clone(),
            project_id: "p".into(),
            entity_id: "n".into(),
            expected_canonical_payload: encode_canonical_base64url(&command.canonical_payload),
            envelope: EncryptedNoteSyncEnvelope {
                crypto_version: 1,
                aad_version: 1,
                nonce: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into(),
                ciphertext: "AAAAAAAAAAAAAAAAAAAAAA".into(),
            },
        }
    }

    fn resolution_upload_list_command() -> ListSealedNoteResolutionUploadsCommand {
        ListSealedNoteResolutionUploadsCommand {
            account_id: ACCOUNT.into(),
            device_id: PULLING_DEVICE.into(),
            canonical_user_id: CANONICAL_USER.into(),
            limit: 10,
        }
    }

    fn resolution_receipt_command(
        event_id: &str,
        sequence: i64,
    ) -> CommitNoteResolutionUploadAcceptanceCommand {
        CommitNoteResolutionUploadAcceptanceCommand {
            account_id: ACCOUNT.into(),
            device_id: PULLING_DEVICE.into(),
            canonical_user_id: CANONICAL_USER.into(),
            receipts: vec![NoteSyncUploadReceipt {
                event_id: event_id.into(),
                server_sequence: sequence,
                duplicate: false,
            }],
        }
    }

    fn receive_self_echo(
        connection: &PrivilegedRemoteApplyConnection,
        prepared: PrepareNoteConflictResolutionCommand,
        server_sequence: i64,
    ) -> VerifiedPeerResolutionCommand {
        receive_self_echo_at(connection, prepared, server_sequence, None)
    }

    fn receive_self_echo_at(
        connection: &PrivilegedRemoteApplyConnection,
        prepared: PrepareNoteConflictResolutionCommand,
        server_sequence: i64,
        inbox_updated_at: Option<&str>,
    ) -> VerifiedPeerResolutionCommand {
        let resolution = decode_canonical_resolution(&prepared).unwrap();
        connection
            .connection()
            .execute(
                "INSERT INTO cloud_sync_inbox(
                account_id,event_id,server_sequence,device_id,project_id,entity_id,
                entity_type,operation,sync_revision,updated_at,deleted_at,state,received_at
             ) VALUES(?1,?2,?3,?4,?5,?6,'note','resolution',?7,?8,NULL,'received','now')",
                rusqlite::params![
                    ACCOUNT,
                    resolution.header.event_id,
                    server_sequence,
                    PULLING_DEVICE,
                    resolution.header.project_id,
                    resolution.header.entity_id,
                    resolution.header.revision,
                    inbox_updated_at.unwrap_or(&resolution.header.updated_at)
                ],
            )
            .unwrap();
        connection
            .connection()
            .execute(
                "UPDATE cloud_sync_state SET pull_cursor=?1 WHERE account_id=?2",
                rusqlite::params![server_sequence, ACCOUNT],
            )
            .unwrap();
        let (nonce, ciphertext): (Vec<u8>, Vec<u8>) = connection
            .connection()
            .query_row(
                "SELECT nonce,ciphertext FROM cloud_sync_event_objects
             WHERE account_id=?1 AND event_id=?2",
                rusqlite::params![ACCOUNT, resolution.header.event_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        VerifiedPeerResolutionCommand {
            account_id: ACCOUNT.into(),
            canonical_user_id: CANONICAL_USER.into(),
            pulling_device_id: PULLING_DEVICE.into(),
            event_id: resolution.header.event_id.clone(),
            server_sequence,
            source_device_id: PULLING_DEVICE.into(),
            crypto_version: 1,
            aad_version: 1,
            nonce,
            ciphertext,
            canonical_payload: prepared.canonical_payload,
            resolution,
        }
    }

    fn prepare_self_echo(
        connection: &mut PrivilegedRemoteApplyConnection,
        strategy: &str,
        accepted: bool,
    ) -> (VerifiedPeerResolutionCommand, String) {
        prepare_self_echo_at(connection, strategy, accepted, None)
    }

    fn prepare_self_echo_at(
        connection: &mut PrivilegedRemoteApplyConnection,
        strategy: &str,
        accepted: bool,
        inbox_updated_at: Option<&str>,
    ) -> (VerifiedPeerResolutionCommand, String) {
        let (local, remote) = prepare_edit_conflict(connection);
        accept_local_conflict_parent(connection, &local, 3);
        let retained = (strategy == "keep_both").then_some(remote.as_str());
        let prepared = resolution_command(connection, strategy, Some(&local), retained);
        prepare_note_conflict_resolution(connection.connection_mut_for_test(), &prepared).unwrap();
        apply_prepared_note_conflict_resolution(connection, &prepared).unwrap();
        let seal = resolution_seal_command(&prepared);
        commit_sealed_note_resolution_event(connection.connection_mut_for_test(), &seal).unwrap();
        let resolution = decode_canonical_resolution(&prepared).unwrap();
        if accepted {
            commit_note_resolution_upload_acceptance(
                connection.connection_mut_for_test(),
                &CommitNoteResolutionUploadAcceptanceCommand {
                    account_id: ACCOUNT.into(),
                    device_id: PULLING_DEVICE.into(),
                    canonical_user_id: CANONICAL_USER.into(),
                    receipts: vec![NoteSyncUploadReceipt {
                        event_id: resolution.header.event_id.clone(),
                        server_sequence: 4,
                        duplicate: true,
                    }],
                },
            )
            .unwrap();
        }
        (
            receive_self_echo_at(connection, prepared, 4, inbox_updated_at),
            local,
        )
    }

    fn self_echo_fixture(
        strategy: &str,
        accepted: bool,
    ) -> (
        PrivilegedRemoteApplyConnection,
        VerifiedPeerResolutionCommand,
        String,
    ) {
        self_echo_fixture_at(strategy, accepted, None)
    }

    fn self_echo_fixture_at(
        strategy: &str,
        accepted: bool,
        inbox_updated_at: Option<&str>,
    ) -> (
        PrivilegedRemoteApplyConnection,
        VerifiedPeerResolutionCommand,
        String,
    ) {
        let mut connection = database();
        let (command, local) =
            prepare_self_echo_at(&mut connection, strategy, accepted, inbox_updated_at);
        (connection, command, local)
    }

    fn self_echo_ipc(
        command: &VerifiedPeerResolutionCommand,
    ) -> ReconcileVerifiedReceivedResolutionSelfEchoIpcCommand {
        ReconcileVerifiedReceivedResolutionSelfEchoIpcCommand {
            account_id: command.account_id.clone(),
            canonical_user_id: command.canonical_user_id.clone(),
            pulling_device_id: command.pulling_device_id.clone(),
            event_id: command.event_id.clone(),
            server_sequence: command.server_sequence,
            source_device_id: command.source_device_id.clone(),
            crypto_version: command.crypto_version,
            aad_version: command.aad_version,
            nonce: command.nonce.clone(),
            ciphertext: command.ciphertext.clone(),
            plaintext: command.canonical_payload.clone(),
        }
    }

    fn prepare_sealed_other_event(connection: &mut PrivilegedRemoteApplyConnection) -> String {
        let original: String = connection
            .connection()
            .query_row("SELECT payload_json FROM notes WHERE id='n'", [], |row| {
                row.get(0)
            })
            .unwrap();
        let snapshot = original.replace("\"id\":\"n\"", "\"id\":\"other\"");
        let event_id = {
            let transaction = connection.connection_mut_for_test().transaction().unwrap();
            let intent = prepare_unsealed_note_intent(
                &transaction,
                PrepareNoteIntent {
                    project_id: "p",
                    entity_id: "other",
                    operation: NoteSyncOperation::Upsert,
                    updated_at: "2026-01-04T00:00:00.000000Z",
                    deleted_at: None,
                    snapshot_json: &snapshot,
                    state_updated_at: "now",
                },
            )
            .unwrap()
            .unwrap();
            transaction.commit().unwrap();
            intent.event_id
        };
        let generation: i64 = connection
            .connection()
            .query_row(
                "SELECT mutation_generation FROM cloud_sync_note_intents WHERE event_id=?1",
                [&event_id],
                |row| row.get(0),
            )
            .unwrap();
        commit_sealed_note_sync_event(
            connection.connection_mut_for_test(),
            &CommitSealedNoteSyncEventCommand {
                event_id: event_id.clone(),
                expected_mutation_generation: generation,
                envelope: EncryptedNoteSyncEnvelope {
                    crypto_version: 1,
                    aad_version: 1,
                    nonce: "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEB".into(),
                    ciphertext: "AgICAgICAgICAgICAgICAg".into(),
                },
            },
        )
        .unwrap();
        event_id
    }

    #[test]
    fn remote_apply_create_updates_note_head_and_inbox_without_echo() {
        let mut connection = database();
        apply_create(&mut connection);
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT json_extract(payload_json,'$.content') FROM notes WHERE id='n'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "create"
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT head_event_id,head_sync_revision FROM cloud_sync_entities",
                    [],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
                )
                .unwrap(),
            (CREATE_EVENT.to_string(), 1)
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT state FROM cloud_sync_inbox WHERE event_id=?1",
                    [CREATE_EVENT],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "applied"
        );
        assert_eq!(
            connection
                .connection()
                .query_row("SELECT count(*) FROM cloud_sync_outbox", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT pull_cursor,ack_cursor FROM cloud_sync_state",
                    [],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
                )
                .unwrap(),
            (0, 0)
        );
    }

    #[test]
    fn remote_apply_accepts_equivalent_transport_timestamp_precision() {
        let mut connection = database();
        let command = received(
            &connection,
            plaintext(
                CREATE_EVENT,
                None,
                1,
                "upsert",
                "2026-01-01T00:00:00.000000Z",
                "transport precision",
            ),
            1,
            REMOTE_DEVICE,
        );
        connection
            .connection()
            .execute(
                "UPDATE cloud_sync_inbox SET updated_at='2026-01-01T00:00:00Z'
             WHERE event_id=?1",
                [CREATE_EVENT],
            )
            .unwrap();
        assert_eq!(
            apply_verified_received_note(&mut connection, &command).unwrap(),
            ApplyVerifiedReceivedNoteResult::Applied,
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT state FROM cloud_sync_inbox WHERE event_id=?1",
                    [CREATE_EVENT],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "applied"
        );
    }

    #[test]
    fn remote_apply_create_does_not_overwrite_unproven_existing_note() {
        let mut connection = database();
        let local_plaintext = plaintext(
            CREATE_EVENT,
            None,
            1,
            "upsert",
            "2026-01-01T00:00:00.000000Z",
            "local without remote head",
        );
        let payload_json = match &local_plaintext {
            NoteSyncPlaintext::Create { note, .. } => note_payload_from_record(note).unwrap(),
            _ => unreachable!(),
        };
        connection
            .authorize_once(
                RemoteApplyAuthorization {
                    event_id: CREATE_EVENT,
                    account_id: ACCOUNT,
                    project_id: "p",
                    entity_id: "n",
                    operation: "upsert",
                    payload_json: Some(&payload_json),
                    prior_payload_json: None,
                },
                |transaction| {
                    transaction.execute(
                        "INSERT INTO notes(id,project_id,stage_id,updated_at,payload_json)
                     VALUES('n','p',NULL,'2026-01-01T00:00:00.000000Z',?1)",
                        [&payload_json],
                    )?;
                    Ok(())
                },
            )
            .unwrap();

        let command = received(
            &connection,
            plaintext(
                CREATE_EVENT,
                None,
                1,
                "upsert",
                "2026-01-01T00:00:00.000000Z",
                "remote replacement",
            ),
            1,
            REMOTE_DEVICE,
        );
        assert_eq!(
            apply_verified_received_note(&mut connection, &command).unwrap(),
            ApplyVerifiedReceivedNoteResult::Conflict,
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT json_extract(payload_json,'$.content') FROM notes WHERE id='n'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "local without remote head"
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT state,error_code FROM cloud_sync_inbox WHERE event_id=?1",
                    [CREATE_EVENT],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .unwrap(),
            ("conflict".to_string(), "conflicting_head".to_string())
        );
        assert_eq!(
            connection
                .connection()
                .query_row("SELECT count(*) FROM cloud_sync_entities", [], |row| row
                    .get::<_, i64>(0),)
                .unwrap(),
            0
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT count(*) FROM cloud_sync_remote_apply_authorizations",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
    }

    #[test]
    fn remote_apply_update_and_delete_advance_chain_atomically() {
        let mut connection = database();
        apply_create(&mut connection);
        let update = received(
            &connection,
            plaintext(
                UPDATE_EVENT,
                Some(CREATE_EVENT),
                2,
                "upsert",
                "2026-01-02T00:00:00.000000Z",
                "update",
            ),
            2,
            REMOTE_DEVICE,
        );
        assert_eq!(
            apply_verified_received_note(&mut connection, &update).unwrap(),
            ApplyVerifiedReceivedNoteResult::Applied
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT json_extract(payload_json,'$.content') FROM notes WHERE id='n'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "update"
        );
        let delete = received(
            &connection,
            plaintext(
                DELETE_EVENT,
                Some(UPDATE_EVENT),
                3,
                "delete",
                "2026-01-03T00:00:00.000000Z",
                "",
            ),
            3,
            REMOTE_DEVICE,
        );
        assert_eq!(
            apply_verified_received_note(&mut connection, &delete).unwrap(),
            ApplyVerifiedReceivedNoteResult::Applied
        );
        assert_eq!(
            connection
                .connection()
                .query_row("SELECT count(*) FROM notes", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT head_event_id,head_sync_revision FROM cloud_sync_entities",
                    [],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
                )
                .unwrap(),
            (DELETE_EVENT.to_string(), 3)
        );
    }

    #[test]
    fn remote_apply_replay_is_idempotent() {
        let mut connection = database();
        let command = apply_create(&mut connection);
        assert_eq!(
            apply_verified_received_note(&mut connection, &command).unwrap(),
            ApplyVerifiedReceivedNoteResult::AlreadyApplied
        );
        let mut conflicting = command.clone();
        conflicting.plaintext = plaintext(
            CREATE_EVENT,
            None,
            1,
            "upsert",
            "2026-01-01T00:00:00.000000Z",
            "same event id, different plaintext",
        );
        assert!(apply_verified_received_note(&mut connection, &conflicting).is_err());
        assert_eq!(
            connection
                .connection()
                .query_row("SELECT count(*) FROM notes", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            1
        );
    }

    #[test]
    fn concurrent_remote_tips_are_durably_preserved_from_causal_history() {
        let mut connection = database();
        apply_create(&mut connection);
        let first = received(
            &connection,
            plaintext(
                UPDATE_EVENT,
                Some(CREATE_EVENT),
                2,
                "upsert",
                "2026-01-02T00:00:00.000000Z",
                "first remote tip",
            ),
            2,
            REMOTE_DEVICE,
        );
        assert_eq!(
            apply_verified_received_note(&mut connection, &first).unwrap(),
            ApplyVerifiedReceivedNoteResult::Applied
        );
        let sibling = received(
            &connection,
            plaintext(
                DELETE_EVENT,
                Some(CREATE_EVENT),
                2,
                "delete",
                "2026-01-02T00:00:01.000000Z",
                "",
            ),
            3,
            "123e4567-e89b-42d3-a456-426614174003",
        );
        assert_eq!(
            apply_verified_received_note(&mut connection, &sibling).unwrap(),
            ApplyVerifiedReceivedNoteResult::Conflict
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT state FROM cloud_sync_inbox WHERE event_id=?1",
                    [DELETE_EVENT],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "conflict_preserved"
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT group_concat(source,',') FROM (
                SELECT source FROM cloud_sync_note_conflict_versions ORDER BY source
             )",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "remote,remote_applied"
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT json_extract(payload_json,'$.content') FROM notes WHERE id='n'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "first remote tip"
        );
    }

    #[test]
    fn remote_apply_self_echo_uses_receipt_evidence_and_preserves_newer_local_note() {
        let mut connection = database();
        let newer_payload = serde_json::json!({
            "id":"n","project_id":"p","stage_id":null,"source_type":"project",
            "source_map_id":null,"source_node_id":null,"content_format":"html",
            "title":"Title","content":"newer local","checklist":[],"color":"default",
            "pinned":false,"archived":false,"sort_order":0,"tags":[],
            "created_at":"2026-01-01T00:00:00.000000Z",
            "updated_at":"2026-01-02T00:00:00.000000Z","revision":0,"metadata":{},
        })
        .to_string();
        // Seed a pre-binding local row, then create the real unsealed local
        // sidecar after binding; this avoids using Notes CRUD to fake remote IO.
        connection
            .connection()
            .execute(
                "DELETE FROM cloud_sync_project_bindings WHERE project_id='p'",
                [],
            )
            .unwrap();
        connection
            .connection()
            .execute(
                "INSERT INTO notes(id,project_id,stage_id,updated_at,payload_json)
             VALUES('n','p',NULL,'2026-01-02T00:00:00.000000Z',?1)",
                [&newer_payload],
            )
            .unwrap();
        connection.connection().execute(
            "INSERT INTO cloud_sync_project_bindings(project_id,account_id,created_at,updated_at)
             VALUES('p',?1,'now','now')",
            [ACCOUNT],
        ).unwrap();
        connection
            .connection()
            .execute(
                "INSERT INTO cloud_sync_outbox(
                event_id,account_id,device_id,project_id,entity_id,entity_type,operation,
                revision,updated_at,deleted_at,created_at,parent_event_id,local_ordinal,lifecycle
             ) VALUES(?1,?2,?3,'p','n','note','upsert',1,
                '2026-01-01T00:00:00.000000Z',NULL,'now',NULL,1,'accepted')",
                rusqlite::params![CREATE_EVENT, ACCOUNT, PULLING_DEVICE],
            )
            .unwrap();
        connection
            .connection()
            .execute(
                "INSERT INTO cloud_sync_upload_receipts(
                account_id,event_id,device_id,server_sequence,duplicate,accepted_at
             ) VALUES(?1,?2,?3,1,0,'now')",
                rusqlite::params![ACCOUNT, CREATE_EVENT, PULLING_DEVICE],
            )
            .unwrap();
        connection
            .connection()
            .execute(
                "INSERT INTO cloud_sync_outbox(
                event_id,account_id,device_id,project_id,entity_id,entity_type,operation,
                revision,updated_at,deleted_at,created_at,parent_event_id,local_ordinal,lifecycle
             ) VALUES('123e4567-e89b-42d3-a456-426614174110',?1,?2,'p','n','note','upsert',2,
                '2026-01-02T00:00:00.000000Z',NULL,'now',?3,2,'unsealed')",
                rusqlite::params![ACCOUNT, PULLING_DEVICE, CREATE_EVENT],
            )
            .unwrap();
        connection.connection().execute(
            "INSERT INTO cloud_sync_note_intents(
                event_id,mutation_generation,snapshot_json,seal_state,seal_attempt_count,state_updated_at
             ) VALUES('123e4567-e89b-42d3-a456-426614174110',1,?1,'pending',0,'now')",
            [&newer_payload],
        ).unwrap();

        let command = received(
            &connection,
            plaintext(
                CREATE_EVENT,
                None,
                1,
                "upsert",
                "2026-01-01T00:00:00.000000Z",
                "server copy",
            ),
            1,
            PULLING_DEVICE,
        );
        assert_eq!(
            apply_verified_received_note(&mut connection, &command).unwrap(),
            ApplyVerifiedReceivedNoteResult::SelfEchoApplied
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT json_extract(payload_json,'$.content') FROM notes WHERE id='n'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "newer local"
        );
        assert_eq!(
            connection
                .connection()
                .query_row("SELECT count(*) FROM cloud_sync_outbox", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT state FROM cloud_sync_inbox WHERE event_id=?1",
                    [CREATE_EVENT],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "applied"
        );
    }

    fn received_accepted_local_branch(
        connection: &mut PrivilegedRemoteApplyConnection,
        operation: NoteSyncOperation,
        stored_updated_at: &str,
        stored_deleted_at: Option<&str>,
    ) -> (String, ApplyVerifiedReceivedNoteCommand) {
        apply_create(connection);
        let event_id = UPDATE_EVENT.to_string();
        let opened = plaintext(
            &event_id,
            Some(CREATE_EVENT),
            2,
            operation.as_str(),
            "2026-01-02T00:00:00.000000Z",
            "local branch",
        );
        connection
            .connection()
            .execute(
                "DELETE FROM cloud_sync_project_bindings WHERE project_id='p'",
                [],
            )
            .unwrap();
        match &opened {
            NoteSyncPlaintext::Update { note, .. } => {
                let snapshot = note_payload_from_record(note).unwrap();
                connection
                    .connection()
                    .execute("UPDATE notes SET payload_json=?1 WHERE id='n'", [&snapshot])
                    .unwrap();
            }
            NoteSyncPlaintext::Delete { .. } => {
                connection
                    .connection()
                    .execute("DELETE FROM notes WHERE id='n'", [])
                    .unwrap();
            }
            _ => unreachable!(),
        }
        connection.connection().execute(
            "INSERT INTO cloud_sync_project_bindings(project_id,account_id,created_at,updated_at)
             VALUES('p',?1,'now','now')", [ACCOUNT],
        ).unwrap();
        connection
            .connection()
            .execute(
                "INSERT INTO cloud_sync_outbox(
                event_id,account_id,device_id,project_id,entity_id,entity_type,operation,
                revision,updated_at,deleted_at,created_at,parent_event_id,local_ordinal,lifecycle
             ) VALUES(?1,?2,?3,'p','n','note',?4,2,?5,?6,'now',?7,1,'accepted')",
                rusqlite::params![
                    event_id,
                    ACCOUNT,
                    PULLING_DEVICE,
                    operation.as_str(),
                    stored_updated_at,
                    stored_deleted_at,
                    CREATE_EVENT
                ],
            )
            .unwrap();
        connection
            .connection()
            .execute(
                "INSERT INTO cloud_sync_upload_receipts(
                account_id,event_id,device_id,server_sequence,duplicate,accepted_at
             ) VALUES(?1,?2,?3,2,0,'now')",
                rusqlite::params![ACCOUNT, event_id, PULLING_DEVICE],
            )
            .unwrap();
        let command = received(connection, opened, 2, PULLING_DEVICE);
        (event_id, command)
    }

    #[test]
    fn ordinary_upsert_self_echo_accepts_equivalent_durable_timestamp() {
        let mut connection = database();
        let (event_id, command) = received_accepted_local_branch(
            &mut connection,
            NoteSyncOperation::Upsert,
            "2026-01-02T00:00:00Z",
            None,
        );
        let note_before: String = connection
            .connection()
            .query_row("SELECT payload_json FROM notes WHERE id='n'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            apply_verified_received_note(&mut connection, &command).unwrap(),
            ApplyVerifiedReceivedNoteResult::SelfEchoApplied
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT state FROM cloud_sync_inbox WHERE event_id=?1",
                    [&event_id],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "applied"
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT head_event_id,head_sync_revision FROM cloud_sync_entities",
                    [],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
                )
                .unwrap(),
            (event_id, 2)
        );
        assert_eq!(
            connection
                .connection()
                .query_row("SELECT payload_json FROM notes WHERE id='n'", [], |row| row
                    .get::<_, String>(0),)
                .unwrap(),
            note_before
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT count(*) FROM cloud_sync_note_conflict_groups",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT count(*) FROM cloud_sync_note_conflict_versions",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
    }

    #[test]
    fn ordinary_self_echo_rejects_a_different_durable_instant() {
        let mut connection = database();
        let (event_id, command) = received_accepted_local_branch(
            &mut connection,
            NoteSyncOperation::Upsert,
            "2026-01-02T00:00:01Z",
            None,
        );
        assert_ne!(
            apply_verified_received_note(&mut connection, &command).unwrap(),
            ApplyVerifiedReceivedNoteResult::SelfEchoApplied
        );
        assert_ne!(
            connection
                .connection()
                .query_row("SELECT head_event_id FROM cloud_sync_entities", [], |row| {
                    row.get::<_, String>(0)
                },)
                .unwrap(),
            event_id
        );
    }

    #[test]
    fn ordinary_delete_self_echo_proves_both_equivalent_timestamps() {
        let mut connection = database();
        let (event_id, command) = received_accepted_local_branch(
            &mut connection,
            NoteSyncOperation::Delete,
            "2026-01-02T00:00:00Z",
            Some("2026-01-02T00:00:00Z"),
        );
        assert_eq!(
            apply_verified_received_note(&mut connection, &command).unwrap(),
            ApplyVerifiedReceivedNoteResult::SelfEchoApplied
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT head_event_id,head_sync_revision FROM cloud_sync_entities",
                    [],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
                )
                .unwrap(),
            (event_id, 2)
        );
        assert_eq!(
            connection
                .connection()
                .query_row("SELECT count(*) FROM notes WHERE id='n'", [], |row| row
                    .get::<_, i64>(0),)
                .unwrap(),
            0
        );
    }

    fn change_visible_note_for_conflict(
        connection: &PrivilegedRemoteApplyConnection,
        change: impl FnOnce(&mut serde_json::Value),
    ) {
        connection
            .connection()
            .execute(
                "DELETE FROM cloud_sync_project_bindings WHERE project_id='p'",
                [],
            )
            .unwrap();
        let payload: String = connection
            .connection()
            .query_row("SELECT payload_json FROM notes WHERE id='n'", [], |row| {
                row.get(0)
            })
            .unwrap();
        let mut value: serde_json::Value = serde_json::from_str(&payload).unwrap();
        change(&mut value);
        connection
            .connection()
            .execute(
                "UPDATE notes SET payload_json=?1 WHERE id='n'",
                [value.to_string()],
            )
            .unwrap();
        connection.connection().execute(
            "INSERT INTO cloud_sync_project_bindings(project_id,account_id,created_at,updated_at)
             VALUES('p',?1,'now','now')", [ACCOUNT],
        ).unwrap();
    }

    #[test]
    fn accepted_self_echo_with_local_revision_preserves_remote_sibling() {
        let mut connection = database();
        let (local_id, own_echo) = received_accepted_local_branch(
            &mut connection,
            NoteSyncOperation::Upsert,
            "2026-01-02T00:00:00Z",
            None,
        );
        change_visible_note_for_conflict(&connection, |note| {
            note["revision"] = serde_json::Value::from(1);
            note["created_at"] = serde_json::Value::from("2026-01-01T00:00:00Z");
            note["updated_at"] = serde_json::Value::from("2026-01-02T00:00:00Z");
        });
        assert_eq!(
            apply_verified_received_note(&mut connection, &own_echo).unwrap(),
            ApplyVerifiedReceivedNoteResult::SelfEchoApplied
        );
        let sibling = received(
            &connection,
            plaintext(
                DELETE_EVENT,
                Some(CREATE_EVENT),
                2,
                "upsert",
                "2026-01-02T00:00:01.000000Z",
                "other branch",
            ),
            3,
            REMOTE_DEVICE,
        );
        assert_eq!(
            apply_verified_received_note(&mut connection, &sibling).unwrap(),
            ApplyVerifiedReceivedNoteResult::Conflict
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT state FROM cloud_sync_inbox WHERE event_id=?1",
                    [DELETE_EVENT],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "conflict_preserved"
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT common_parent_event_id,tip_revision,generation,lifecycle
             FROM cloud_sync_note_conflict_groups",
                    [],
                    |row| Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?
                    )),
                )
                .unwrap(),
            (CREATE_EVENT.into(), 2, 1, "open".into())
        );
        let tips: Vec<(String, String)> = connection
            .connection()
            .prepare(
                "SELECT event_id,source FROM cloud_sync_note_conflict_versions ORDER BY event_id",
            )
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            tips,
            vec![
                (local_id, "remote_applied".into()),
                (DELETE_EVENT.into(), "remote".into())
            ]
            .into_iter()
            .collect::<Vec<_>>()
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT json_extract(payload_json,'$.content') FROM notes WHERE id='n'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "local branch"
        );
    }

    #[test]
    fn unrelated_visible_note_cannot_be_promoted_to_conflict_tip() {
        let mut connection = database();
        let (_, own_echo) = received_accepted_local_branch(
            &mut connection,
            NoteSyncOperation::Upsert,
            "2026-01-02T00:00:00Z",
            None,
        );
        assert_eq!(
            apply_verified_received_note(&mut connection, &own_echo).unwrap(),
            ApplyVerifiedReceivedNoteResult::SelfEchoApplied
        );
        change_visible_note_for_conflict(&connection, |note| {
            note["content"] = serde_json::Value::from("unrelated local content");
        });
        let sibling = received(
            &connection,
            plaintext(
                DELETE_EVENT,
                Some(CREATE_EVENT),
                2,
                "upsert",
                "2026-01-02T00:00:01.000000Z",
                "other branch",
            ),
            3,
            REMOTE_DEVICE,
        );
        assert_eq!(
            apply_verified_received_note(&mut connection, &sibling).unwrap(),
            ApplyVerifiedReceivedNoteResult::Conflict
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT state FROM cloud_sync_inbox WHERE event_id=?1",
                    [DELETE_EVENT],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "conflict"
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT count(*) FROM cloud_sync_note_conflict_groups",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
    }

    #[test]
    fn semantic_visible_snapshot_requires_route_content_and_valid_instants() {
        let source = plaintext(
            UPDATE_EVENT,
            Some(CREATE_EVENT),
            2,
            "upsert",
            "2026-01-02T00:00:00.000000Z",
            "local branch",
        );
        let NoteSyncPlaintext::Update { note, .. } = source else {
            unreachable!()
        };
        let history = note_payload_from_record(&note).unwrap();
        let mut visible: serde_json::Value = serde_json::from_str(&history).unwrap();
        visible["revision"] = serde_json::Value::from(42);
        visible["created_at"] = serde_json::Value::from("2025-12-31T19:00:00-05:00");
        visible["updated_at"] = serde_json::Value::from("2026-01-02T00:00:00Z");
        assert!(note_snapshot_semantically_matches_visible(
            &history,
            &visible.to_string(),
            "remote_applied"
        ));
        let mut local_intent = visible.clone();
        local_intent["created_at"] = serde_json::Value::from("2026-01-01T00:00:00.000000Z");
        local_intent["updated_at"] = serde_json::Value::from("2026-01-02T00:00:00.000000Z");
        assert!(note_snapshot_semantically_matches_visible(
            &local_intent.to_string(),
            &visible.to_string(),
            "local_unsealed"
        ));
        for (field, value) in [
            ("content", serde_json::Value::from("different")),
            ("project_id", serde_json::Value::from("other-project")),
            ("source_type", serde_json::Value::from("mindmap")),
            (
                "updated_at",
                serde_json::Value::from("2026-01-02T00:00:01Z"),
            ),
            ("created_at", serde_json::Value::from("not-a-timestamp")),
            ("revision", serde_json::Value::from(-1)),
        ] {
            let mut changed = visible.clone();
            changed[field] = value;
            assert!(
                !note_snapshot_semantically_matches_visible(
                    &history,
                    &changed.to_string(),
                    "remote_applied"
                ),
                "unexpected match after changing {field}"
            );
        }
        let mut extra = visible.clone();
        extra["unexpected"] = serde_json::Value::Null;
        assert!(!note_snapshot_semantically_matches_visible(
            &history,
            &extra.to_string(),
            "remote_applied"
        ));
        let mut missing = visible;
        missing.as_object_mut().unwrap().remove("title");
        assert!(!note_snapshot_semantically_matches_visible(
            &history,
            &missing.to_string(),
            "remote_applied"
        ));
    }

    #[test]
    fn remote_apply_classifies_conflicting_and_missing_parents() {
        let mut connection = database();
        apply_create(&mut connection);
        let conflict = received(
            &connection,
            plaintext(
                UPDATE_EVENT,
                Some(DELETE_EVENT),
                2,
                "upsert",
                "2026-01-02T00:00:00.000000Z",
                "conflict",
            ),
            2,
            REMOTE_DEVICE,
        );
        assert_eq!(
            apply_verified_received_note(&mut connection, &conflict).unwrap(),
            ApplyVerifiedReceivedNoteResult::Conflict
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT state FROM cloud_sync_inbox WHERE event_id=?1",
                    [UPDATE_EVENT],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            "conflict"
        );

        let mut other = database();
        let missing = received(
            &other,
            plaintext(
                UPDATE_EVENT,
                Some(CREATE_EVENT),
                2,
                "upsert",
                "2026-01-02T00:00:00.000000Z",
                "missing",
            ),
            2,
            REMOTE_DEVICE,
        );
        assert_eq!(
            apply_verified_received_note(&mut other, &missing).unwrap(),
            ApplyVerifiedReceivedNoteResult::Orphan
        );
        assert_eq!(
            other
                .connection()
                .query_row(
                    "SELECT state,error_code FROM cloud_sync_inbox WHERE event_id=?1",
                    [UPDATE_EVENT],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .unwrap(),
            ("orphan".to_string(), "missing_parent".to_string())
        );
    }

    #[test]
    fn remote_apply_preserves_local_unsealed_change_and_rejects_wrong_object() {
        let mut connection = database();
        apply_create(&mut connection);
        connection
            .connection()
            .execute(
                "INSERT INTO cloud_sync_outbox(
                event_id,account_id,device_id,project_id,entity_id,entity_type,operation,
                revision,updated_at,deleted_at,created_at,parent_event_id,local_ordinal,lifecycle
             ) VALUES('123e4567-e89b-42d3-a456-426614174110',?1,?2,'p','n','note','upsert',2,
                '2026-01-02T00:00:00.000000Z',NULL,'now',?3,1,'unsealed')",
                rusqlite::params![ACCOUNT, PULLING_DEVICE, CREATE_EVENT],
            )
            .unwrap();
        let conflict = received(
            &connection,
            plaintext(
                UPDATE_EVENT,
                Some(CREATE_EVENT),
                2,
                "upsert",
                "2026-01-02T00:00:00.000000Z",
                "remote",
            ),
            2,
            REMOTE_DEVICE,
        );
        assert_eq!(
            apply_verified_received_note(&mut connection, &conflict).unwrap(),
            ApplyVerifiedReceivedNoteResult::Conflict
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT json_extract(payload_json,'$.content') FROM notes WHERE id='n'",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            "create"
        );

        let mut mismatch = database();
        let mut command = received(
            &mismatch,
            plaintext(
                CREATE_EVENT,
                None,
                1,
                "upsert",
                "2026-01-01T00:00:00.000000Z",
                "create",
            ),
            1,
            REMOTE_DEVICE,
        );
        command.ciphertext[0] ^= 1;
        assert_eq!(
            apply_verified_received_note(&mut mismatch, &command).unwrap(),
            ApplyVerifiedReceivedNoteResult::Rejected
        );
        assert_eq!(
            mismatch
                .connection()
                .query_row("SELECT count(*) FROM notes", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn causal_edit_conflict_preserves_both_tips_and_allows_ack() {
        let mut connection = database();
        apply_create(&mut connection);
        let local = prepare_local_branch(&connection, NoteSyncOperation::Upsert, "local edit");
        let command = received(
            &connection,
            plaintext(
                UPDATE_EVENT,
                Some(CREATE_EVENT),
                2,
                "upsert",
                "2026-01-02T00:00:00.000000Z",
                "remote edit",
            ),
            2,
            REMOTE_DEVICE,
        );
        assert_eq!(
            apply_verified_received_note(&mut connection, &command).unwrap(),
            ApplyVerifiedReceivedNoteResult::Conflict
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT state FROM cloud_sync_inbox WHERE event_id=?1",
                    [UPDATE_EVENT],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "conflict_preserved"
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT count(*) FROM cloud_sync_note_conflict_versions",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            2
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT json_extract(snapshot_json,'$.content')
             FROM cloud_sync_note_conflict_versions WHERE source='local_unsealed'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "local edit"
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT local_mutation_generation FROM cloud_sync_note_conflict_versions
             WHERE source='local_unsealed'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            local.mutation_generation
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT json_extract(payload_json,'$.content') FROM notes WHERE id='n'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "local edit"
        );

        // Exact replay neither duplicates versions nor increments generation.
        assert_eq!(
            apply_verified_received_note(&mut connection, &command).unwrap(),
            ApplyVerifiedReceivedNoteResult::Conflict
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT generation FROM cloud_sync_note_conflict_groups",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );

        let coalesced = prepare_local_branch(
            &connection,
            NoteSyncOperation::Upsert,
            "local after preservation",
        );
        assert_eq!(coalesced.event_id, local.event_id);
        assert!(coalesced.mutation_generation > local.mutation_generation);
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT json_extract(snapshot_json,'$.content')
             FROM cloud_sync_note_conflict_versions WHERE source='local_unsealed'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "local edit"
        );

        connection
            .connection()
            .execute(
                "UPDATE cloud_sync_state SET pull_cursor=3 WHERE account_id=?1",
                [ACCOUNT],
            )
            .unwrap();
        connection
            .connection()
            .execute(
                "INSERT INTO cloud_sync_inbox(
                account_id,event_id,server_sequence,device_id,project_id,entity_id,
                entity_type,operation,sync_revision,updated_at,deleted_at,state,received_at
             ) VALUES(?1,'123e4567-e89b-42d3-a456-426614174105',3,?2,'p','independent',
                      'note','upsert',1,'2026-01-03T00:00:00.000000Z',NULL,'applied','now')",
                rusqlite::params![ACCOUNT, REMOTE_DEVICE],
            )
            .unwrap();
        let candidate = prepare_note_sync_ack(
            connection.connection_mut_for_test(),
            &PrepareNoteSyncAckCommand {
                account_id: ACCOUNT.into(),
                device_id: PULLING_DEVICE.into(),
                canonical_user_id: CANONICAL_USER.into(),
            },
        )
        .unwrap();
        assert_eq!(candidate.candidate_cursor, 3);
        assert_eq!(
            commit_note_sync_ack(
                connection.connection_mut_for_test(),
                &CommitNoteSyncAckCommand {
                    account_id: ACCOUNT.into(),
                    device_id: PULLING_DEVICE.into(),
                    canonical_user_id: CANONICAL_USER.into(),
                    expected_old_ack_cursor: 0,
                    acknowledged_cursor: 3,
                },
            )
            .unwrap(),
            CommitNoteSyncAckResult::Advanced
        );
    }

    #[test]
    fn preserved_conflict_survives_database_restart() {
        let (root, path) = conflict_database_path("conflict-restart");
        let raw = Connection::open(&path).unwrap();
        let mut connection = configured_database(raw);
        apply_create(&mut connection);
        prepare_local_branch(&connection, NoteSyncOperation::Upsert, "restart local");
        let command = received(
            &connection,
            plaintext(
                UPDATE_EVENT,
                Some(CREATE_EVENT),
                2,
                "upsert",
                "2026-01-02T00:00:00.000000Z",
                "restart remote",
            ),
            2,
            REMOTE_DEVICE,
        );
        apply_verified_received_note(&mut connection, &command).unwrap();
        drop(connection);

        let reopened = Connection::open(&path).unwrap();
        assert_eq!(
            reopened
                .query_row(
                    "SELECT state FROM cloud_sync_inbox WHERE event_id=?1",
                    [UPDATE_EVENT],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "conflict_preserved"
        );
        assert_eq!(
            reopened
                .query_row(
                    "SELECT count(*) FROM cloud_sync_note_conflict_versions",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            2
        );
        assert_eq!(
            reopened
                .query_row(
                    "SELECT json_extract(payload_json,'$.content') FROM notes WHERE id='n'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "restart local"
        );
        drop(reopened);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn causal_delete_edit_conflict_keeps_tombstone_and_visible_remote_is_not_applied() {
        let mut connection = database();
        apply_create(&mut connection);
        prepare_local_branch(&connection, NoteSyncOperation::Delete, "");
        let command = received(
            &connection,
            plaintext(
                UPDATE_EVENT,
                Some(CREATE_EVENT),
                2,
                "upsert",
                "2026-01-02T00:00:00.000000Z",
                "remote survives",
            ),
            2,
            REMOTE_DEVICE,
        );
        assert_eq!(
            apply_verified_received_note(&mut connection, &command).unwrap(),
            ApplyVerifiedReceivedNoteResult::Conflict
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT operation,json_extract(snapshot_json,'$.deleted_at')
             FROM cloud_sync_note_conflict_versions WHERE source='local_unsealed'",
                    [],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .unwrap(),
            ("delete".into(), "2026-01-02T00:00:00.000000Z".into())
        );
        assert_eq!(
            connection
                .connection()
                .query_row("SELECT count(*) FROM notes", [], |row| row.get::<_, i64>(0),)
                .unwrap(),
            0
        );
    }

    #[test]
    fn preserved_group_accepts_multiple_causal_tips_and_rolls_back_injected_failure() {
        let mut connection = database();
        apply_create(&mut connection);
        prepare_local_branch(&connection, NoteSyncOperation::Upsert, "local");
        let first = received(
            &connection,
            plaintext(
                UPDATE_EVENT,
                Some(CREATE_EVENT),
                2,
                "upsert",
                "2026-01-02T00:00:00.000000Z",
                "remote one",
            ),
            2,
            REMOTE_DEVICE,
        );
        apply_verified_received_note(&mut connection, &first).unwrap();
        let third_event = "123e4567-e89b-42d3-a456-426614174103";
        let third = received(
            &connection,
            plaintext(
                third_event,
                Some(CREATE_EVENT),
                2,
                "delete",
                "2026-01-02T00:00:01.000000Z",
                "",
            ),
            3,
            REMOTE_DEVICE,
        );
        assert_eq!(
            apply_verified_received_note(&mut connection, &third).unwrap(),
            ApplyVerifiedReceivedNoteResult::Conflict
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT generation FROM cloud_sync_note_conflict_groups",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            2
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT count(*) FROM cloud_sync_note_conflict_tips",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            3
        );

        let fourth_event = "123e4567-e89b-42d3-a456-426614174104";
        let fourth = received(
            &connection,
            plaintext(
                fourth_event,
                Some(CREATE_EVENT),
                2,
                "upsert",
                "2026-01-02T00:00:02.000000Z",
                "rollback",
            ),
            4,
            REMOTE_DEVICE,
        );
        connection
            .connection()
            .execute_batch(
                "CREATE TRIGGER conflict_preservation_test_fail
             BEFORE INSERT ON cloud_sync_note_conflict_tips
             WHEN NEW.event_id='123e4567-e89b-42d3-a456-426614174104'
             BEGIN SELECT RAISE(ABORT,'injected conflict preservation failure'); END;",
            )
            .unwrap();
        assert!(apply_verified_received_note(&mut connection, &fourth).is_err());
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT generation FROM cloud_sync_note_conflict_groups",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            2
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT state FROM cloud_sync_inbox WHERE event_id=?1",
                    [fourth_event],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "received"
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT count(*) FROM cloud_sync_note_conflict_versions WHERE event_id=?1",
                    [fourth_event],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
    }

    #[test]
    fn multigeneration_tip_proof_completes_and_controls_ack() {
        let mut connection = database();
        let resolution_event = seed_three_tip_applying_resolution(&mut connection);
        let ack = PrepareNoteSyncAckCommand {
            account_id: ACCOUNT.into(),
            device_id: PULLING_DEVICE.into(),
            canonical_user_id: CANONICAL_USER.into(),
        };
        assert_eq!(
            prepare_note_sync_ack(connection.connection_mut_for_test(), &ack)
                .unwrap()
                .candidate_cursor,
            4,
        );
        let transaction = connection
            .connection_mut_for_test()
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        transaction
            .execute(
                "UPDATE cloud_sync_note_applied_resolutions
             SET lifecycle='applied',applied_at='applied'
             WHERE account_id=?1 AND resolution_event_id=?2",
                rusqlite::params![ACCOUNT, resolution_event],
            )
            .unwrap();
        transaction
            .execute(
                "UPDATE cloud_sync_inbox SET state='applied',applied_at='applied'
             WHERE account_id=?1 AND event_id=?2",
                rusqlite::params![ACCOUNT, resolution_event],
            )
            .unwrap();
        transaction.commit().unwrap();
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT count(*) FROM cloud_sync_note_applied_resolution_parents
             WHERE resolution_event_id=?1",
                    [&resolution_event],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            3
        );
        assert_eq!(
            prepare_note_sync_ack(connection.connection_mut_for_test(), &ack)
                .unwrap()
                .candidate_cursor,
            5,
        );
        assert_eq!(
            connection
                .connection()
                .query_row("PRAGMA foreign_key_check", [], |_| Ok(1_i64),)
                .optional()
                .unwrap(),
            None
        );
    }

    #[test]
    fn multigeneration_tip_proof_rejects_newer_group_generation() {
        let mut connection = database();
        let resolution_event = seed_three_tip_applying_resolution(&mut connection);
        let fourth_event = "123e4567-e89b-42d3-a456-426614174104";
        let fourth = received(
            &connection,
            plaintext(
                fourth_event,
                Some(CREATE_EVENT),
                2,
                "upsert",
                "2026-01-02T00:00:02.000000Z",
                "remote three",
            ),
            6,
            REMOTE_DEVICE,
        );
        assert_eq!(
            apply_verified_received_note(&mut connection, &fourth).unwrap(),
            ApplyVerifiedReceivedNoteResult::Conflict,
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT generation FROM cloud_sync_note_conflict_groups",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            3
        );
        let transaction = connection
            .connection_mut_for_test()
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        assert!(transaction
            .execute(
                "UPDATE cloud_sync_note_applied_resolutions
             SET lifecycle='applied',applied_at='applied'
             WHERE account_id=?1 AND resolution_event_id=?2",
                rusqlite::params![ACCOUNT, resolution_event],
            )
            .is_err());
        transaction.rollback().unwrap();
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT lifecycle FROM cloud_sync_note_applied_resolutions
             WHERE resolution_event_id=?1",
                    [&resolution_event],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "applying"
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT state FROM cloud_sync_inbox WHERE event_id=?1",
                    [&resolution_event],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "received"
        );
    }

    #[test]
    fn peer_resolution_applies_all_strategies_with_heads_ledger_and_ack() {
        for (strategy, selected_remote, expected_content, note_count) in [
            ("choose_version", true, Some("remote edit"), 1_i64),
            ("manual_merge", false, Some("manual merge"), 1_i64),
            ("keep_both", false, Some("local edit"), 2_i64),
            ("delete", false, None, 0_i64),
        ] {
            let (mut connection, command, _, _) =
                peer_resolution_fixture(strategy, selected_remote);
            let ack = PrepareNoteSyncAckCommand {
                account_id: ACCOUNT.into(),
                device_id: PULLING_DEVICE.into(),
                canonical_user_id: CANONICAL_USER.into(),
            };
            assert_eq!(
                prepare_note_sync_ack(connection.connection_mut_for_test(), &ack)
                    .unwrap()
                    .candidate_cursor,
                3
            );
            let outbox_before: i64 = connection
                .connection()
                .query_row("SELECT count(*) FROM cloud_sync_outbox", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(
                apply_verified_received_resolution_v2_ipc(&mut connection, peer_ipc(&command))
                    .unwrap(),
                ApplyVerifiedReceivedResolutionV2Result::Applied,
            );
            assert_eq!(
                connection
                    .connection()
                    .query_row("SELECT count(*) FROM notes", [], |row| row.get::<_, i64>(0),)
                    .unwrap(),
                note_count
            );
            if let Some(content) = expected_content {
                assert_eq!(
                    connection
                        .connection()
                        .query_row(
                            "SELECT json_extract(payload_json,'$.content') FROM notes WHERE id='n'",
                            [],
                            |row| row.get::<_, String>(0),
                        )
                        .unwrap(),
                    content
                );
            }
            let (remote_group, local_group, remote_generation, local_generation, lifecycle): (
                String,
                String,
                i64,
                i64,
                String,
            ) = connection
                .connection()
                .query_row(
                    "SELECT remote_conflict_group_id,local_conflict_group_id,
                            remote_conflict_generation,local_conflict_generation,lifecycle
                     FROM cloud_sync_note_applied_resolutions",
                    [],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                        ))
                    },
                )
                .unwrap();
            assert_ne!(remote_group, local_group);
            assert_eq!(
                (remote_generation, local_generation, lifecycle),
                (77, 1, "applied".into())
            );
            assert_eq!(
                connection
                    .connection()
                    .query_row(
                        "SELECT state FROM cloud_sync_inbox WHERE event_id=?1",
                        [&command.event_id],
                        |row| row.get::<_, String>(0),
                    )
                    .unwrap(),
                "applied"
            );
            assert_eq!(
                connection
                    .connection()
                    .query_row(
                        "SELECT lifecycle FROM cloud_sync_note_conflict_groups",
                        [],
                        |row| row.get::<_, String>(0),
                    )
                    .unwrap(),
                "resolved"
            );
            assert_eq!(
                connection
                    .connection()
                    .query_row("SELECT count(*) FROM cloud_sync_outbox", [], |row| row
                        .get::<_, i64>(0),)
                    .unwrap(),
                outbox_before
            );
            assert_eq!(
                connection
                    .connection()
                    .query_row(
                        "SELECT count(*) FROM cloud_sync_note_resolution_outbox",
                        [],
                        |row| row.get::<_, i64>(0),
                    )
                    .unwrap(),
                0
            );
            assert_eq!(
                connection
                    .connection()
                    .query_row(
                        "SELECT head_event_id,head_sync_revision FROM cloud_sync_entities
                 WHERE entity_id='n'",
                        [],
                        |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
                    )
                    .unwrap(),
                (command.event_id.clone(), 3)
            );
            let transaction = connection.connection().unchecked_transaction().unwrap();
            assert_eq!(
                read_entity_sync_head(&transaction, ACCOUNT, "p", "n").unwrap(),
                Some(EntitySyncHead {
                    event_id: command.event_id.clone(),
                    revision: 3
                },)
            );
            if strategy == "keep_both" {
                assert_eq!(
                    transaction
                        .query_row(
                            "SELECT head_event_id,head_sync_revision FROM cloud_sync_entities
                     WHERE entity_id='n-copy'",
                            [],
                            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
                        )
                        .unwrap(),
                    (command.event_id.clone(), 3)
                );
                assert_eq!(
                    read_entity_sync_head(&transaction, ACCOUNT, "p", "n-copy").unwrap(),
                    Some(EntitySyncHead {
                        event_id: command.event_id.clone(),
                        revision: 3
                    },)
                );
            }
            transaction.rollback().unwrap();
            assert_eq!(
                prepare_note_sync_ack(connection.connection_mut_for_test(), &ack)
                    .unwrap()
                    .candidate_cursor,
                4
            );
        }
    }

    #[test]
    fn peer_resolution_accepts_equivalent_inbox_timestamp_and_rejects_other_instants() {
        let (mut connection, command, _, _) =
            peer_resolution_fixture_at("manual_merge", false, Some("2026-01-03T00:00:00Z"));
        assert_eq!(
            apply_verified_received_resolution_v2_ipc(&mut connection, peer_ipc(&command)).unwrap(),
            ApplyVerifiedReceivedResolutionV2Result::Applied
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT state FROM cloud_sync_inbox WHERE event_id=?1",
                    [&command.event_id],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "applied"
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT lifecycle FROM cloud_sync_note_conflict_groups",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "resolved"
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT event_updated_at FROM cloud_sync_note_applied_resolutions",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "2026-01-03T00:00:00Z"
        );
        let ack = PrepareNoteSyncAckCommand {
            account_id: ACCOUNT.into(),
            device_id: PULLING_DEVICE.into(),
            canonical_user_id: CANONICAL_USER.into(),
        };
        assert_eq!(
            prepare_note_sync_ack(connection.connection_mut_for_test(), &ack)
                .unwrap()
                .candidate_cursor,
            4
        );

        for changed_at in ["2026-01-03T00:00:01Z", "invalid"] {
            let (mut mismatch, command, _, _) =
                peer_resolution_fixture_at("manual_merge", false, Some(changed_at));
            assert!(matches!(
                apply_verified_received_resolution_v2_ipc(&mut mismatch, peer_ipc(&command)),
                Err(ApplyVerifiedReceivedResolutionV2Error::ImmutableMismatch)
            ));
            assert_eq!(
                mismatch
                    .connection()
                    .query_row(
                        "SELECT state FROM cloud_sync_inbox WHERE event_id=?1",
                        [&command.event_id],
                        |row| row.get::<_, String>(0),
                    )
                    .unwrap(),
                "received"
            );
        }
    }

    #[test]
    fn resolution_self_echo_accepts_equivalent_inbox_timestamp_once() {
        let (mut connection, command, _) =
            self_echo_fixture_at("manual_merge", true, Some("2026-01-03T00:00:00Z"));
        let before: String = connection
            .connection()
            .query_row("SELECT payload_json FROM notes WHERE id='n'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            reconcile_verified_received_resolution_self_echo_ipc(
                &mut connection,
                self_echo_ipc(&command)
            )
            .unwrap(),
            ReconcileVerifiedReceivedResolutionSelfEchoResult::Reconciled
        );
        assert_eq!(
            connection
                .connection()
                .query_row("SELECT payload_json FROM notes WHERE id='n'", [], |row| row
                    .get::<_, String>(0),)
                .unwrap(),
            before
        );
        assert_eq!(connection.connection().query_row(
            "SELECT lifecycle FROM cloud_sync_note_resolution_outbox WHERE resolution_event_id=?1",
            [&command.event_id],|row|row.get::<_,String>(0),
        ).unwrap(),"accepted");
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT state FROM cloud_sync_inbox WHERE event_id=?1",
                    [&command.event_id],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "applied"
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT lifecycle FROM cloud_sync_note_conflict_groups",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "resolved"
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT event_updated_at FROM cloud_sync_note_applied_resolutions",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "2026-01-03T00:00:00Z"
        );
        let ack = PrepareNoteSyncAckCommand {
            account_id: ACCOUNT.into(),
            device_id: PULLING_DEVICE.into(),
            canonical_user_id: CANONICAL_USER.into(),
        };
        assert_eq!(
            prepare_note_sync_ack(connection.connection_mut_for_test(), &ack)
                .unwrap()
                .candidate_cursor,
            4
        );
        assert_eq!(
            reconcile_verified_received_resolution_self_echo_ipc(
                &mut connection,
                self_echo_ipc(&command)
            )
            .unwrap(),
            ReconcileVerifiedReceivedResolutionSelfEchoResult::AlreadyReconciled
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT count(*) FROM cloud_sync_note_applied_resolutions",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );
    }

    #[test]
    fn resolution_self_echo_rejects_a_different_or_invalid_inbox_timestamp() {
        for changed_at in ["2026-01-03T00:00:01Z", "invalid"] {
            let (mut connection, command, _) =
                self_echo_fixture_at("manual_merge", true, Some(changed_at));
            assert!(matches!(
                reconcile_verified_received_resolution_self_echo_ipc(
                    &mut connection,
                    self_echo_ipc(&command)
                ),
                Err(ApplyVerifiedReceivedResolutionV2Error::ImmutableMismatch)
            ));
            assert_eq!(
                connection
                    .connection()
                    .query_row(
                        "SELECT state FROM cloud_sync_inbox WHERE event_id=?1",
                        [&command.event_id],
                        |row| row.get::<_, String>(0),
                    )
                    .unwrap(),
                "received"
            );
        }
    }

    #[test]
    fn peer_resolution_accepts_three_historical_tip_generations() {
        let mut connection = database();
        apply_create(&mut connection);
        let local = prepare_local_branch(&connection, NoteSyncOperation::Upsert, "local edit");
        let first = received(
            &connection,
            plaintext(
                UPDATE_EVENT,
                Some(CREATE_EVENT),
                2,
                "upsert",
                "2026-01-02T00:00:00.000000Z",
                "remote one",
            ),
            2,
            REMOTE_DEVICE,
        );
        assert_eq!(
            apply_verified_received_note(&mut connection, &first).unwrap(),
            ApplyVerifiedReceivedNoteResult::Conflict
        );
        let third_event = "123e4567-e89b-42d3-a456-426614174103";
        let third = received(
            &connection,
            plaintext(
                third_event,
                Some(CREATE_EVENT),
                2,
                "upsert",
                "2026-01-02T00:00:01.000000Z",
                "remote two",
            ),
            3,
            REMOTE_DEVICE,
        );
        assert_eq!(
            apply_verified_received_note(&mut connection, &third).unwrap(),
            ApplyVerifiedReceivedNoteResult::Conflict
        );
        accept_local_conflict_parent(&mut connection, &local.event_id, 4);
        let prepared = resolution_command(&connection, "choose_version", Some(UPDATE_EVENT), None);
        let command = received_peer_resolution(&connection, prepared, 5, REMOTE_DEVICE);
        assert_eq!(
            connection
                .connection()
                .prepare(
                    "SELECT generation,count(*) FROM cloud_sync_note_conflict_tips
             GROUP BY generation ORDER BY generation",
                )
                .unwrap()
                .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
            vec![(1, 2), (2, 1)]
        );
        assert_eq!(
            apply_verified_received_resolution_v2_ipc(&mut connection, peer_ipc(&command)).unwrap(),
            ApplyVerifiedReceivedResolutionV2Result::Applied
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT local_conflict_generation,parent_count,lifecycle
             FROM cloud_sync_note_applied_resolutions",
                    [],
                    |row| Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?
                    )),
                )
                .unwrap(),
            (2, 3, "applied".into())
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT count(*) FROM cloud_sync_note_applied_resolution_parents",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            3
        );
        let ack = PrepareNoteSyncAckCommand {
            account_id: ACCOUNT.into(),
            device_id: PULLING_DEVICE.into(),
            canonical_user_id: CANONICAL_USER.into(),
        };
        assert_eq!(
            prepare_note_sync_ack(connection.connection_mut_for_test(), &ack)
                .unwrap()
                .candidate_cursor,
            5
        );
    }

    #[test]
    fn peer_resolution_orphans_missing_proof_and_rejects_changed_state() {
        let (mut missing_group, command, _, _) = peer_resolution_fixture("choose_version", true);
        missing_group
            .connection()
            .execute(
                "UPDATE cloud_sync_note_conflict_groups SET lifecycle='abandoned'",
                [],
            )
            .unwrap();
        assert_eq!(
            apply_verified_received_resolution_v2_ipc(&mut missing_group, peer_ipc(&command))
                .unwrap(),
            ApplyVerifiedReceivedResolutionV2Result::Orphan
        );
        assert_eq!(
            missing_group
                .connection()
                .query_row(
                    "SELECT state,error_code FROM cloud_sync_inbox WHERE event_id=?1",
                    [&command.event_id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .unwrap(),
            ("orphan".into(), "missing_conflict_group".into())
        );

        let mut unpublished = database();
        let (local, remote) = prepare_edit_conflict(&mut unpublished);
        let prepared = resolution_command(&unpublished, "choose_version", Some(&remote), None);
        let unpublished_command =
            received_peer_resolution(&unpublished, prepared, 3, REMOTE_DEVICE);
        assert_eq!(
            apply_verified_received_resolution_v2_ipc(
                &mut unpublished,
                peer_ipc(&unpublished_command)
            )
            .unwrap(),
            ApplyVerifiedReceivedResolutionV2Result::Orphan
        );
        assert_eq!(
            unpublished
                .connection()
                .query_row(
                    "SELECT state FROM cloud_sync_inbox WHERE event_id=?1",
                    [&unpublished_command.event_id],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "orphan"
        );
        assert_eq!(
            unpublished
                .connection()
                .query_row(
                    "SELECT lifecycle FROM cloud_sync_outbox WHERE event_id=?1",
                    [local],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "unsealed"
        );

        let mut missing_parent = database();
        let (local, remote) = prepare_edit_conflict(&mut missing_parent);
        accept_local_conflict_parent(&mut missing_parent, &local, 3);
        let prepared = resolution_command(&missing_parent, "choose_version", Some(&remote), None);
        let mut payload: serde_json::Value =
            serde_json::from_slice(&prepared.canonical_payload).unwrap();
        let mut ids: Vec<String> = payload["resolution"]["resolved_event_ids"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap().to_string())
            .collect();
        ids.push("123e4567-e89b-42d3-a456-426614174105".into());
        ids.sort();
        payload["resolution"]["resolved_event_ids"] = serde_json::json!(ids.clone());
        payload["header"]["parent_event_id"] = serde_json::Value::String(ids[0].clone());
        payload["header"]["additional_parent_event_ids"] = serde_json::json!(ids[1..].to_vec());
        let missing_command = received_peer_resolution(
            &missing_parent,
            PrepareNoteConflictResolutionCommand {
                canonical_payload: canonical_json(&payload).unwrap().into_bytes(),
                ..prepared
            },
            4,
            REMOTE_DEVICE,
        );
        assert_eq!(
            apply_verified_received_resolution_v2_ipc(
                &mut missing_parent,
                peer_ipc(&missing_command)
            )
            .unwrap(),
            ApplyVerifiedReceivedResolutionV2Result::Orphan
        );
        assert_eq!(
            missing_parent
                .connection()
                .query_row(
                    "SELECT error_code FROM cloud_sync_inbox WHERE event_id=?1",
                    [missing_command.event_id],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "missing_conflict_parent"
        );

        let (mut changed_object, command, _, _) = peer_resolution_fixture("choose_version", true);
        let mut ipc = peer_ipc(&command);
        ipc.ciphertext[0] ^= 1;
        assert!(matches!(
            apply_verified_received_resolution_v2_ipc(&mut changed_object, ipc),
            Err(ApplyVerifiedReceivedResolutionV2Error::ImmutableMismatch)
        ));
        assert_eq!(
            changed_object
                .connection()
                .query_row(
                    "SELECT state FROM cloud_sync_inbox WHERE event_id=?1",
                    [&command.event_id],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "received"
        );

        let (mut extra_tip, command, _, _) = peer_resolution_fixture("choose_version", true);
        let fourth = received(
            &extra_tip,
            plaintext(
                "123e4567-e89b-42d3-a456-426614174104",
                Some(CREATE_EVENT),
                2,
                "upsert",
                "2026-01-02T00:00:02.000000Z",
                "later tip",
            ),
            5,
            REMOTE_DEVICE,
        );
        assert_eq!(
            apply_verified_received_note(&mut extra_tip, &fourth).unwrap(),
            ApplyVerifiedReceivedNoteResult::Conflict
        );
        assert!(matches!(
            apply_verified_received_resolution_v2_ipc(&mut extra_tip, peer_ipc(&command)),
            Err(ApplyVerifiedReceivedResolutionV2Error::StaleConflict)
        ));
    }

    #[test]
    fn peer_resolution_rejects_scope_snapshot_local_edit_and_clone_collision() {
        let (mut wrong_scope, command, _, _) = peer_resolution_fixture("choose_version", true);
        let mut ipc = peer_ipc(&command);
        ipc.canonical_user_id = REMOTE_DEVICE.into();
        assert!(matches!(
            apply_verified_received_resolution_v2_ipc(&mut wrong_scope, ipc),
            Err(ApplyVerifiedReceivedResolutionV2Error::ScopeMismatch)
        ));
        let mut ipc = peer_ipc(&command);
        ipc.source_device_id = PULLING_DEVICE.into();
        assert!(matches!(
            apply_verified_received_resolution_v2_ipc(&mut wrong_scope, ipc),
            Err(ApplyVerifiedReceivedResolutionV2Error::ImmutableMismatch)
        ));
        let mut ipc = peer_ipc(&command);
        ipc.server_sequence += 1;
        assert!(matches!(
            apply_verified_received_resolution_v2_ipc(&mut wrong_scope, ipc),
            Err(ApplyVerifiedReceivedResolutionV2Error::ImmutableMismatch)
        ));

        let (mut changed_snapshot, command, _, _) = peer_resolution_fixture("choose_version", true);
        let mut payload: serde_json::Value =
            serde_json::from_slice(&command.canonical_payload).unwrap();
        payload["result"]["note"]["content"] = serde_json::Value::String("forged snapshot".into());
        let changed_payload = canonical_json(&payload).unwrap().into_bytes();
        let changed_resolution = decode_canonical_resolution_bytes(&changed_payload).unwrap();
        let changed = VerifiedPeerResolutionCommand {
            canonical_payload: changed_payload,
            resolution: changed_resolution,
            ..command
        };
        assert!(matches!(
            apply_verified_received_resolution_v2_inner(
                &mut changed_snapshot,
                &changed,
                false,
                false
            ),
            Err(ApplyVerifiedReceivedResolutionV2Error::MissingCausalProof)
        ));

        let (mut local_edit, command, _, _) = peer_resolution_fixture("choose_version", true);
        prepare_local_branch(&local_edit, NoteSyncOperation::Upsert, "newer local edit");
        assert!(matches!(
            apply_verified_received_resolution_v2_ipc(&mut local_edit, peer_ipc(&command)),
            Err(ApplyVerifiedReceivedResolutionV2Error::StaleConflict)
        ));

        let (mut collision, command, _, _) = peer_resolution_fixture("keep_both", false);
        collision
            .connection()
            .execute(
                "INSERT INTO cloud_sync_entities(
                account_id,project_id,entity_id,entity_type,head_event_id,
                head_sync_revision,updated_at
             ) VALUES(?1,'p','n-copy','note',?2,1,'now')",
                rusqlite::params![ACCOUNT, CREATE_EVENT],
            )
            .unwrap();
        assert!(matches!(
            apply_verified_received_resolution_v2_ipc(&mut collision, peer_ipc(&command)),
            Err(ApplyVerifiedReceivedResolutionV2Error::CloneCollision)
        ));
    }

    #[test]
    fn peer_resolution_rolls_back_and_allows_proven_ordinary_continuation() {
        let (mut after_original, command, _, _) = peer_resolution_fixture("choose_version", true);
        let before: String = after_original
            .connection()
            .query_row("SELECT payload_json FROM notes WHERE id='n'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(matches!(
            apply_verified_received_resolution_v2_inner(&mut after_original, &command, true, false),
            Err(ApplyVerifiedReceivedResolutionV2Error::InjectedFailure)
        ));
        assert_eq!(
            after_original
                .connection()
                .query_row("SELECT payload_json FROM notes WHERE id='n'", [], |row| row
                    .get::<_, String>(0),)
                .unwrap(),
            before
        );
        assert_eq!(
            after_original
                .connection()
                .query_row(
                    "SELECT count(*) FROM cloud_sync_note_applied_resolutions",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );

        let (mut before_completion, command, _, _) = peer_resolution_fixture("keep_both", false);
        assert!(matches!(
            apply_verified_received_resolution_v2_inner(
                &mut before_completion,
                &command,
                false,
                true
            ),
            Err(ApplyVerifiedReceivedResolutionV2Error::InjectedFailure)
        ));
        assert_eq!(
            before_completion
                .connection()
                .query_row("SELECT count(*) FROM notes WHERE id='n-copy'", [], |row| {
                    row.get::<_, i64>(0)
                },)
                .unwrap(),
            0
        );
        assert_eq!(
            before_completion
                .connection()
                .query_row(
                    "SELECT count(*) FROM cloud_sync_note_applied_resolutions",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
        assert_eq!(
            before_completion
                .connection()
                .query_row(
                    "SELECT lifecycle FROM cloud_sync_note_conflict_groups",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "open"
        );

        let (mut applied, command, _, _) = peer_resolution_fixture("choose_version", true);
        assert_eq!(
            apply_verified_received_resolution_v2_ipc(&mut applied, peer_ipc(&command)).unwrap(),
            ApplyVerifiedReceivedResolutionV2Result::Applied
        );
        let snapshot: String = applied
            .connection()
            .query_row("SELECT payload_json FROM notes WHERE id='n'", [], |row| {
                row.get(0)
            })
            .unwrap();
        let transaction = applied.connection().unchecked_transaction().unwrap();
        let child = prepare_unsealed_note_intent(
            &transaction,
            PrepareNoteIntent {
                project_id: "p",
                entity_id: "n",
                operation: NoteSyncOperation::Upsert,
                updated_at: "2026-01-04T00:00:00.000000Z",
                deleted_at: None,
                snapshot_json: &snapshot,
                state_updated_at: "now",
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            child.parent_event_id.as_deref(),
            Some(command.event_id.as_str())
        );
        assert_eq!(child.revision, command.resolution.header.revision + 1);
        let other_snapshot = snapshot.replace("\"id\":\"n\"", "\"id\":\"other\"");
        assert!(prepare_unsealed_note_intent(
            &transaction,
            PrepareNoteIntent {
                project_id: "p",
                entity_id: "other",
                operation: NoteSyncOperation::Upsert,
                updated_at: "2026-01-04T00:00:00.000000Z",
                deleted_at: None,
                snapshot_json: &other_snapshot,
                state_updated_at: "now",
            }
        )
        .unwrap()
        .is_some());
        transaction.rollback().unwrap();

        let (mut applied_clone, clone_command, _, _) = peer_resolution_fixture("keep_both", false);
        assert_eq!(
            apply_verified_received_resolution_v2_ipc(&mut applied_clone, peer_ipc(&clone_command))
                .unwrap(),
            ApplyVerifiedReceivedResolutionV2Result::Applied
        );
        let clone_snapshot: String = applied_clone
            .connection()
            .query_row(
                "SELECT payload_json FROM notes WHERE id='n-copy'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let transaction = applied_clone.connection().unchecked_transaction().unwrap();
        let child = prepare_unsealed_note_intent(
            &transaction,
            PrepareNoteIntent {
                project_id: "p",
                entity_id: "n-copy",
                operation: NoteSyncOperation::Upsert,
                updated_at: "2026-01-04T00:00:00.000000Z",
                deleted_at: None,
                snapshot_json: &clone_snapshot,
                state_updated_at: "now",
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            child.parent_event_id.as_deref(),
            Some(clone_command.event_id.as_str())
        );
        assert_eq!(child.revision, clone_command.resolution.header.revision + 1);
        transaction.rollback().unwrap();
    }

    #[test]
    fn applied_resolution_child_coalesces_and_seals_as_ordinary_v1() {
        let (mut connection, resolution, _, _) = peer_resolution_fixture("choose_version", true);
        assert_eq!(
            apply_verified_received_resolution_v2_ipc(&mut connection, peer_ipc(&resolution))
                .unwrap(),
            ApplyVerifiedReceivedResolutionV2Result::Applied
        );
        let snapshot: String = connection
            .connection()
            .query_row("SELECT payload_json FROM notes WHERE id='n'", [], |row| {
                row.get(0)
            })
            .unwrap();
        let tombstone = build_note_tombstone(
            &serde_json::from_str(&snapshot).unwrap(),
            "2026-01-04T00:00:00.000000Z",
        )
        .unwrap();
        let transaction = connection.connection().unchecked_transaction().unwrap();
        let prepare = |operation, deleted_at| {
            prepare_unsealed_note_intent(
                &transaction,
                PrepareNoteIntent {
                    project_id: "p",
                    entity_id: "n",
                    operation,
                    updated_at: "2026-01-04T00:00:00.000000Z",
                    deleted_at,
                    snapshot_json: if deleted_at.is_some() {
                        &tombstone
                    } else {
                        &snapshot
                    },
                    state_updated_at: "now",
                },
            )
            .unwrap()
            .unwrap()
        };
        let first = prepare(NoteSyncOperation::Upsert, None);
        let second = prepare(NoteSyncOperation::Upsert, None);
        let deleted = prepare(
            NoteSyncOperation::Delete,
            Some("2026-01-04T00:00:00.000000Z"),
        );
        assert_eq!(first.event_id, second.event_id);
        assert_eq!(first.event_id, deleted.event_id);
        assert_eq!(second.mutation_generation, first.mutation_generation + 1);
        assert_eq!(deleted.mutation_generation, second.mutation_generation + 1);
        assert_eq!(
            deleted.parent_event_id.as_deref(),
            Some(resolution.event_id.as_str())
        );
        assert_eq!(deleted.revision, resolution.resolution.header.revision + 1);
        transaction.commit().unwrap();
        assert_eq!(
            commit_sealed_note_sync_event(
                connection.connection_mut_for_test(),
                &CommitSealedNoteSyncEventCommand {
                    event_id: deleted.event_id.clone(),
                    expected_mutation_generation: deleted.mutation_generation,
                    envelope: EncryptedNoteSyncEnvelope {
                        crypto_version: 1,
                        aad_version: 1,
                        nonce: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into(),
                        ciphertext: "AAAAAAAAAAAAAAAAAAAAAA".into(),
                    },
                }
            )
            .unwrap(),
            CommitSealedNoteSyncEventResult::Sealed
        );
        let (operation, parent, revision): (String, String, i64) = connection.connection().query_row(
            "SELECT operation,parent_event_id,revision FROM cloud_sync_outbox WHERE event_id=?1",
            [&deleted.event_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).unwrap();
        assert_eq!(
            (operation.as_str(), parent.as_str(), revision),
            (
                "delete",
                resolution.event_id.as_str(),
                resolution.resolution.header.revision + 1
            )
        );
    }

    #[test]
    fn ordinary_child_waits_for_resolution_then_applies_and_preserves_siblings() {
        let (mut connection, resolution, _, _) = peer_resolution_fixture("choose_version", true);
        let revision = resolution.resolution.header.revision + 1;
        let a_id = "123e4567-e89b-42d3-a456-426614174301";
        let b_id = "123e4567-e89b-42d3-a456-426614174302";
        let a = received(
            &connection,
            plaintext(
                a_id,
                Some(&resolution.event_id),
                revision,
                "upsert",
                "2026-01-04T00:00:00.000000Z",
                "after resolution A",
            ),
            5,
            REMOTE_DEVICE,
        );
        assert_eq!(
            apply_verified_received_note(&mut connection, &a).unwrap(),
            ApplyVerifiedReceivedNoteResult::Orphan
        );
        assert_eq!(
            apply_verified_received_resolution_v2_ipc(&mut connection, peer_ipc(&resolution))
                .unwrap(),
            ApplyVerifiedReceivedResolutionV2Result::Applied
        );
        assert_eq!(
            apply_verified_received_note(&mut connection, &a).unwrap(),
            ApplyVerifiedReceivedNoteResult::Applied
        );
        let b = received(
            &connection,
            plaintext(
                b_id,
                Some(&resolution.event_id),
                revision,
                "upsert",
                "2026-01-04T00:00:01.000000Z",
                "after resolution B",
            ),
            6,
            REMOTE_DEVICE,
        );
        assert_eq!(
            apply_verified_received_note(&mut connection, &b).unwrap(),
            ApplyVerifiedReceivedNoteResult::Conflict
        );
        let (parent, tips): (String, i64) = connection.connection().query_row(
            "SELECT common_parent_event_id,
                    (SELECT count(*) FROM cloud_sync_note_conflict_tips WHERE group_id=group_row.group_id)
             FROM cloud_sync_note_conflict_groups AS group_row WHERE lifecycle='open'",
            [], |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap();
        assert_eq!(parent, resolution.event_id);
        assert_eq!(tips, 2);
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT state FROM cloud_sync_inbox WHERE event_id=?1",
                    [b_id],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "conflict_preserved"
        );

        let local_second = resolution_command(&connection, "choose_version", Some(a_id), None);
        prepare_note_conflict_resolution(connection.connection_mut_for_test(), &local_second)
            .unwrap();
        let second = payload_with_event_id(&local_second, "123e4567-e89b-42d3-a456-426614174303");
        let second = received_peer_resolution(&connection, second, 7, REMOTE_DEVICE);
        assert_eq!(second.resolution.header.revision, revision + 1);
        assert_eq!(second.resolution.resolved_event_ids, vec![a_id, b_id]);
        assert_eq!(
            apply_verified_received_resolution_v2_ipc(&mut connection, peer_ipc(&second)).unwrap(),
            ApplyVerifiedReceivedResolutionV2Result::Applied
        );
        assert_eq!(connection.connection().query_row(
            "SELECT head_event_id FROM cloud_sync_entities WHERE account_id=?1 AND entity_id='n'",
            [ACCOUNT], |row| row.get::<_, String>(0),
        ).unwrap(), second.event_id);
    }

    #[test]
    fn local_resolution_blocks_children_until_self_echo_is_applied() {
        for accepted in [false, true] {
            let (mut connection, resolution, _) = self_echo_fixture("keep_both", accepted);
            for entity_id in ["n", "n-copy"] {
                let snapshot: String = connection
                    .connection()
                    .query_row(
                        "SELECT payload_json FROM notes WHERE id=?1",
                        [entity_id],
                        |row| row.get(0),
                    )
                    .unwrap();
                let transaction = connection.connection().unchecked_transaction().unwrap();
                assert!(matches!(
                    prepare_unsealed_note_intent(
                        &transaction,
                        PrepareNoteIntent {
                            project_id: "p",
                            entity_id,
                            operation: NoteSyncOperation::Upsert,
                            updated_at: "2026-01-04T00:00:00.000000Z",
                            deleted_at: None,
                            snapshot_json: &snapshot,
                            state_updated_at: "now",
                        }
                    ),
                    Err(NoteSyncError::InvalidSealState(
                        "resolution is not durably applied"
                    ))
                ));
                transaction.rollback().unwrap();
            }
            assert_eq!(
                reconcile_verified_received_resolution_self_echo_ipc(
                    &mut connection,
                    self_echo_ipc(&resolution)
                )
                .unwrap(),
                ReconcileVerifiedReceivedResolutionSelfEchoResult::Reconciled
            );
            for entity_id in ["n", "n-copy"] {
                let snapshot: String = connection
                    .connection()
                    .query_row(
                        "SELECT payload_json FROM notes WHERE id=?1",
                        [entity_id],
                        |row| row.get(0),
                    )
                    .unwrap();
                let transaction = connection.connection().unchecked_transaction().unwrap();
                let child = prepare_unsealed_note_intent(
                    &transaction,
                    PrepareNoteIntent {
                        project_id: "p",
                        entity_id,
                        operation: NoteSyncOperation::Upsert,
                        updated_at: "2026-01-04T00:00:00.000000Z",
                        deleted_at: None,
                        snapshot_json: &snapshot,
                        state_updated_at: "now",
                    },
                )
                .unwrap()
                .unwrap();
                assert_eq!(
                    child.parent_event_id.as_deref(),
                    Some(resolution.event_id.as_str())
                );
                assert_eq!(child.revision, resolution.resolution.header.revision + 1);
                transaction.rollback().unwrap();
            }
        }
    }

    #[test]
    fn resolution_parent_proof_is_exactly_scoped_and_delete_can_continue() {
        let (mut connection, resolution, _, _) = peer_resolution_fixture("keep_both", false);
        assert_eq!(
            apply_verified_received_resolution_v2_ipc(&mut connection, peer_ipc(&resolution))
                .unwrap(),
            ApplyVerifiedReceivedResolutionV2Result::Applied
        );
        let transaction = connection.connection().unchecked_transaction().unwrap();
        for entity_id in ["n", "n-copy"] {
            assert_eq!(
                applied_resolution_parent_revision(
                    &transaction,
                    ACCOUNT,
                    "p",
                    entity_id,
                    &resolution.event_id
                )
                .unwrap(),
                Some(resolution.resolution.header.revision)
            );
        }
        for (account, project, entity) in [
            ("other", "p", "n"),
            (ACCOUNT, "other", "n"),
            (ACCOUNT, "p", "unrelated"),
        ] {
            assert_eq!(
                applied_resolution_parent_revision(
                    &transaction,
                    account,
                    project,
                    entity,
                    &resolution.event_id
                )
                .unwrap(),
                None
            );
        }
        transaction.rollback().unwrap();
        let delete = received(
            &connection,
            plaintext(
                "123e4567-e89b-42d3-a456-426614174304",
                Some(&resolution.event_id),
                resolution.resolution.header.revision + 1,
                "delete",
                "2026-01-04T00:00:00.000000Z",
                "",
            ),
            5,
            REMOTE_DEVICE,
        );
        assert_eq!(
            apply_verified_received_note(&mut connection, &delete).unwrap(),
            ApplyVerifiedReceivedNoteResult::Applied
        );
        assert_eq!(
            connection
                .connection()
                .query_row("SELECT count(*) FROM notes WHERE id='n'", [], |row| row
                    .get::<_, i64>(0),)
                .unwrap(),
            0
        );
    }

    #[test]
    fn local_unsealed_and_remote_children_preserve_resolution_sibling_conflict() {
        let (mut connection, resolution, _, _) = peer_resolution_fixture("choose_version", true);
        assert_eq!(
            apply_verified_received_resolution_v2_ipc(&mut connection, peer_ipc(&resolution))
                .unwrap(),
            ApplyVerifiedReceivedResolutionV2Result::Applied
        );
        let local =
            prepare_local_branch(&connection, NoteSyncOperation::Upsert, "local descendant");
        assert_eq!(
            local.parent_event_id.as_deref(),
            Some(resolution.event_id.as_str())
        );
        let remote_id = "123e4567-e89b-42d3-a456-426614174305";
        let remote = received(
            &connection,
            plaintext(
                remote_id,
                Some(&resolution.event_id),
                local.revision,
                "upsert",
                "2026-01-04T00:00:00.000000Z",
                "remote descendant",
            ),
            5,
            REMOTE_DEVICE,
        );
        assert_eq!(
            apply_verified_received_note(&mut connection, &remote).unwrap(),
            ApplyVerifiedReceivedNoteResult::Conflict
        );
        let (parent, source): (String, String) = connection
            .connection()
            .query_row(
                "SELECT conflict_group.common_parent_event_id,version.source
             FROM cloud_sync_note_conflict_groups AS conflict_group
             JOIN cloud_sync_note_conflict_versions AS version
               ON version.group_id=conflict_group.group_id
             WHERE conflict_group.lifecycle='open' AND version.event_id=?1",
                [&local.event_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            (parent.as_str(), source.as_str()),
            (resolution.event_id.as_str(), "local_unsealed")
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT state FROM cloud_sync_inbox WHERE event_id=?1",
                    [remote_id],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "conflict_preserved"
        );
    }

    #[test]
    fn prepared_keep_both_blocks_original_and_planned_clone() {
        let mut connection = database();
        let (local, remote) = prepare_edit_conflict(&mut connection);
        let resolution = resolution_command(&connection, "keep_both", Some(&local), Some(&remote));
        prepare_note_conflict_resolution(connection.connection_mut_for_test(), &resolution)
            .unwrap();
        for entity_id in ["n", "n-copy"] {
            let snapshot = serde_json::json!({"id":entity_id,"project_id":"p"}).to_string();
            let transaction = connection.connection().unchecked_transaction().unwrap();
            assert!(matches!(
                prepare_unsealed_note_intent(
                    &transaction,
                    PrepareNoteIntent {
                        project_id: "p",
                        entity_id,
                        operation: NoteSyncOperation::Upsert,
                        updated_at: "2026-01-04T00:00:00.000000Z",
                        deleted_at: None,
                        snapshot_json: &snapshot,
                        state_updated_at: "now",
                    }
                ),
                Err(NoteSyncError::InvalidSealState(
                    "resolution is not durably applied"
                ))
            ));
            transaction.rollback().unwrap();
        }
    }

    #[test]
    fn locally_applied_but_unsealed_resolution_blocks_ordinary_child() {
        let mut connection = database();
        let (resolution, _, _) = applied_resolution_for_sealing(&mut connection);
        let snapshot: String = connection
            .connection()
            .query_row("SELECT payload_json FROM notes WHERE id='n'", [], |row| {
                row.get(0)
            })
            .unwrap();
        let transaction = connection.connection().unchecked_transaction().unwrap();
        assert!(matches!(
            prepare_unsealed_note_intent(
                &transaction,
                PrepareNoteIntent {
                    project_id: "p",
                    entity_id: "n",
                    operation: NoteSyncOperation::Upsert,
                    updated_at: "2026-01-04T00:00:00.000000Z",
                    deleted_at: None,
                    snapshot_json: &snapshot,
                    state_updated_at: "now",
                }
            ),
            Err(NoteSyncError::InvalidSealState(
                "resolution is not durably applied"
            ))
        ));
        assert_eq!(
            applied_resolution_parent_revision(
                &transaction,
                ACCOUNT,
                "p",
                "n",
                &decode_canonical_resolution(&resolution)
                    .unwrap()
                    .header
                    .event_id
            )
            .unwrap(),
            None
        );
        transaction.rollback().unwrap();
    }

    #[test]
    fn deleted_resolution_result_allows_later_ordinary_recreation() {
        let (mut connection, resolution, _, _) = peer_resolution_fixture("delete", false);
        assert_eq!(
            apply_verified_received_resolution_v2_ipc(&mut connection, peer_ipc(&resolution))
                .unwrap(),
            ApplyVerifiedReceivedResolutionV2Result::Applied
        );
        assert_eq!(
            connection
                .connection()
                .query_row("SELECT count(*) FROM notes WHERE id='n'", [], |row| row
                    .get::<_, i64>(0),)
                .unwrap(),
            0
        );
        let child = received(
            &connection,
            plaintext(
                "123e4567-e89b-42d3-a456-426614174306",
                Some(&resolution.event_id),
                resolution.resolution.header.revision + 1,
                "upsert",
                "2026-01-04T00:00:00.000000Z",
                "recreated",
            ),
            5,
            REMOTE_DEVICE,
        );
        assert_eq!(
            apply_verified_received_note(&mut connection, &child).unwrap(),
            ApplyVerifiedReceivedNoteResult::Applied
        );
        assert_eq!(
            connection
                .connection()
                .query_row("SELECT count(*) FROM notes WHERE id='n'", [], |row| row
                    .get::<_, i64>(0),)
                .unwrap(),
            1
        );
    }

    #[test]
    fn missing_applied_resolution_parent_proof_blocks_child() {
        let (mut connection, resolution, _, _) = peer_resolution_fixture("choose_version", true);
        assert_eq!(
            apply_verified_received_resolution_v2_ipc(&mut connection, peer_ipc(&resolution))
                .unwrap(),
            ApplyVerifiedReceivedResolutionV2Result::Applied
        );
        let snapshot: String = connection
            .connection()
            .query_row("SELECT payload_json FROM notes WHERE id='n'", [], |row| {
                row.get(0)
            })
            .unwrap();
        let transaction = connection.connection().unchecked_transaction().unwrap();
        let child = prepare_unsealed_note_intent(
            &transaction,
            PrepareNoteIntent {
                project_id: "p",
                entity_id: "n",
                operation: NoteSyncOperation::Upsert,
                updated_at: "2026-01-04T00:00:00.000000Z",
                deleted_at: None,
                snapshot_json: &snapshot,
                state_updated_at: "now",
            },
        )
        .unwrap()
        .unwrap();
        transaction.commit().unwrap();
        // Simulate a damaged local database; normal application writes cannot
        // delete these immutable rows.
        connection
            .connection()
            .execute_batch(
                "DROP TRIGGER cloud_sync_note_applied_resolution_parent_immutable_delete",
            )
            .unwrap();
        connection
            .connection()
            .execute(
                "DELETE FROM cloud_sync_note_applied_resolution_parents
             WHERE account_id=?1 AND resolution_event_id=?2 AND parent_ordinal=0",
                rusqlite::params![ACCOUNT, resolution.event_id],
            )
            .unwrap();
        let transaction = connection.connection().unchecked_transaction().unwrap();
        assert_eq!(
            applied_resolution_parent_revision(
                &transaction,
                ACCOUNT,
                "p",
                "n",
                &resolution.event_id
            )
            .unwrap(),
            None
        );
        assert!(matches!(
            prepare_unsealed_note_intent(
                &transaction,
                PrepareNoteIntent {
                    project_id: "p",
                    entity_id: "n",
                    operation: NoteSyncOperation::Upsert,
                    updated_at: "2026-01-04T00:00:00.000000Z",
                    deleted_at: None,
                    snapshot_json: &snapshot,
                    state_updated_at: "now",
                }
            ),
            Err(NoteSyncError::InvalidSealState(
                "resolution parent proof is incomplete"
            ))
        ));
        assert_eq!(
            transaction
                .query_row(
                    "SELECT mutation_generation FROM cloud_sync_note_intents WHERE event_id=?1",
                    [&child.event_id],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            child.mutation_generation
        );
        transaction.rollback().unwrap();
    }

    #[test]
    fn post_resolution_conflict_failure_rolls_back_all_new_evidence() {
        let (mut connection, resolution, _, _) = peer_resolution_fixture("choose_version", true);
        assert_eq!(
            apply_verified_received_resolution_v2_ipc(&mut connection, peer_ipc(&resolution))
                .unwrap(),
            ApplyVerifiedReceivedResolutionV2Result::Applied
        );
        let first_id = "123e4567-e89b-42d3-a456-426614174307";
        let sibling_id = "123e4567-e89b-42d3-a456-426614174308";
        let revision = resolution.resolution.header.revision + 1;
        let first = received(
            &connection,
            plaintext(
                first_id,
                Some(&resolution.event_id),
                revision,
                "upsert",
                "2026-01-04T00:00:00.000000Z",
                "first",
            ),
            5,
            REMOTE_DEVICE,
        );
        assert_eq!(
            apply_verified_received_note(&mut connection, &first).unwrap(),
            ApplyVerifiedReceivedNoteResult::Applied
        );
        let sibling = received(
            &connection,
            plaintext(
                sibling_id,
                Some(&resolution.event_id),
                revision,
                "upsert",
                "2026-01-04T00:00:01.000000Z",
                "sibling",
            ),
            6,
            REMOTE_DEVICE,
        );
        connection
            .connection()
            .execute_batch(
                "CREATE TRIGGER inject_post_resolution_conflict_failure
             BEFORE INSERT ON cloud_sync_note_conflict_versions
             WHEN NEW.event_id='123e4567-e89b-42d3-a456-426614174308'
             BEGIN SELECT RAISE(ABORT,'injected_conflict_failure'); END",
            )
            .unwrap();
        assert!(apply_verified_received_note(&mut connection, &sibling).is_err());
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT state FROM cloud_sync_inbox WHERE event_id=?1",
                    [sibling_id],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "received"
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT count(*) FROM cloud_sync_note_conflict_groups WHERE lifecycle='open'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
        connection
            .connection()
            .execute_batch("DROP TRIGGER inject_post_resolution_conflict_failure")
            .unwrap();
        assert_eq!(
            apply_verified_received_note(&mut connection, &sibling).unwrap(),
            ApplyVerifiedReceivedNoteResult::Conflict
        );
    }

    #[test]
    fn orphan_child_retries_as_preserved_sibling_after_resolution_and_peer_child() {
        let (mut connection, resolution, _, _) = peer_resolution_fixture("choose_version", true);
        let revision = resolution.resolution.header.revision + 1;
        let orphan_id = "123e4567-e89b-42d3-a456-426614174309";
        let orphan = received(
            &connection,
            plaintext(
                orphan_id,
                Some(&resolution.event_id),
                revision,
                "upsert",
                "2026-01-04T00:00:00.000000Z",
                "orphan sibling",
            ),
            5,
            REMOTE_DEVICE,
        );
        assert_eq!(
            apply_verified_received_note(&mut connection, &orphan).unwrap(),
            ApplyVerifiedReceivedNoteResult::Orphan
        );
        assert_eq!(
            apply_verified_received_resolution_v2_ipc(&mut connection, peer_ipc(&resolution))
                .unwrap(),
            ApplyVerifiedReceivedResolutionV2Result::Applied
        );
        let first = received(
            &connection,
            plaintext(
                "123e4567-e89b-42d3-a456-426614174310",
                Some(&resolution.event_id),
                revision,
                "upsert",
                "2026-01-04T00:00:01.000000Z",
                "first applied",
            ),
            6,
            REMOTE_DEVICE,
        );
        assert_eq!(
            apply_verified_received_note(&mut connection, &first).unwrap(),
            ApplyVerifiedReceivedNoteResult::Applied
        );
        assert_eq!(
            apply_verified_received_note(&mut connection, &orphan).unwrap(),
            ApplyVerifiedReceivedNoteResult::Conflict
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT state FROM cloud_sync_inbox WHERE event_id=?1",
                    [orphan_id],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "conflict_preserved"
        );
    }

    #[test]
    fn peer_resolution_replays_after_restart_and_isolates_self_echo() {
        let (root, path) = conflict_database_path("peer-resolution-restart");
        let mut connection = configured_database(Connection::open(&path).unwrap());
        let (local, remote) = prepare_edit_conflict(&mut connection);
        accept_local_conflict_parent(&mut connection, &local, 3);
        let prepared = resolution_command(&connection, "keep_both", Some(&local), Some(&remote));
        let command = received_peer_resolution(&connection, prepared, 4, REMOTE_DEVICE);
        assert_eq!(
            apply_verified_received_resolution_v2_ipc(&mut connection, peer_ipc(&command)).unwrap(),
            ApplyVerifiedReceivedResolutionV2Result::Applied
        );
        drop(connection);
        let mut reopened = crate::sqlite::open_privileged_remote_apply_database(&path).unwrap();
        assert_eq!(
            apply_verified_received_resolution_v2_ipc(&mut reopened, peer_ipc(&command)).unwrap(),
            ApplyVerifiedReceivedResolutionV2Result::AlreadyApplied
        );
        assert_eq!(
            reopened
                .connection()
                .query_row("SELECT count(*) FROM notes WHERE id='n-copy'", [], |row| {
                    row.get::<_, i64>(0)
                },)
                .unwrap(),
            1
        );
        let transaction = reopened.connection().unchecked_transaction().unwrap();
        for entity_id in ["n", "n-copy"] {
            assert_eq!(
                applied_resolution_parent_revision(
                    &transaction,
                    ACCOUNT,
                    "p",
                    entity_id,
                    &command.event_id
                )
                .unwrap(),
                Some(command.resolution.header.revision)
            );
        }
        transaction.rollback().unwrap();
        let mut conflicting = peer_ipc(&command);
        let mut payload: serde_json::Value =
            serde_json::from_slice(&conflicting.plaintext).unwrap();
        payload["resolution"]["conflict_generation"] = serde_json::Value::from(78);
        conflicting.plaintext = canonical_json(&payload).unwrap().into_bytes();
        assert!(matches!(
            apply_verified_received_resolution_v2_ipc(&mut reopened, conflicting),
            Err(ApplyVerifiedReceivedResolutionV2Error::ImmutableMismatch)
        ));
        assert_eq!(
            reopened
                .connection()
                .query_row("SELECT count(*) FROM notes WHERE id='n-copy'", [], |row| {
                    row.get::<_, i64>(0)
                },)
                .unwrap(),
            1
        );
        drop(reopened);
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(&root).unwrap();

        let mut self_echo = database();
        let (resolution, _, _) = applied_resolution_for_sealing(&mut self_echo);
        let seal = resolution_seal_command(&resolution);
        commit_sealed_note_resolution_event(self_echo.connection_mut_for_test(), &seal).unwrap();
        let decoded = decode_canonical_resolution(&resolution).unwrap();
        self_echo
            .connection()
            .execute(
                "INSERT INTO cloud_sync_inbox(
                account_id,event_id,server_sequence,device_id,project_id,entity_id,
                entity_type,operation,sync_revision,updated_at,deleted_at,state,received_at
             ) VALUES(?1,?2,50,?3,?4,?5,'note','resolution',?6,?7,NULL,'received','now')",
                rusqlite::params![
                    ACCOUNT,
                    decoded.header.event_id,
                    PULLING_DEVICE,
                    decoded.header.project_id,
                    decoded.header.entity_id,
                    decoded.header.revision,
                    decoded.header.updated_at
                ],
            )
            .unwrap();
        let (nonce, ciphertext): (Vec<u8>, Vec<u8>) = self_echo
            .connection()
            .query_row(
                "SELECT nonce,ciphertext FROM cloud_sync_event_objects WHERE event_id=?1",
                [&decoded.header.event_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        let own = VerifiedPeerResolutionCommand {
            account_id: ACCOUNT.into(),
            canonical_user_id: CANONICAL_USER.into(),
            pulling_device_id: PULLING_DEVICE.into(),
            event_id: decoded.header.event_id.clone(),
            server_sequence: 50,
            source_device_id: PULLING_DEVICE.into(),
            crypto_version: 1,
            aad_version: 1,
            nonce,
            ciphertext,
            canonical_payload: resolution.canonical_payload.clone(),
            resolution: decoded,
        };
        let note_before: String = self_echo
            .connection()
            .query_row("SELECT payload_json FROM notes WHERE id='n'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            apply_verified_received_resolution_v2_ipc(&mut self_echo, peer_ipc(&own)).unwrap(),
            ApplyVerifiedReceivedResolutionV2Result::SelfEchoPending
        );
        assert_eq!(
            self_echo
                .connection()
                .query_row("SELECT payload_json FROM notes WHERE id='n'", [], |row| row
                    .get::<_, String>(0),)
                .unwrap(),
            note_before
        );
        assert_eq!(
            self_echo
                .connection()
                .query_row(
                    "SELECT state FROM cloud_sync_inbox WHERE event_id=?1",
                    [own.event_id],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "received"
        );

        let mut unmatched_self_echo = database();
        let (local, remote) = prepare_edit_conflict(&mut unmatched_self_echo);
        accept_local_conflict_parent(&mut unmatched_self_echo, &local, 3);
        let prepared =
            resolution_command(&unmatched_self_echo, "choose_version", Some(&remote), None);
        let unmatched = received_peer_resolution(&unmatched_self_echo, prepared, 4, PULLING_DEVICE);
        let note_before: String = unmatched_self_echo
            .connection()
            .query_row("SELECT payload_json FROM notes WHERE id='n'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(matches!(
            apply_verified_received_resolution_v2_ipc(
                &mut unmatched_self_echo,
                peer_ipc(&unmatched)
            ),
            Err(ApplyVerifiedReceivedResolutionV2Error::SelfEchoMismatch)
        ));
        assert_eq!(
            unmatched_self_echo
                .connection()
                .query_row("SELECT payload_json FROM notes WHERE id='n'", [], |row| row
                    .get::<_, String>(0),)
                .unwrap(),
            note_before
        );
        assert_eq!(
            unmatched_self_echo
                .connection()
                .query_row(
                    "SELECT state FROM cloud_sync_inbox WHERE event_id=?1",
                    [unmatched.event_id],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "received"
        );
    }

    #[test]
    fn self_echo_reconciles_existing_receipts_for_all_strategies_without_note_mutation() {
        for strategy in ["choose_version", "manual_merge", "keep_both", "delete"] {
            let (mut connection, command, _) = self_echo_fixture(strategy, true);
            let notes_before: Vec<(String, String)> = {
                let mut statement = connection
                    .connection()
                    .prepare("SELECT id,payload_json FROM notes ORDER BY id")
                    .unwrap();
                statement
                    .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                    .unwrap()
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap()
            };
            match strategy {
                "choose_version" => {
                    assert_eq!(connection.connection().query_row(
                    "SELECT json_extract(payload_json,'$.content') FROM notes WHERE id='n'",[],
                    |row|row.get::<_,String>(0),
                ).unwrap(),"local edit")
                }
                "manual_merge" => {
                    assert_eq!(connection.connection().query_row(
                    "SELECT json_extract(payload_json,'$.content') FROM notes WHERE id='n'",[],
                    |row|row.get::<_,String>(0),
                ).unwrap(),"manual merge")
                }
                "keep_both" => assert_eq!(connection.connection().query_row(
                    "SELECT group_concat(id || ':' || json_extract(payload_json,'$.content'),'|')
                     FROM (SELECT id,payload_json FROM notes ORDER BY id)",[],
                    |row|row.get::<_,String>(0),
                ).unwrap(),"n:local edit|n-copy:remote edit"),
                "delete" => assert!(notes_before.is_empty()),
                _ => unreachable!(),
            }
            let outgoing_before: (i64, i64) = connection
                .connection()
                .query_row(
                    "SELECT (SELECT count(*) FROM cloud_sync_outbox),
                        (SELECT count(*) FROM cloud_sync_note_resolution_outbox)",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            let ack = PrepareNoteSyncAckCommand {
                account_id: ACCOUNT.into(),
                device_id: PULLING_DEVICE.into(),
                canonical_user_id: CANONICAL_USER.into(),
            };
            assert_eq!(
                prepare_note_sync_ack(connection.connection_mut_for_test(), &ack)
                    .unwrap()
                    .candidate_cursor,
                3
            );
            assert_eq!(
                reconcile_verified_received_resolution_self_echo_ipc(
                    &mut connection,
                    self_echo_ipc(&command)
                )
                .unwrap(),
                ReconcileVerifiedReceivedResolutionSelfEchoResult::Reconciled
            );
            let notes_after: Vec<(String, String)> = {
                let mut statement = connection
                    .connection()
                    .prepare("SELECT id,payload_json FROM notes ORDER BY id")
                    .unwrap();
                statement
                    .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                    .unwrap()
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap()
            };
            assert_eq!(notes_after, notes_before);
            assert_eq!(
                connection
                    .connection()
                    .query_row(
                        "SELECT acceptance_source,duplicate,server_sequence
                 FROM cloud_sync_note_resolution_upload_receipts
                 WHERE resolution_event_id=?1",
                        [&command.event_id],
                        |row| Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, Option<i64>>(1)?,
                            row.get::<_, i64>(2)?
                        )),
                    )
                    .unwrap(),
                ("push_response".into(), Some(1), 4)
            );
            assert_eq!(
                connection
                    .connection()
                    .query_row(
                        "SELECT remote_conflict_group_id=local_conflict_group_id,
                        remote_conflict_generation=local_conflict_generation,lifecycle
                 FROM cloud_sync_note_applied_resolutions WHERE resolution_event_id=?1",
                        [&command.event_id],
                        |row| Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, String>(2)?
                        )),
                    )
                    .unwrap(),
                (1, 1, "applied".into())
            );
            assert_eq!(
                connection
                    .connection()
                    .query_row(
                        "SELECT count(*) FROM cloud_sync_note_applied_resolution_parents
                 WHERE account_id=?1 AND resolution_event_id=?2",
                        rusqlite::params![ACCOUNT, command.event_id],
                        |row| row.get::<_, i64>(0),
                    )
                    .unwrap(),
                2
            );
            assert_eq!(
                connection
                    .connection()
                    .query_row(
                        "SELECT state FROM cloud_sync_inbox WHERE event_id=?1",
                        [&command.event_id],
                        |row| row.get::<_, String>(0),
                    )
                    .unwrap(),
                "applied"
            );
            assert_eq!(
                connection
                    .connection()
                    .query_row(
                        "SELECT head_event_id,head_sync_revision FROM cloud_sync_entities
                 WHERE entity_id='n'",
                        [],
                        |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
                    )
                    .unwrap(),
                (command.event_id.clone(), 3)
            );
            if strategy == "keep_both" {
                assert_eq!(
                    connection
                        .connection()
                        .query_row(
                            "SELECT head_event_id,head_sync_revision FROM cloud_sync_entities
                     WHERE entity_id='n-copy'",
                            [],
                            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
                        )
                        .unwrap(),
                    (command.event_id.clone(), 3)
                );
            }
            assert_eq!(
                connection
                    .connection()
                    .query_row(
                        "SELECT count(*) FROM cloud_sync_note_causal_history WHERE event_id=?1",
                        [&command.event_id],
                        |row| row.get::<_, i64>(0),
                    )
                    .unwrap(),
                0
            );
            assert_eq!(
                connection
                    .connection()
                    .query_row(
                        "SELECT (SELECT count(*) FROM cloud_sync_outbox),
                        (SELECT count(*) FROM cloud_sync_note_resolution_outbox)",
                        [],
                        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
                    )
                    .unwrap(),
                outgoing_before
            );
            assert_eq!(
                prepare_note_sync_ack(connection.connection_mut_for_test(), &ack)
                    .unwrap()
                    .candidate_cursor,
                4
            );
        }
    }

    #[test]
    fn self_echo_lost_response_creates_null_duplicate_receipt_atomically() {
        let (mut connection, command, _) = self_echo_fixture("keep_both", false);
        let notes_before: i64 = connection
            .connection()
            .query_row("SELECT count(*) FROM notes", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT lifecycle FROM cloud_sync_note_resolution_outbox
             WHERE resolution_event_id=?1",
                    [&command.event_id],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "sealed_local"
        );
        assert_eq!(
            reconcile_verified_received_resolution_self_echo_ipc(
                &mut connection,
                self_echo_ipc(&command)
            )
            .unwrap(),
            ReconcileVerifiedReceivedResolutionSelfEchoResult::Reconciled
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT acceptance_source,duplicate,server_sequence
             FROM cloud_sync_note_resolution_upload_receipts
             WHERE resolution_event_id=?1",
                    [&command.event_id],
                    |row| Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<i64>>(1)?,
                        row.get::<_, i64>(2)?
                    )),
                )
                .unwrap(),
            ("pull_self_echo".into(), None, 4)
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT lifecycle FROM cloud_sync_note_resolution_outbox
             WHERE resolution_event_id=?1",
                    [&command.event_id],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "accepted"
        );
        assert_eq!(
            connection
                .connection()
                .query_row("SELECT count(*) FROM notes", [], |row| row.get::<_, i64>(0),)
                .unwrap(),
            notes_before
        );
        assert_eq!(
            connection
                .connection()
                .query_row("SELECT count(*) FROM notes WHERE id='n-copy'", [], |row| {
                    row.get::<_, i64>(0)
                },)
                .unwrap(),
            1
        );
    }

    #[test]
    fn self_echo_lost_response_rolls_back_every_injected_boundary() {
        for failure in [
            SelfEchoFailurePoint::AfterReceipt,
            SelfEchoFailurePoint::AfterAcceptance,
            SelfEchoFailurePoint::BeforeLedgerCompletion,
            SelfEchoFailurePoint::BeforeInboxApply,
        ] {
            let (mut connection, command, _) = self_echo_fixture("choose_version", false);
            let note_before: String = connection
                .connection()
                .query_row("SELECT payload_json FROM notes WHERE id='n'", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert!(matches!(
                reconcile_verified_received_resolution_self_echo_inner(
                    &mut connection,
                    &command,
                    failure
                ),
                Err(ApplyVerifiedReceivedResolutionV2Error::InjectedFailure)
            ));
            assert_eq!(
                connection
                    .connection()
                    .query_row(
                        "SELECT count(*) FROM cloud_sync_note_resolution_upload_receipts
                 WHERE resolution_event_id=?1",
                        [&command.event_id],
                        |row| row.get::<_, i64>(0),
                    )
                    .unwrap(),
                0
            );
            assert_eq!(
                connection
                    .connection()
                    .query_row(
                        "SELECT lifecycle FROM cloud_sync_note_resolution_outbox
                 WHERE resolution_event_id=?1",
                        [&command.event_id],
                        |row| row.get::<_, String>(0),
                    )
                    .unwrap(),
                "sealed_local"
            );
            assert_eq!(
                connection
                    .connection()
                    .query_row(
                        "SELECT count(*) FROM cloud_sync_note_applied_resolutions
                 WHERE resolution_event_id=?1",
                        [&command.event_id],
                        |row| row.get::<_, i64>(0),
                    )
                    .unwrap(),
                0
            );
            assert_eq!(
                connection
                    .connection()
                    .query_row(
                        "SELECT lifecycle FROM cloud_sync_note_conflict_groups",
                        [],
                        |row| row.get::<_, String>(0),
                    )
                    .unwrap(),
                "resolving"
            );
            assert_eq!(
                connection
                    .connection()
                    .query_row(
                        "SELECT state FROM cloud_sync_inbox WHERE event_id=?1",
                        [&command.event_id],
                        |row| row.get::<_, String>(0),
                    )
                    .unwrap(),
                "received"
            );
            assert_eq!(
                connection
                    .connection()
                    .query_row("SELECT payload_json FROM notes WHERE id='n'", [], |row| row
                        .get::<_, String>(0),)
                    .unwrap(),
                note_before
            );
        }
    }

    #[test]
    fn self_echo_rejects_untrusted_scope_object_payload_and_missing_outbox() {
        for mutate in 0..4 {
            let (mut connection, command, _) = self_echo_fixture("choose_version", false);
            let mut ipc = self_echo_ipc(&command);
            match mutate {
                0 => ipc.source_device_id = REMOTE_DEVICE.into(),
                1 => ipc.server_sequence = 5,
                2 => ipc.ciphertext[0] ^= 1,
                3 => {
                    let mut payload: serde_json::Value =
                        serde_json::from_slice(&ipc.plaintext).unwrap();
                    payload["result"]["note"]["content"] =
                        serde_json::Value::String("forged".into());
                    ipc.plaintext = canonical_json(&payload).unwrap().into_bytes();
                }
                _ => unreachable!(),
            }
            assert!(
                reconcile_verified_received_resolution_self_echo_ipc(&mut connection, ipc).is_err()
            );
            assert_eq!(
                connection
                    .connection()
                    .query_row(
                        "SELECT state FROM cloud_sync_inbox WHERE event_id=?1",
                        [&command.event_id],
                        |row| row.get::<_, String>(0),
                    )
                    .unwrap(),
                "received"
            );
            assert_eq!(
                connection
                    .connection()
                    .query_row(
                        "SELECT count(*) FROM cloud_sync_note_applied_resolutions",
                        [],
                        |row| row.get::<_, i64>(0),
                    )
                    .unwrap(),
                0
            );
        }

        let mut missing = database();
        let (local, remote) = prepare_edit_conflict(&mut missing);
        accept_local_conflict_parent(&mut missing, &local, 3);
        let prepared = resolution_command(&missing, "choose_version", Some(&remote), None);
        let command = received_peer_resolution(&missing, prepared, 4, PULLING_DEVICE);
        assert!(matches!(
            reconcile_verified_received_resolution_self_echo_ipc(
                &mut missing,
                self_echo_ipc(&command)
            ),
            Err(ApplyVerifiedReceivedResolutionV2Error::SelfEchoMismatch)
        ));
        assert_eq!(
            missing
                .connection()
                .query_row(
                    "SELECT state FROM cloud_sync_inbox WHERE event_id=?1",
                    [command.event_id],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "received"
        );

        let mut wrong_lifecycle = database();
        let (local, remote) = prepare_edit_conflict(&mut wrong_lifecycle);
        accept_local_conflict_parent(&mut wrong_lifecycle, &local, 3);
        let prepared = resolution_command(&wrong_lifecycle, "choose_version", Some(&remote), None);
        prepare_note_conflict_resolution(wrong_lifecycle.connection_mut_for_test(), &prepared)
            .unwrap();
        apply_prepared_note_conflict_resolution(&mut wrong_lifecycle, &prepared).unwrap();
        let event_id = decode_canonical_resolution(&prepared)
            .unwrap()
            .header
            .event_id;
        wrong_lifecycle
            .connection()
            .execute(
                "INSERT INTO cloud_sync_event_objects(
                account_id,event_id,crypto_version,aad_version,nonce,ciphertext,stored_at
             ) VALUES(?1,?2,1,1,?3,?4,'now')",
                rusqlite::params![ACCOUNT, event_id, vec![0_u8; 24], vec![0_u8; 16]],
            )
            .unwrap();
        let command = receive_self_echo(&wrong_lifecycle, prepared, 4);
        assert!(matches!(
            reconcile_verified_received_resolution_self_echo_ipc(
                &mut wrong_lifecycle,
                self_echo_ipc(&command)
            ),
            Err(ApplyVerifiedReceivedResolutionV2Error::SelfEchoMismatch)
        ));
        assert_eq!(
            wrong_lifecycle
                .connection()
                .query_row(
                    "SELECT lifecycle FROM cloud_sync_note_resolution_outbox
             WHERE resolution_event_id=?1",
                    [command.event_id],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "local_pending"
        );

        let (mut incomplete_pull, command, _) = self_echo_fixture("choose_version", false);
        let transaction = incomplete_pull
            .connection_mut_for_test()
            .transaction()
            .unwrap();
        transaction
            .execute(
                "INSERT INTO cloud_sync_note_resolution_upload_receipts(
                account_id,resolution_event_id,device_id,server_sequence,duplicate,
                acceptance_source,accepted_at
             ) VALUES(?1,?2,?3,?4,NULL,'pull_self_echo','now')",
                rusqlite::params![
                    ACCOUNT,
                    command.event_id,
                    PULLING_DEVICE,
                    command.server_sequence
                ],
            )
            .unwrap();
        transaction
            .execute(
                "UPDATE cloud_sync_note_resolution_outbox SET lifecycle='accepted',updated_at='now'
             WHERE resolution_event_id=?1 AND lifecycle='sealed_local'",
                [&command.event_id],
            )
            .unwrap();
        transaction.commit().unwrap();
        assert!(matches!(
            reconcile_verified_received_resolution_self_echo_ipc(
                &mut incomplete_pull,
                self_echo_ipc(&command)
            ),
            Err(ApplyVerifiedReceivedResolutionV2Error::SelfEchoMismatch)
        ));
        assert_eq!(
            incomplete_pull
                .connection()
                .query_row(
                    "SELECT state FROM cloud_sync_inbox WHERE event_id=?1",
                    [&command.event_id],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "received"
        );
        assert_eq!(
            incomplete_pull
                .connection()
                .query_row(
                    "SELECT count(*) FROM cloud_sync_note_applied_resolutions",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );

        let (mut changed_note, command, _) = self_echo_fixture("choose_version", true);
        let prior: String = changed_note
            .connection()
            .query_row("SELECT payload_json FROM notes WHERE id='n'", [], |row| {
                row.get(0)
            })
            .unwrap();
        let mut value: serde_json::Value = serde_json::from_str(&prior).unwrap();
        value["content"] = serde_json::Value::String("unexpected later content".into());
        let changed = serde_json::to_string(&value).unwrap();
        changed_note
            .authorize_once(
                crate::sqlite::RemoteApplyAuthorization {
                    event_id: &command.event_id,
                    account_id: ACCOUNT,
                    project_id: "p",
                    entity_id: "n",
                    operation: "upsert",
                    payload_json: Some(&changed),
                    prior_payload_json: Some(&prior),
                },
                |transaction| {
                    transaction.execute("UPDATE notes SET payload_json=?1 WHERE id='n'", [&changed])
                },
            )
            .unwrap();
        assert!(matches!(
            reconcile_verified_received_resolution_self_echo_ipc(
                &mut changed_note,
                self_echo_ipc(&command)
            ),
            Err(ApplyVerifiedReceivedResolutionV2Error::ImmutableMismatch)
        ));
        assert_eq!(
            changed_note
                .connection()
                .query_row("SELECT payload_json FROM notes WHERE id='n'", [], |row| row
                    .get::<_, String>(0),)
                .unwrap(),
            changed
        );
        assert_eq!(
            changed_note
                .connection()
                .query_row(
                    "SELECT state FROM cloud_sync_inbox WHERE event_id=?1",
                    [&command.event_id],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "received"
        );
        assert_eq!(changed_note.connection().query_row(
            "SELECT acceptance_source,duplicate FROM cloud_sync_note_resolution_upload_receipts
             WHERE resolution_event_id=?1",[&command.event_id],
            |row|Ok((row.get::<_,String>(0)?,row.get::<_,Option<i64>>(1)?)),
        ).unwrap(),("push_response".into(),Some(1)));
    }

    #[test]
    fn self_echo_exact_replay_survives_restart_without_second_clone_or_receipt() {
        let (root, path) = conflict_database_path("self-echo-restart");
        let mut connection = configured_database(Connection::open(&path).unwrap());
        let (command, _) = prepare_self_echo(&mut connection, "keep_both", false);
        assert_eq!(
            reconcile_verified_received_resolution_self_echo_ipc(
                &mut connection,
                self_echo_ipc(&command)
            )
            .unwrap(),
            ReconcileVerifiedReceivedResolutionSelfEchoResult::Reconciled
        );
        drop(connection);
        let mut reopened = crate::sqlite::open_privileged_remote_apply_database(&path).unwrap();
        assert_eq!(
            reconcile_verified_received_resolution_self_echo_ipc(
                &mut reopened,
                self_echo_ipc(&command)
            )
            .unwrap(),
            ReconcileVerifiedReceivedResolutionSelfEchoResult::AlreadyReconciled
        );
        let prior: String = reopened
            .connection()
            .query_row("SELECT payload_json FROM notes WHERE id='n'", [], |row| {
                row.get(0)
            })
            .unwrap();
        let mut value: serde_json::Value = serde_json::from_str(&prior).unwrap();
        value["content"] = serde_json::Value::String("later authenticated content".into());
        let later = serde_json::to_string(&value).unwrap();
        reopened
            .authorize_once(
                crate::sqlite::RemoteApplyAuthorization {
                    event_id: &command.event_id,
                    account_id: ACCOUNT,
                    project_id: "p",
                    entity_id: "n",
                    operation: "upsert",
                    payload_json: Some(&later),
                    prior_payload_json: Some(&prior),
                },
                |transaction| {
                    transaction.execute("UPDATE notes SET payload_json=?1 WHERE id='n'", [&later])
                },
            )
            .unwrap();
        assert_eq!(
            reconcile_verified_received_resolution_self_echo_ipc(
                &mut reopened,
                self_echo_ipc(&command)
            )
            .unwrap(),
            ReconcileVerifiedReceivedResolutionSelfEchoResult::AlreadyReconciled
        );
        assert_eq!(
            reopened
                .connection()
                .query_row("SELECT payload_json FROM notes WHERE id='n'", [], |row| row
                    .get::<_, String>(0),)
                .unwrap(),
            later
        );
        assert_eq!(
            reopened
                .connection()
                .query_row("SELECT count(*) FROM notes WHERE id='n-copy'", [], |row| {
                    row.get::<_, i64>(0)
                },)
                .unwrap(),
            1
        );
        assert_eq!(
            reopened
                .connection()
                .query_row(
                    "SELECT count(*) FROM cloud_sync_note_resolution_upload_receipts
             WHERE resolution_event_id=?1",
                    [&command.event_id],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );
        assert_eq!(
            reopened
                .connection()
                .query_row(
                    "SELECT count(*) FROM cloud_sync_note_applied_resolutions
             WHERE resolution_event_id=?1",
                    [&command.event_id],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );
        let mut conflicting = self_echo_ipc(&command);
        conflicting.nonce[0] ^= 1;
        assert!(matches!(
            reconcile_verified_received_resolution_self_echo_ipc(&mut reopened, conflicting),
            Err(ApplyVerifiedReceivedResolutionV2Error::ImmutableMismatch)
        ));
        let mut conflicting = self_echo_ipc(&command);
        let mut payload: serde_json::Value =
            serde_json::from_slice(&conflicting.plaintext).unwrap();
        payload["resolution"]["conflict_generation"] = serde_json::Value::from(2);
        conflicting.plaintext = canonical_json(&payload).unwrap().into_bytes();
        assert!(
            reconcile_verified_received_resolution_self_echo_ipc(&mut reopened, conflicting)
                .is_err()
        );
        drop(reopened);
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(&root).unwrap();
    }

    #[test]
    fn self_echo_accepts_multigeneration_tips_and_rejects_missing_parent_publication() {
        let mut connection = database();
        apply_create(&mut connection);
        let local = prepare_local_branch(&connection, NoteSyncOperation::Upsert, "local edit");
        let first = received(
            &connection,
            plaintext(
                UPDATE_EVENT,
                Some(CREATE_EVENT),
                2,
                "upsert",
                "2026-01-02T00:00:00.000000Z",
                "remote one",
            ),
            2,
            REMOTE_DEVICE,
        );
        assert_eq!(
            apply_verified_received_note(&mut connection, &first).unwrap(),
            ApplyVerifiedReceivedNoteResult::Conflict
        );
        let third_event = "123e4567-e89b-42d3-a456-426614174103";
        let third = received(
            &connection,
            plaintext(
                third_event,
                Some(CREATE_EVENT),
                2,
                "upsert",
                "2026-01-02T00:00:01.000000Z",
                "remote two",
            ),
            3,
            REMOTE_DEVICE,
        );
        assert_eq!(
            apply_verified_received_note(&mut connection, &third).unwrap(),
            ApplyVerifiedReceivedNoteResult::Conflict
        );
        accept_local_conflict_parent(&mut connection, &local.event_id, 4);
        let prepared = resolution_command(&connection, "choose_version", Some(UPDATE_EVENT), None);
        prepare_note_conflict_resolution(connection.connection_mut_for_test(), &prepared).unwrap();
        apply_prepared_note_conflict_resolution(&mut connection, &prepared).unwrap();
        let seal = resolution_seal_command(&prepared);
        commit_sealed_note_resolution_event(connection.connection_mut_for_test(), &seal).unwrap();
        let command = receive_self_echo(&connection, prepared, 5);
        assert_eq!(
            connection
                .connection()
                .prepare(
                    "SELECT generation,count(*) FROM cloud_sync_note_conflict_tips
             GROUP BY generation ORDER BY generation",
                )
                .unwrap()
                .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
            vec![(1, 2), (2, 1)]
        );
        assert_eq!(
            reconcile_verified_received_resolution_self_echo_ipc(
                &mut connection,
                self_echo_ipc(&command)
            )
            .unwrap(),
            ReconcileVerifiedReceivedResolutionSelfEchoResult::Reconciled
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT local_conflict_generation,parent_count,lifecycle
             FROM cloud_sync_note_applied_resolutions WHERE resolution_event_id=?1",
                    [&command.event_id],
                    |row| Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?
                    )),
                )
                .unwrap(),
            (2, 3, "applied".into())
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT count(*) FROM cloud_sync_note_applied_resolution_parents
             WHERE resolution_event_id=?1",
                    [&command.event_id],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            3
        );

        let mut unpublished = database();
        let (_local, remote) = prepare_edit_conflict(&mut unpublished);
        let prepared = resolution_command(&unpublished, "choose_version", Some(&remote), None);
        prepare_note_conflict_resolution(unpublished.connection_mut_for_test(), &prepared).unwrap();
        apply_prepared_note_conflict_resolution(&mut unpublished, &prepared).unwrap();
        let seal = resolution_seal_command(&prepared);
        commit_sealed_note_resolution_event(unpublished.connection_mut_for_test(), &seal).unwrap();
        let command = receive_self_echo(&unpublished, prepared, 3);
        assert!(matches!(
            reconcile_verified_received_resolution_self_echo_ipc(
                &mut unpublished,
                self_echo_ipc(&command)
            ),
            Err(ApplyVerifiedReceivedResolutionV2Error::MissingCausalProof)
        ));
        assert_eq!(
            unpublished
                .connection()
                .query_row(
                    "SELECT lifecycle FROM cloud_sync_note_resolution_outbox
             WHERE resolution_event_id=?1",
                    [&command.event_id],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "sealed_local"
        );
        assert_eq!(
            unpublished
                .connection()
                .query_row(
                    "SELECT state FROM cloud_sync_inbox WHERE event_id=?1",
                    [command.event_id],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "received"
        );
    }

    #[test]
    fn self_echo_enforces_v1_resolution_receipt_sequence_collisions_both_directions() {
        let (mut existing_v1, command, _) = self_echo_fixture("choose_version", false);
        let other = prepare_sealed_other_event(&mut existing_v1);
        commit_note_sync_upload_acceptance(
            existing_v1.connection_mut_for_test(),
            &CommitNoteSyncUploadAcceptanceCommand {
                account_id: ACCOUNT.into(),
                device_id: PULLING_DEVICE.into(),
                receipts: vec![NoteSyncUploadReceipt {
                    event_id: other,
                    server_sequence: 4,
                    duplicate: false,
                }],
            },
        )
        .unwrap();
        assert!(reconcile_verified_received_resolution_self_echo_ipc(
            &mut existing_v1,
            self_echo_ipc(&command)
        )
        .is_err());
        assert_eq!(
            existing_v1
                .connection()
                .query_row(
                    "SELECT count(*) FROM cloud_sync_note_resolution_upload_receipts
             WHERE resolution_event_id=?1",
                    [&command.event_id],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
        assert_eq!(
            existing_v1
                .connection()
                .query_row(
                    "SELECT lifecycle FROM cloud_sync_note_resolution_outbox
             WHERE resolution_event_id=?1",
                    [&command.event_id],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "sealed_local"
        );

        let (mut existing_resolution, command, _) = self_echo_fixture("choose_version", false);
        assert_eq!(
            reconcile_verified_received_resolution_self_echo_ipc(
                &mut existing_resolution,
                self_echo_ipc(&command)
            )
            .unwrap(),
            ReconcileVerifiedReceivedResolutionSelfEchoResult::Reconciled
        );
        let other = prepare_sealed_other_event(&mut existing_resolution);
        assert!(commit_note_sync_upload_acceptance(
            existing_resolution.connection_mut_for_test(),
            &CommitNoteSyncUploadAcceptanceCommand {
                account_id: ACCOUNT.into(),
                device_id: PULLING_DEVICE.into(),
                receipts: vec![NoteSyncUploadReceipt {
                    event_id: other,
                    server_sequence: 4,
                    duplicate: false,
                }],
            }
        )
        .is_err());
        assert_eq!(
            existing_resolution
                .connection()
                .query_row(
                    "SELECT count(*) FROM cloud_sync_upload_receipts WHERE server_sequence=4",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
    }

    #[test]
    fn revision_one_id_collision_is_not_promoted_to_durable_conflict() {
        let mut connection = database();
        let local_plaintext = plaintext(
            CREATE_EVENT,
            None,
            1,
            "upsert",
            "2026-01-01T00:00:00.000000Z",
            "unproven local",
        );
        let payload = match local_plaintext {
            NoteSyncPlaintext::Create { note, .. } => note_payload_from_record(&note).unwrap(),
            _ => unreachable!(),
        };
        connection
            .connection()
            .execute("DELETE FROM cloud_sync_project_bindings", [])
            .unwrap();
        connection
            .connection()
            .execute(
                "INSERT INTO notes(id,project_id,stage_id,updated_at,payload_json)
             VALUES('n','p',NULL,'2026-01-01T00:00:00.000000Z',?1)",
                [&payload],
            )
            .unwrap();
        connection
            .connection()
            .execute(
                "INSERT INTO cloud_sync_project_bindings VALUES('p',?1,'now','now')",
                [ACCOUNT],
            )
            .unwrap();
        let command = received(
            &connection,
            plaintext(
                UPDATE_EVENT,
                None,
                1,
                "upsert",
                "2026-01-01T00:00:00.000000Z",
                "remote collision",
            ),
            1,
            REMOTE_DEVICE,
        );
        assert_eq!(
            apply_verified_received_note(&mut connection, &command).unwrap(),
            ApplyVerifiedReceivedNoteResult::Conflict
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT count(*) FROM cloud_sync_note_conflict_groups",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
    }

    #[test]
    fn remote_apply_rechecks_scope_and_missing_project_without_losing_inbox_evidence() {
        let mut connection = database();
        let mut wrong_device = received(
            &connection,
            plaintext(
                CREATE_EVENT,
                None,
                1,
                "upsert",
                "2026-01-01T00:00:00.000000Z",
                "create",
            ),
            1,
            REMOTE_DEVICE,
        );
        wrong_device.pulling_device_id = REMOTE_DEVICE.to_string();
        assert!(apply_verified_received_note(&mut connection, &wrong_device).is_err());
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT state FROM cloud_sync_inbox WHERE event_id=?1",
                    [CREATE_EVENT],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "received"
        );

        let mut orphan = database();
        let command = received(
            &orphan,
            plaintext(
                CREATE_EVENT,
                None,
                1,
                "upsert",
                "2026-01-01T00:00:00.000000Z",
                "create",
            ),
            1,
            REMOTE_DEVICE,
        );
        orphan
            .connection()
            .execute(
                "DELETE FROM cloud_sync_project_bindings WHERE project_id='p'",
                [],
            )
            .unwrap();
        assert_eq!(
            apply_verified_received_note(&mut orphan, &command).unwrap(),
            ApplyVerifiedReceivedNoteResult::Orphan
        );
        assert_eq!(
            orphan
                .connection()
                .query_row(
                    "SELECT state,error_code FROM cloud_sync_inbox WHERE event_id=?1",
                    [CREATE_EVENT],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .unwrap(),
            ("orphan".to_string(), "missing_project".to_string())
        );
        assert_eq!(
            orphan
                .connection()
                .query_row("SELECT count(*) FROM notes", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn remote_apply_fault_rolls_back_note_head_inbox_and_capability() {
        let mut connection = database();
        connection
            .connection()
            .execute_batch(
                "CREATE TRIGGER test_remote_apply_fault BEFORE INSERT ON cloud_sync_entities
             BEGIN SELECT RAISE(ABORT, 'deterministic remote apply fault'); END;",
            )
            .unwrap();
        let command = received(
            &connection,
            plaintext(
                CREATE_EVENT,
                None,
                1,
                "upsert",
                "2026-01-01T00:00:00.000000Z",
                "create",
            ),
            1,
            REMOTE_DEVICE,
        );
        assert!(apply_verified_received_note(&mut connection, &command).is_err());
        assert_eq!(
            connection
                .connection()
                .query_row("SELECT count(*) FROM notes", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            connection
                .connection()
                .query_row("SELECT count(*) FROM cloud_sync_entities", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT state FROM cloud_sync_inbox WHERE event_id=?1",
                    [CREATE_EVENT],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            "received"
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT count(*) FROM cloud_sync_remote_apply_authorizations",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        assert!(connection
            .connection()
            .execute(
                "INSERT INTO notes(id,project_id,stage_id,updated_at,payload_json)
             VALUES('n','p',NULL,'now','{}')",
                [],
            )
            .is_err());
    }

    #[test]
    fn remote_apply_ipc_rejects_invalid_plaintext_before_any_sqlite_mutation() {
        let mut connection = database();
        let result = apply_verified_received_note_ipc(
            &mut connection,
            ApplyVerifiedReceivedNoteIpcCommand {
                account_id: ACCOUNT.to_string(),
                canonical_user_id: CANONICAL_USER.to_string(),
                pulling_device_id: PULLING_DEVICE.to_string(),
                event_id: CREATE_EVENT.to_string(),
                server_sequence: 1,
                source_device_id: REMOTE_DEVICE.to_string(),
                crypto_version: 1,
                aad_version: 1,
                nonce: vec![0; 24],
                ciphertext: vec![0; 16],
                plaintext: br#"{"not":"a NoteSyncPlaintext"}"#.to_vec(),
            },
        );
        assert!(result.is_err());
        assert_eq!(
            connection
                .connection()
                .query_row("SELECT count(*) FROM notes", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            connection
                .connection()
                .query_row("SELECT count(*) FROM cloud_sync_entities", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT count(*) FROM cloud_sync_remote_apply_authorizations",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
    }

    #[test]
    fn remote_apply_ipc_decodes_checked_in_typescript_canonical_bytes() {
        let mut connection = database();
        let fixture: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../src/cloud/__fixtures__/noteSyncPlaintextV1.json"
        )))
        .unwrap();
        let plaintext = fixture["canonical_json"]
            .as_str()
            .unwrap()
            .as_bytes()
            .to_vec();
        let typed = decode_note_sync_plaintext(&plaintext).unwrap();
        let received = received(&connection, typed, 1, REMOTE_DEVICE);
        let result = apply_verified_received_note_ipc(
            &mut connection,
            ApplyVerifiedReceivedNoteIpcCommand {
                account_id: received.account_id,
                canonical_user_id: received.canonical_user_id,
                pulling_device_id: received.pulling_device_id,
                event_id: received.event_id,
                server_sequence: received.server_sequence,
                source_device_id: received.source_device_id,
                crypto_version: received.crypto_version,
                aad_version: received.aad_version,
                nonce: received.nonce,
                ciphertext: received.ciphertext,
                plaintext,
            },
        );
        assert_eq!(result.unwrap(), ApplyVerifiedReceivedNoteResult::Applied);
        assert_eq!(
            connection
                .connection()
                .query_row("SELECT count(*) FROM notes", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            1
        );
    }

    #[test]
    fn prepared_resolution_is_atomic_non_mutating_and_replays_after_restart() {
        let (root, path) = conflict_database_path("prepared-resolution-restart");
        let mut connection = configured_database(Connection::open(&path).unwrap());
        let (_, remote) = prepare_edit_conflict(&mut connection);
        let command = resolution_command(&connection, "choose_version", Some(&remote), None);
        let before = (
            connection
                .connection()
                .query_row("SELECT payload_json FROM notes WHERE id='n'", [], |row| {
                    row.get::<_, String>(0)
                })
                .unwrap(),
            connection
                .connection()
                .query_row("SELECT count(*) FROM cloud_sync_outbox", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            connection
                .connection()
                .query_row("SELECT count(*) FROM cloud_sync_inbox", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            connection
                .connection()
                .query_row(
                    "SELECT count(*) FROM cloud_sync_upload_receipts",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            connection
                .connection()
                .query_row(
                    "SELECT count(*) FROM cloud_sync_note_conflict_versions",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
        );
        assert_eq!(
            prepare_note_conflict_resolution(connection.connection_mut_for_test(), &command)
                .unwrap(),
            PrepareNoteConflictResolutionResult::Prepared
        );
        assert_eq!(prepared_count(&connection), 1);
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT lifecycle FROM cloud_sync_note_conflict_groups",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            "open"
        );
        assert_eq!(
            (
                connection
                    .connection()
                    .query_row("SELECT payload_json FROM notes WHERE id='n'", [], |row| row
                        .get::<_, String>(0))
                    .unwrap(),
                connection
                    .connection()
                    .query_row("SELECT count(*) FROM cloud_sync_outbox", [], |row| row
                        .get::<_, i64>(0))
                    .unwrap(),
                connection
                    .connection()
                    .query_row("SELECT count(*) FROM cloud_sync_inbox", [], |row| row
                        .get::<_, i64>(0))
                    .unwrap(),
                connection
                    .connection()
                    .query_row(
                        "SELECT count(*) FROM cloud_sync_upload_receipts",
                        [],
                        |row| row.get::<_, i64>(0)
                    )
                    .unwrap(),
                connection
                    .connection()
                    .query_row(
                        "SELECT count(*) FROM cloud_sync_note_conflict_versions",
                        [],
                        |row| row.get::<_, i64>(0)
                    )
                    .unwrap(),
            ),
            before,
        );
        drop(connection);
        let mut reopened = crate::sqlite::open_database(&path).unwrap();
        assert_eq!(
            prepare_note_conflict_resolution(&mut reopened, &command).unwrap(),
            PrepareNoteConflictResolutionResult::AlreadyPrepared
        );
        assert_eq!(
            reopened
                .query_row(
                    "SELECT lifecycle FROM cloud_sync_note_pending_resolutions",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            "prepared"
        );
        drop(reopened);
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    fn prepared_resolution_validates_all_four_strategies() {
        let mut choose = database();
        let (_, remote) = prepare_edit_conflict(&mut choose);
        let command = resolution_command(&choose, "choose_version", Some(&remote), None);
        assert_eq!(
            prepare_note_conflict_resolution(choose.connection_mut_for_test(), &command).unwrap(),
            PrepareNoteConflictResolutionResult::Prepared
        );

        let mut merge = database();
        prepare_edit_conflict(&mut merge);
        let command = resolution_command(&merge, "manual_merge", None, None);
        assert_eq!(
            prepare_note_conflict_resolution(merge.connection_mut_for_test(), &command).unwrap(),
            PrepareNoteConflictResolutionResult::Prepared
        );

        let mut both = database();
        let (local, remote) = prepare_edit_conflict(&mut both);
        let command = resolution_command(&both, "keep_both", Some(&local), Some(&remote));
        assert_eq!(
            prepare_note_conflict_resolution(both.connection_mut_for_test(), &command).unwrap(),
            PrepareNoteConflictResolutionResult::Prepared
        );

        let mut delete_edit = database();
        apply_create(&mut delete_edit);
        let local_delete = prepare_local_branch(&delete_edit, NoteSyncOperation::Delete, "");
        let remote_edit = received(
            &delete_edit,
            plaintext(
                UPDATE_EVENT,
                Some(CREATE_EVENT),
                2,
                "upsert",
                "2026-01-02T00:00:00.000000Z",
                "remote edit",
            ),
            2,
            REMOTE_DEVICE,
        );
        assert_eq!(
            apply_verified_received_note(&mut delete_edit, &remote_edit).unwrap(),
            ApplyVerifiedReceivedNoteResult::Conflict
        );
        let command = resolution_command(&delete_edit, "choose_version", Some(UPDATE_EVENT), None);
        assert_eq!(
            prepare_note_conflict_resolution(delete_edit.connection_mut_for_test(), &command)
                .unwrap(),
            PrepareNoteConflictResolutionResult::Prepared
        );
        assert_eq!(local_delete.revision, 2);

        let mut delete_resolution = database();
        prepare_edit_conflict(&mut delete_resolution);
        let command = resolution_command(&delete_resolution, "delete", None, None);
        assert_eq!(
            prepare_note_conflict_resolution(delete_resolution.connection_mut_for_test(), &command)
                .unwrap(),
            PrepareNoteConflictResolutionResult::Prepared
        );
    }

    #[test]
    fn prepared_resolution_rejects_stale_new_tip_scope_and_active_replacement() {
        let mut connection = database();
        let (_, remote) = prepare_edit_conflict(&mut connection);
        let command = resolution_command(&connection, "choose_version", Some(&remote), None);
        prepare_local_branch(
            &connection,
            NoteSyncOperation::Upsert,
            "coalesced after preview",
        );
        assert!(matches!(
            prepare_note_conflict_resolution(connection.connection_mut_for_test(), &command),
            Err(PrepareNoteConflictResolutionError::StaleConflict)
        ));
        assert_eq!(prepared_count(&connection), 0);

        let mut with_tip = database();
        let (_, remote) = prepare_edit_conflict(&mut with_tip);
        let command = resolution_command(&with_tip, "choose_version", Some(&remote), None);
        let third = received(
            &with_tip,
            plaintext(
                "123e4567-e89b-42d3-a456-426614174103",
                Some(CREATE_EVENT),
                2,
                "upsert",
                "2026-01-02T00:00:01.000000Z",
                "third tip",
            ),
            3,
            REMOTE_DEVICE,
        );
        assert_eq!(
            apply_verified_received_note(&mut with_tip, &third).unwrap(),
            ApplyVerifiedReceivedNoteResult::Conflict
        );
        assert!(matches!(
            prepare_note_conflict_resolution(with_tip.connection_mut_for_test(), &command),
            Err(PrepareNoteConflictResolutionError::StaleConflict)
        ));

        let mut active = database();
        let (_, remote) = prepare_edit_conflict(&mut active);
        let command = resolution_command(&active, "choose_version", Some(&remote), None);
        assert_eq!(
            prepare_note_conflict_resolution(active.connection_mut_for_test(), &command).unwrap(),
            PrepareNoteConflictResolutionResult::Prepared
        );
        let replacement = payload_with_event_id(&command, "123e4567-e89b-42d3-a456-426614174201");
        assert!(matches!(
            prepare_note_conflict_resolution(active.connection_mut_for_test(), &replacement),
            Err(PrepareNoteConflictResolutionError::ConflictingResolution)
        ));
        let mut changed_payload =
            serde_json::from_slice::<serde_json::Value>(&command.canonical_payload).unwrap();
        changed_payload["result"]["note"]["content"] =
            serde_json::Value::String("changed bytes".into());
        let changed = PrepareNoteConflictResolutionCommand {
            canonical_payload: canonical_json(&changed_payload).unwrap().into_bytes(),
            ..command.clone()
        };
        assert!(matches!(
            prepare_note_conflict_resolution(active.connection_mut_for_test(), &changed),
            Err(PrepareNoteConflictResolutionError::ConflictingResolution)
        ));
        let mut wrong_scope = command.clone();
        wrong_scope.device_id = REMOTE_DEVICE.into();
        assert!(matches!(
            prepare_note_conflict_resolution(active.connection_mut_for_test(), &wrong_scope),
            Err(PrepareNoteConflictResolutionError::ScopeMismatch)
        ));
        let mut wrong_account = command.clone();
        wrong_account.account_id = "other-account".into();
        assert!(matches!(
            prepare_note_conflict_resolution(active.connection_mut_for_test(), &wrong_account),
            Err(PrepareNoteConflictResolutionError::ScopeMismatch)
        ));
        let mut wrong_project_value: serde_json::Value =
            serde_json::from_slice(&command.canonical_payload).unwrap();
        wrong_project_value["header"]["project_id"] =
            serde_json::Value::String("other-project".into());
        wrong_project_value["result"]["note"]["project_id"] =
            serde_json::Value::String("other-project".into());
        let wrong_project = PrepareNoteConflictResolutionCommand {
            canonical_payload: canonical_json(&wrong_project_value).unwrap().into_bytes(),
            ..command
        };
        assert!(matches!(
            prepare_note_conflict_resolution(active.connection_mut_for_test(), &wrong_project),
            Err(PrepareNoteConflictResolutionError::ScopeMismatch)
        ));
    }

    #[test]
    fn prepared_resolution_fails_closed_for_proof_collision_and_rollback() {
        let mut rollback = database();
        let (_, remote) = prepare_edit_conflict(&mut rollback);
        let command = resolution_command(&rollback, "choose_version", Some(&remote), None);
        assert!(matches!(
            prepare_note_conflict_resolution_inner(
                rollback.connection_mut_for_test(),
                &command,
                true
            ),
            Err(PrepareNoteConflictResolutionError::InjectedFailure)
        ));
        assert_eq!(prepared_count(&rollback), 0);

        let mut proof = database();
        let (_, remote) = prepare_edit_conflict(&mut proof);
        let command = resolution_command(&proof, "choose_version", Some(&remote), None);
        proof.connection().execute("UPDATE cloud_sync_note_conflict_groups SET common_parent_event_id='123e4567-e89b-42d3-a456-426614174199'", []).unwrap();
        assert!(matches!(
            prepare_note_conflict_resolution(proof.connection_mut_for_test(), &command),
            Err(PrepareNoteConflictResolutionError::MissingCausalProof)
        ));

        let mut collision = database();
        let (_, remote) = prepare_edit_conflict(&mut collision);
        let command = resolution_command(&collision, "choose_version", Some(&remote), None);
        let mut value: serde_json::Value =
            serde_json::from_slice(&command.canonical_payload).unwrap();
        value["resolution"]["conflict_group_id"] =
            serde_json::Value::String("123e4567-e89b-42d3-a456-426614174299".into());
        let command = PrepareNoteConflictResolutionCommand {
            canonical_payload: canonical_json(&value).unwrap().into_bytes(),
            ..command
        };
        assert!(matches!(
            prepare_note_conflict_resolution(collision.connection_mut_for_test(), &command),
            Err(PrepareNoteConflictResolutionError::MissingCausalProof)
        ));
    }

    #[test]
    fn applied_resolution_covers_all_strategies_and_keeps_v1_isolated() {
        let mut choose = database();
        let (_, remote) = prepare_edit_conflict(&mut choose);
        let command = resolution_command(&choose, "choose_version", Some(&remote), None);
        prepare_note_conflict_resolution(choose.connection_mut_for_test(), &command).unwrap();
        let immutable_before: (i64, i64, i64, i64, i64) = choose
            .connection()
            .query_row(
                "SELECT (SELECT count(*) FROM cloud_sync_outbox),
                    (SELECT count(*) FROM cloud_sync_inbox),
                    (SELECT count(*) FROM cloud_sync_upload_receipts),
                    (SELECT count(*) FROM cloud_sync_note_conflict_versions),
                    (SELECT count(*) FROM cloud_sync_note_conflict_tips)",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            apply_prepared_note_conflict_resolution(&mut choose, &command).unwrap(),
            ApplyPreparedNoteConflictResolutionResult::Applied
        );
        assert_eq!(
            choose
                .connection()
                .query_row(
                    "SELECT json_extract(payload_json,'$.content') FROM notes WHERE id='n'",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            "remote edit"
        );
        assert_eq!(
            choose
                .connection()
                .query_row(
                    "SELECT lifecycle FROM cloud_sync_note_conflict_groups",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            "resolving"
        );
        assert_eq!(
            choose
                .connection()
                .query_row(
                    "SELECT lifecycle FROM cloud_sync_note_pending_resolutions",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            "consumed"
        );
        assert_eq!(
            choose
                .connection()
                .query_row(
                    "SELECT count(*) FROM cloud_sync_note_resolution_outbox",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
        assert_eq!(
            choose
                .connection()
                .query_row(
                    "SELECT count(*) FROM cloud_sync_note_resolution_dependencies",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            2
        );
        assert!(choose
            .connection()
            .execute(
                "UPDATE cloud_sync_note_intents SET mutation_generation=mutation_generation+1",
                [],
            )
            .is_err());
        assert_eq!(
            choose
                .connection()
                .query_row(
                    "SELECT (SELECT count(*) FROM cloud_sync_outbox),
                    (SELECT count(*) FROM cloud_sync_inbox),
                    (SELECT count(*) FROM cloud_sync_upload_receipts),
                    (SELECT count(*) FROM cloud_sync_note_conflict_versions),
                    (SELECT count(*) FROM cloud_sync_note_conflict_tips)",
                    [],
                    |row| Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?
                    )),
                )
                .unwrap(),
            immutable_before
        );
        assert_eq!(choose.connection().query_row(
            "SELECT count(*) FROM cloud_sync_outbox WHERE event_id=?1 OR entity_id='n-copy'",
            [&decode_canonical_resolution(&command).unwrap().header.event_id],|row|row.get::<_,i64>(0),
        ).unwrap(),0);
        let later = received(
            &choose,
            plaintext(
                "123e4567-e89b-42d3-a456-426614174103",
                Some(CREATE_EVENT),
                2,
                "upsert",
                "2026-01-02T00:00:01.000000Z",
                "later competing edit",
            ),
            3,
            REMOTE_DEVICE,
        );
        assert_eq!(
            apply_verified_received_note(&mut choose, &later).unwrap(),
            ApplyVerifiedReceivedNoteResult::Conflict
        );
        assert_eq!(choose.connection().query_row(
            "SELECT count(*) FROM cloud_sync_note_conflict_groups WHERE lifecycle='resolving'",[],
            |row|row.get::<_,i64>(0),
        ).unwrap(),1);
        assert_eq!(
            choose
                .connection()
                .query_row(
                    "SELECT count(*) FROM cloud_sync_note_conflict_groups WHERE lifecycle='open'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
        assert_eq!(
            choose
                .connection()
                .query_row(
                    "SELECT state FROM cloud_sync_inbox WHERE event_id=?1",
                    [later.event_id.as_str()],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "conflict"
        );

        let mut merge = database();
        prepare_edit_conflict(&mut merge);
        let command = resolution_command(&merge, "manual_merge", None, None);
        prepare_note_conflict_resolution(merge.connection_mut_for_test(), &command).unwrap();
        assert_eq!(
            apply_prepared_note_conflict_resolution(&mut merge, &command).unwrap(),
            ApplyPreparedNoteConflictResolutionResult::Applied
        );
        assert_eq!(
            merge
                .connection()
                .query_row(
                    "SELECT json_extract(payload_json,'$.content') FROM notes",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            "manual merge"
        );

        let mut deleted = database();
        prepare_edit_conflict(&mut deleted);
        let command = resolution_command(&deleted, "delete", None, None);
        prepare_note_conflict_resolution(deleted.connection_mut_for_test(), &command).unwrap();
        assert_eq!(
            apply_prepared_note_conflict_resolution(&mut deleted, &command).unwrap(),
            ApplyPreparedNoteConflictResolutionResult::Applied
        );
        assert_eq!(
            deleted
                .connection()
                .query_row("SELECT count(*) FROM notes", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            0
        );

        let mut both = database();
        let (local, remote) = prepare_edit_conflict(&mut both);
        let command = resolution_command(&both, "keep_both", Some(&local), Some(&remote));
        prepare_note_conflict_resolution(both.connection_mut_for_test(), &command).unwrap();
        assert_eq!(
            apply_prepared_note_conflict_resolution(&mut both, &command).unwrap(),
            ApplyPreparedNoteConflictResolutionResult::Applied
        );
        assert_eq!(
            both.connection()
                .query_row(
                    "SELECT count(*) FROM notes WHERE id IN ('n','n-copy')",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            2
        );
        assert_eq!(
            both.connection()
                .query_row(
                    "SELECT count(*) FROM cloud_sync_outbox WHERE entity_id='n-copy'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        assert_eq!(
            both.connection()
                .query_row(
                    "SELECT clone_entity_id FROM cloud_sync_note_resolution_outbox",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            "n-copy"
        );

        let mut delete_edit = database();
        apply_create(&mut delete_edit);
        prepare_local_branch(&delete_edit, NoteSyncOperation::Delete, "");
        let remote_edit = received(
            &delete_edit,
            plaintext(
                UPDATE_EVENT,
                Some(CREATE_EVENT),
                2,
                "upsert",
                "2026-01-02T00:00:00.000000Z",
                "remote edit",
            ),
            2,
            REMOTE_DEVICE,
        );
        assert_eq!(
            apply_verified_received_note(&mut delete_edit, &remote_edit).unwrap(),
            ApplyVerifiedReceivedNoteResult::Conflict
        );
        let command = resolution_command(&delete_edit, "choose_version", Some(UPDATE_EVENT), None);
        prepare_note_conflict_resolution(delete_edit.connection_mut_for_test(), &command).unwrap();
        assert_eq!(
            apply_prepared_note_conflict_resolution(&mut delete_edit, &command).unwrap(),
            ApplyPreparedNoteConflictResolutionResult::Applied
        );
        assert_eq!(
            delete_edit
                .connection()
                .query_row(
                    "SELECT json_extract(payload_json,'$.content') FROM notes",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            "remote edit"
        );
    }

    #[test]
    fn applied_resolution_replays_after_restart_and_rejects_changed_identity() {
        let (root, path) = conflict_database_path("applied-resolution-restart");
        let mut connection = configured_database(Connection::open(&path).unwrap());
        let (_, remote) = prepare_edit_conflict(&mut connection);
        let command = resolution_command(&connection, "choose_version", Some(&remote), None);
        prepare_note_conflict_resolution(connection.connection_mut_for_test(), &command).unwrap();
        apply_prepared_note_conflict_resolution(&mut connection, &command).unwrap();
        drop(connection);
        let mut reopened = crate::sqlite::open_privileged_remote_apply_database(&path).unwrap();
        assert_eq!(
            apply_prepared_note_conflict_resolution(&mut reopened, &command).unwrap(),
            ApplyPreparedNoteConflictResolutionResult::AlreadyApplied
        );
        assert_eq!(
            reopened
                .connection()
                .query_row(
                    "SELECT count(*) FROM cloud_sync_note_resolution_outbox",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
        assert_eq!(
            reopened
                .connection()
                .query_row(
                    "SELECT count(*) FROM cloud_sync_note_resolution_dependencies",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            2
        );
        let mut changed_value: serde_json::Value =
            serde_json::from_slice(&command.canonical_payload).unwrap();
        changed_value["result"]["note"]["content"] =
            serde_json::Value::String("changed bytes".into());
        let changed = PrepareNoteConflictResolutionCommand {
            canonical_payload: canonical_json(&changed_value).unwrap().into_bytes(),
            ..command
        };
        assert!(matches!(
            apply_prepared_note_conflict_resolution(&mut reopened, &changed),
            Err(PrepareNoteConflictResolutionError::ConflictingResolution)
        ));
        drop(reopened);
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    fn resolution_sealing_is_atomic_exact_and_restart_durable() {
        let (root, path) = conflict_database_path("resolution-sealing-restart");
        let mut connection = configured_database(Connection::open(&path).unwrap());
        let (resolution, _, _) = applied_resolution_for_sealing(&mut connection);
        let seal = resolution_seal_command(&resolution);
        assert_eq!(
            list_unsealed_note_resolution_intents(
                connection.connection_mut_for_test(),
                ACCOUNT,
                10
            )
            .unwrap()
            .len(),
            1
        );
        assert_eq!(
            commit_sealed_note_resolution_event(connection.connection_mut_for_test(), &seal)
                .unwrap(),
            CommitSealedNoteResolutionResult::Sealed
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT lifecycle FROM cloud_sync_note_resolution_outbox",
                    [],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
            "sealed_local"
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT count(*) FROM cloud_sync_event_objects WHERE event_id=?1",
                    [&seal.event_id],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
        assert!(list_unsealed_note_resolution_intents(
            connection.connection_mut_for_test(),
            ACCOUNT,
            10
        )
        .unwrap()
        .is_empty());
        drop(connection);
        let mut reopened = crate::sqlite::open_database(&path).unwrap();
        assert_eq!(
            commit_sealed_note_resolution_event(&mut reopened, &seal).unwrap(),
            CommitSealedNoteResolutionResult::AlreadySealed
        );
        let stored: (Vec<u8>, Vec<u8>) = reopened
            .query_row(
                "SELECT nonce,ciphertext FROM cloud_sync_event_objects WHERE event_id=?1",
                [&seal.event_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(stored, (vec![0; 24], vec![0; 16]));
        let mut changed = seal.clone();
        changed.envelope.nonce = "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEB".into();
        assert!(matches!(
            commit_sealed_note_resolution_event(&mut reopened, &changed),
            Err(NoteSyncError::ConflictingEncryptedObject)
        ));
        assert_eq!(
            reopened
                .query_row(
                    "SELECT nonce,ciphertext FROM cloud_sync_event_objects WHERE event_id=?1",
                    [&seal.event_id],
                    |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, Vec<u8>>(1)?))
                )
                .unwrap(),
            stored
        );
        assert!(reopened
            .execute(
                "UPDATE cloud_sync_event_objects SET nonce=?1 WHERE event_id=?2",
                rusqlite::params![vec![3_u8; 24], seal.event_id]
            )
            .is_err());
        assert!(reopened
            .execute(
                "DELETE FROM cloud_sync_event_objects WHERE event_id=?1",
                [&seal.event_id]
            )
            .is_err());
        drop(reopened);
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    fn resolution_sealing_rejects_scope_payload_collision_and_rolls_back_injected_failure() {
        let mut injected = database();
        let (resolution, _, _) = applied_resolution_for_sealing(&mut injected);
        let seal = resolution_seal_command(&resolution);
        assert!(commit_sealed_note_resolution_event_inner(
            injected.connection_mut_for_test(),
            &seal,
            true
        )
        .is_err());
        assert_eq!(
            injected
                .connection()
                .query_row(
                    "SELECT count(*) FROM cloud_sync_event_objects WHERE event_id=?1",
                    [&seal.event_id],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        assert_eq!(
            injected
                .connection()
                .query_row(
                    "SELECT lifecycle FROM cloud_sync_note_resolution_outbox",
                    [],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
            "local_pending"
        );
        for changed in [
            CommitSealedNoteResolutionCommand {
                account_id: "other".into(),
                ..seal.clone()
            },
            CommitSealedNoteResolutionCommand {
                device_id: REMOTE_DEVICE.into(),
                ..seal.clone()
            },
            CommitSealedNoteResolutionCommand {
                project_id: "other".into(),
                ..seal.clone()
            },
            CommitSealedNoteResolutionCommand {
                entity_id: "other".into(),
                ..seal.clone()
            },
            CommitSealedNoteResolutionCommand {
                expected_canonical_payload: encode_canonical_base64url(b"{}"),
                ..seal.clone()
            },
        ] {
            assert!(commit_sealed_note_resolution_event(
                injected.connection_mut_for_test(),
                &changed
            )
            .is_err());
        }
        injected.connection().execute("INSERT INTO cloud_sync_event_objects(account_id,event_id,crypto_version,aad_version,nonce,ciphertext,stored_at) VALUES(?1,?2,1,1,?3,?4,'now')",rusqlite::params![ACCOUNT,seal.event_id,vec![9_u8;24],vec![9_u8;16]]).unwrap();
        assert!(matches!(
            commit_sealed_note_resolution_event(injected.connection_mut_for_test(), &seal),
            Err(NoteSyncError::ConflictingEncryptedObject)
        ));
        assert_eq!(
            injected
                .connection()
                .query_row(
                    "SELECT nonce FROM cloud_sync_event_objects WHERE event_id=?1",
                    [&seal.event_id],
                    |r| r.get::<_, Vec<u8>>(0)
                )
                .unwrap(),
            vec![9; 24]
        );
    }

    #[test]
    fn resolution_readiness_rechecks_exact_remote_and_local_publication_proof() {
        let mut connection = database();
        let (resolution, local, remote) = applied_resolution_for_sealing(&mut connection);
        let seal = resolution_seal_command(&resolution);
        commit_sealed_note_resolution_event(connection.connection_mut_for_test(), &seal).unwrap();
        assert!(
            !read_note_resolution_readiness(
                connection.connection_mut_for_test(),
                ACCOUNT,
                &seal.event_id
            )
            .unwrap()
            .ready
        );
        let generation:i64=connection.connection().query_row("SELECT local_mutation_generation FROM cloud_sync_note_resolution_dependencies WHERE parent_event_id=?1",[&local],|r|r.get(0)).unwrap();
        commit_sealed_note_sync_event(
            connection.connection_mut_for_test(),
            &CommitSealedNoteSyncEventCommand {
                event_id: local.clone(),
                expected_mutation_generation: generation,
                envelope: EncryptedNoteSyncEnvelope {
                    crypto_version: 1,
                    aad_version: 1,
                    nonce: "AgICAgICAgICAgICAgICAgICAgICAgIC".into(),
                    ciphertext: "AgICAgICAgICAgICAgICAg".into(),
                },
            },
        )
        .unwrap();
        assert!(
            !read_note_resolution_readiness(
                connection.connection_mut_for_test(),
                ACCOUNT,
                &seal.event_id
            )
            .unwrap()
            .ready
        );
        commit_note_sync_upload_acceptance(
            connection.connection_mut_for_test(),
            &CommitNoteSyncUploadAcceptanceCommand {
                account_id: ACCOUNT.into(),
                device_id: PULLING_DEVICE.into(),
                receipts: vec![NoteSyncUploadReceipt {
                    event_id: local.clone(),
                    server_sequence: 42,
                    duplicate: false,
                }],
            },
        )
        .unwrap();
        assert!(
            read_note_resolution_readiness(
                connection.connection_mut_for_test(),
                ACCOUNT,
                &seal.event_id
            )
            .unwrap()
            .ready
        );
        connection
            .connection()
            .execute(
                "UPDATE cloud_sync_inbox SET server_sequence=99 WHERE event_id=?1",
                [&remote],
            )
            .unwrap();
        assert!(
            !read_note_resolution_readiness(
                connection.connection_mut_for_test(),
                ACCOUNT,
                &seal.event_id
            )
            .unwrap()
            .ready
        );
        connection
            .connection()
            .execute(
                "UPDATE cloud_sync_inbox SET server_sequence=2 WHERE event_id=?1",
                [&remote],
            )
            .unwrap();
        assert!(
            read_note_resolution_readiness(
                connection.connection_mut_for_test(),
                ACCOUNT,
                &seal.event_id
            )
            .unwrap()
            .ready
        );
        connection
            .connection()
            .execute(
                "UPDATE cloud_sync_inbox SET project_id='other' WHERE event_id=?1",
                [&remote],
            )
            .unwrap();
        assert!(
            !read_note_resolution_readiness(
                connection.connection_mut_for_test(),
                ACCOUNT,
                &seal.event_id
            )
            .unwrap()
            .ready
        );
        connection
            .connection()
            .execute(
                "UPDATE cloud_sync_inbox SET project_id='p',sync_revision=3 WHERE event_id=?1",
                [&remote],
            )
            .unwrap();
        assert!(
            !read_note_resolution_readiness(
                connection.connection_mut_for_test(),
                ACCOUNT,
                &seal.event_id
            )
            .unwrap()
            .ready
        );
        connection
            .connection()
            .execute(
                "UPDATE cloud_sync_inbox SET sync_revision=2 WHERE event_id=?1",
                [&remote],
            )
            .unwrap();
        assert!(
            read_note_resolution_readiness(
                connection.connection_mut_for_test(),
                ACCOUNT,
                &seal.event_id
            )
            .unwrap()
            .ready
        );
        connection
            .connection()
            .execute(
                "DELETE FROM cloud_sync_upload_receipts WHERE event_id=?1",
                [&local],
            )
            .unwrap();
        assert!(
            !read_note_resolution_readiness(
                connection.connection_mut_for_test(),
                ACCOUNT,
                &seal.event_id
            )
            .unwrap()
            .ready
        );
        connection.connection().execute("INSERT INTO cloud_sync_upload_receipts(account_id,event_id,device_id,server_sequence,duplicate,accepted_at) VALUES(?1,?2,?3,42,0,'now')",rusqlite::params![ACCOUNT,local,PULLING_DEVICE]).unwrap();
        assert!(
            read_note_resolution_readiness(
                connection.connection_mut_for_test(),
                ACCOUNT,
                &seal.event_id
            )
            .unwrap()
            .ready
        );
        connection
            .connection()
            .execute("DELETE FROM cloud_sync_inbox WHERE event_id=?1", [&remote])
            .unwrap();
        assert!(
            !read_note_resolution_readiness(
                connection.connection_mut_for_test(),
                ACCOUNT,
                &seal.event_id
            )
            .unwrap()
            .ready
        );
        assert!(connection
            .connection()
            .execute(
                "UPDATE cloud_sync_note_resolution_outbox SET canonical_payload=x'7b7d'",
                []
            )
            .is_err());
        assert!(connection
            .connection()
            .execute("DELETE FROM cloud_sync_note_resolution_outbox", [])
            .is_err());
        assert!(connection
            .connection()
            .execute(
                "UPDATE cloud_sync_note_resolution_dependencies SET snapshot_json='{}'",
                []
            )
            .is_err());
        assert!(connection
            .connection()
            .execute("DELETE FROM cloud_sync_note_resolution_dependencies", [])
            .is_err());
    }

    #[test]
    fn resolution_readiness_requires_exact_remote_applied_causal_history() {
        let mut connection = database();
        apply_create(&mut connection);
        let applied = received(
            &connection,
            plaintext(
                UPDATE_EVENT,
                Some(CREATE_EVENT),
                2,
                "upsert",
                "2026-01-02T00:00:00.000000Z",
                "first remote",
            ),
            2,
            REMOTE_DEVICE,
        );
        assert_eq!(
            apply_verified_received_note(&mut connection, &applied).unwrap(),
            ApplyVerifiedReceivedNoteResult::Applied
        );
        let sibling_id = "123e4567-e89b-42d3-a456-426614174103";
        let sibling = received(
            &connection,
            plaintext(
                sibling_id,
                Some(CREATE_EVENT),
                2,
                "upsert",
                "2026-01-02T00:01:00.000000Z",
                "second remote",
            ),
            3,
            "123e4567-e89b-42d3-a456-426614174003",
        );
        assert_eq!(
            apply_verified_received_note(&mut connection, &sibling).unwrap(),
            ApplyVerifiedReceivedNoteResult::Conflict
        );
        let resolution = resolution_command(&connection, "choose_version", Some(sibling_id), None);
        prepare_note_conflict_resolution(connection.connection_mut_for_test(), &resolution)
            .unwrap();
        apply_prepared_note_conflict_resolution(&mut connection, &resolution).unwrap();
        let seal = resolution_seal_command(&resolution);
        commit_sealed_note_resolution_event(connection.connection_mut_for_test(), &seal).unwrap();
        assert!(
            read_note_resolution_readiness(
                connection.connection_mut_for_test(),
                ACCOUNT,
                &seal.event_id
            )
            .unwrap()
            .ready
        );
        // Simulate corrupted durable history after proving the production
        // immutability trigger exists; readiness must still fail closed.
        assert!(connection
            .connection()
            .execute(
                "UPDATE cloud_sync_note_causal_history SET revision=9 WHERE event_id=?1",
                [UPDATE_EVENT]
            )
            .is_err());
        connection
            .connection()
            .execute_batch("DROP TRIGGER cloud_sync_note_causal_history_immutable_update")
            .unwrap();
        connection
            .connection()
            .execute(
                "UPDATE cloud_sync_note_causal_history SET revision=9 WHERE event_id=?1",
                [UPDATE_EVENT],
            )
            .unwrap();
        assert!(
            !read_note_resolution_readiness(
                connection.connection_mut_for_test(),
                ACCOUNT,
                &seal.event_id
            )
            .unwrap()
            .ready
        );
        connection.connection().execute("UPDATE cloud_sync_note_causal_history SET revision=2,server_sequence=9 WHERE event_id=?1",[UPDATE_EVENT]).unwrap();
        assert!(
            !read_note_resolution_readiness(
                connection.connection_mut_for_test(),
                ACCOUNT,
                &seal.event_id
            )
            .unwrap()
            .ready
        );
        connection
            .connection()
            .execute(
                "UPDATE cloud_sync_note_causal_history SET server_sequence=2 WHERE event_id=?1",
                [UPDATE_EVENT],
            )
            .unwrap();
        assert!(
            read_note_resolution_readiness(
                connection.connection_mut_for_test(),
                ACCOUNT,
                &seal.event_id
            )
            .unwrap()
            .ready
        );
    }

    #[test]
    fn sealed_resolution_upload_reader_rechecks_parent_proofs_and_hides_plaintext() {
        let mut connection = database();
        let (resolution, local, _) = applied_resolution_for_sealing(&mut connection);
        let seal = resolution_seal_command(&resolution);
        commit_sealed_note_resolution_event(connection.connection_mut_for_test(), &seal).unwrap();
        assert!(list_sealed_note_resolution_uploads(
            connection.connection_mut_for_test(),
            &resolution_upload_list_command()
        )
        .unwrap()
        .is_empty());
        let generation:i64=connection.connection().query_row("SELECT local_mutation_generation FROM cloud_sync_note_resolution_dependencies WHERE parent_event_id=?1",[&local],|r|r.get(0)).unwrap();
        commit_sealed_note_sync_event(
            connection.connection_mut_for_test(),
            &CommitSealedNoteSyncEventCommand {
                event_id: local.clone(),
                expected_mutation_generation: generation,
                envelope: EncryptedNoteSyncEnvelope {
                    crypto_version: 1,
                    aad_version: 1,
                    nonce: "AgICAgICAgICAgICAgICAgICAgICAgIC".into(),
                    ciphertext: "AgICAgICAgICAgICAgICAg".into(),
                },
            },
        )
        .unwrap();
        commit_note_sync_upload_acceptance(
            connection.connection_mut_for_test(),
            &CommitNoteSyncUploadAcceptanceCommand {
                account_id: ACCOUNT.into(),
                device_id: PULLING_DEVICE.into(),
                receipts: vec![NoteSyncUploadReceipt {
                    event_id: local.clone(),
                    server_sequence: 42,
                    duplicate: false,
                }],
            },
        )
        .unwrap();
        let items = list_sealed_note_resolution_uploads(
            connection.connection_mut_for_test(),
            &resolution_upload_list_command(),
        )
        .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].event_id, seal.event_id);
        assert_eq!(items[0].operation, "resolution");
        assert_eq!(items[0].envelope.nonce, seal.envelope.nonce);
        assert!(!format!("{:?}", items[0]).contains("canonical_payload"));
        connection
            .connection()
            .execute(
                "DELETE FROM cloud_sync_upload_receipts WHERE event_id=?1",
                [&local],
            )
            .unwrap();
        assert!(list_sealed_note_resolution_uploads(
            connection.connection_mut_for_test(),
            &resolution_upload_list_command()
        )
        .unwrap()
        .is_empty());
    }

    #[test]
    fn resolution_upload_receipt_is_atomic_idempotent_and_preserves_envelope() {
        let (root, path) = conflict_database_path("resolution-upload-receipt");
        let mut connection = configured_database(Connection::open(&path).unwrap());
        let (resolution, local, _) = applied_resolution_for_sealing(&mut connection);
        let seal = resolution_seal_command(&resolution);
        commit_sealed_note_resolution_event(connection.connection_mut_for_test(), &seal).unwrap();
        let generation:i64=connection.connection().query_row("SELECT local_mutation_generation FROM cloud_sync_note_resolution_dependencies WHERE parent_event_id=?1",[&local],|r|r.get(0)).unwrap();
        commit_sealed_note_sync_event(
            connection.connection_mut_for_test(),
            &CommitSealedNoteSyncEventCommand {
                event_id: local.clone(),
                expected_mutation_generation: generation,
                envelope: EncryptedNoteSyncEnvelope {
                    crypto_version: 1,
                    aad_version: 1,
                    nonce: "AgICAgICAgICAgICAgICAgICAgICAgIC".into(),
                    ciphertext: "AgICAgICAgICAgICAgICAg".into(),
                },
            },
        )
        .unwrap();
        commit_note_sync_upload_acceptance(
            connection.connection_mut_for_test(),
            &CommitNoteSyncUploadAcceptanceCommand {
                account_id: ACCOUNT.into(),
                device_id: PULLING_DEVICE.into(),
                receipts: vec![NoteSyncUploadReceipt {
                    event_id: local,
                    server_sequence: 42,
                    duplicate: false,
                }],
            },
        )
        .unwrap();
        assert!(matches!(
            commit_note_resolution_upload_acceptance(
                connection.connection_mut_for_test(),
                &resolution_receipt_command(&seal.event_id, 42)
            ),
            Err(NoteSyncError::ConflictingUploadReceipt)
        ));
        let receipt = resolution_receipt_command(&seal.event_id, 43);
        assert!(commit_note_resolution_upload_acceptance_inner(
            connection.connection_mut_for_test(),
            &receipt,
            true
        )
        .is_err());
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT lifecycle FROM cloud_sync_note_resolution_outbox",
                    [],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
            "sealed_local"
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT count(*) FROM cloud_sync_note_resolution_upload_receipts",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        assert_eq!(
            commit_note_resolution_upload_acceptance(
                connection.connection_mut_for_test(),
                &receipt
            )
            .unwrap(),
            vec![CommitNoteResolutionUploadAcceptanceResult::Accepted]
        );
        let envelope: (Vec<u8>, Vec<u8>) = connection
            .connection()
            .query_row(
                "SELECT nonce,ciphertext FROM cloud_sync_event_objects WHERE event_id=?1",
                [&seal.event_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        drop(connection);
        let mut reopened = crate::sqlite::open_database(&path).unwrap();
        assert_eq!(
            commit_note_resolution_upload_acceptance(&mut reopened, &receipt).unwrap(),
            vec![CommitNoteResolutionUploadAcceptanceResult::AlreadyAccepted]
        );
        let mut replay = receipt.clone();
        replay.receipts[0].duplicate = true;
        assert_eq!(
            commit_note_resolution_upload_acceptance(&mut reopened, &replay).unwrap(),
            vec![CommitNoteResolutionUploadAcceptanceResult::AlreadyAccepted]
        );
        assert_eq!(reopened.query_row("SELECT duplicate FROM cloud_sync_note_resolution_upload_receipts WHERE resolution_event_id=?1",[&seal.event_id],|r|r.get::<_,i64>(0)).unwrap(),0);
        assert_eq!(
            reopened
                .query_row(
                    "SELECT nonce,ciphertext FROM cloud_sync_event_objects WHERE event_id=?1",
                    [&seal.event_id],
                    |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, Vec<u8>>(1)?))
                )
                .unwrap(),
            envelope
        );
        assert!(matches!(
            commit_note_resolution_upload_acceptance(
                &mut reopened,
                &resolution_receipt_command(&seal.event_id, 44)
            ),
            Err(NoteSyncError::ConflictingUploadReceipt)
        ));
        drop(reopened);
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    fn frozen_resolution_parent_can_seal_and_record_upload_acceptance() {
        let mut connection = database();
        let (local, remote) = prepare_edit_conflict(&mut connection);
        let command = resolution_command(&connection, "choose_version", Some(&remote), None);
        prepare_note_conflict_resolution(connection.connection_mut_for_test(), &command).unwrap();
        apply_prepared_note_conflict_resolution(&mut connection, &command).unwrap();
        let (generation, snapshot): (i64, String) = connection
            .connection()
            .query_row(
                "SELECT local_mutation_generation,snapshot_json
             FROM cloud_sync_note_resolution_dependencies WHERE parent_event_id=?1",
                [&local],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert!(connection
            .connection()
            .execute(
                "UPDATE cloud_sync_note_intents
             SET mutation_generation=mutation_generation+1 WHERE event_id=?1",
                [&local],
            )
            .is_err());
        assert_eq!(
            commit_sealed_note_sync_event(
                connection.connection_mut_for_test(),
                &CommitSealedNoteSyncEventCommand {
                    event_id: local.clone(),
                    expected_mutation_generation: generation,
                    envelope: EncryptedNoteSyncEnvelope {
                        crypto_version: 1,
                        aad_version: 1,
                        nonce: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into(),
                        ciphertext: "AAAAAAAAAAAAAAAAAAAAAA".into(),
                    },
                },
            )
            .unwrap(),
            CommitSealedNoteSyncEventResult::Sealed
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT lifecycle FROM cloud_sync_outbox WHERE event_id=?1",
                    [&local],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "sealed"
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT count(*) FROM cloud_sync_note_intents WHERE event_id=?1",
                    [&local],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
        let queued =
            list_sealed_note_sync_outbox(connection.connection_mut_for_test(), ACCOUNT, 10)
                .unwrap();
        assert!(queued.iter().any(|event| event.event_id == local));
        assert_eq!(
            commit_note_sync_upload_acceptance(
                connection.connection_mut_for_test(),
                &CommitNoteSyncUploadAcceptanceCommand {
                    account_id: ACCOUNT.into(),
                    device_id: PULLING_DEVICE.into(),
                    receipts: vec![NoteSyncUploadReceipt {
                        event_id: local.clone(),
                        server_sequence: 42,
                        duplicate: false,
                    }],
                },
            )
            .unwrap(),
            vec![CommitNoteSyncUploadAcceptanceResult::Accepted]
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT lifecycle FROM cloud_sync_outbox WHERE event_id=?1",
                    [&local],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "accepted"
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT local_mutation_generation,local_outbox_lifecycle,
                    upload_receipt_sequence,snapshot_json
             FROM cloud_sync_note_resolution_dependencies WHERE parent_event_id=?1",
                    [&local],
                    |row| Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        row.get::<_, String>(3)?
                    )),
                )
                .unwrap(),
            (generation, "unsealed".into(), None, snapshot)
        );
        assert_eq!(
            connection
                .connection()
                .query_row(
                    "SELECT lifecycle FROM cloud_sync_note_conflict_groups
             WHERE lifecycle='resolving'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "resolving"
        );
    }

    #[test]
    fn applied_resolution_rejects_stale_tip_generation_and_scope() {
        let mut coalesced = database();
        let (_, remote) = prepare_edit_conflict(&mut coalesced);
        let command = resolution_command(&coalesced, "choose_version", Some(&remote), None);
        prepare_note_conflict_resolution(coalesced.connection_mut_for_test(), &command).unwrap();
        let snapshot: String = coalesced
            .connection()
            .query_row("SELECT payload_json FROM notes WHERE id='n'", [], |row| {
                row.get(0)
            })
            .unwrap();
        let transaction = coalesced.connection().unchecked_transaction().unwrap();
        assert!(matches!(
            prepare_unsealed_note_intent(
                &transaction,
                PrepareNoteIntent {
                    project_id: "p",
                    entity_id: "n",
                    operation: NoteSyncOperation::Upsert,
                    updated_at: "2026-01-04T00:00:00.000000Z",
                    deleted_at: None,
                    snapshot_json: &snapshot,
                    state_updated_at: "now",
                }
            ),
            Err(NoteSyncError::InvalidSealState(
                "resolution is not durably applied"
            ))
        ));
        transaction.rollback().unwrap();
        assert_eq!(
            coalesced
                .connection()
                .query_row(
                    "SELECT count(*) FROM cloud_sync_note_resolution_outbox",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );

        let mut competing = database();
        let (_, remote) = prepare_edit_conflict(&mut competing);
        let command = resolution_command(&competing, "choose_version", Some(&remote), None);
        prepare_note_conflict_resolution(competing.connection_mut_for_test(), &command).unwrap();
        let third = received(
            &competing,
            plaintext(
                "123e4567-e89b-42d3-a456-426614174103",
                Some(CREATE_EVENT),
                2,
                "upsert",
                "2026-01-02T00:00:01.000000Z",
                "third",
            ),
            3,
            REMOTE_DEVICE,
        );
        assert_eq!(
            apply_verified_received_note(&mut competing, &third).unwrap(),
            ApplyVerifiedReceivedNoteResult::Conflict
        );
        assert!(matches!(
            apply_prepared_note_conflict_resolution(&mut competing, &command),
            Err(PrepareNoteConflictResolutionError::StaleConflict)
        ));

        let mut wrong = database();
        let (_, remote) = prepare_edit_conflict(&mut wrong);
        let command = resolution_command(&wrong, "choose_version", Some(&remote), None);
        prepare_note_conflict_resolution(wrong.connection_mut_for_test(), &command).unwrap();
        let mut wrong_device = command.clone();
        wrong_device.device_id = REMOTE_DEVICE.into();
        assert!(matches!(
            apply_prepared_note_conflict_resolution(&mut wrong, &wrong_device),
            Err(PrepareNoteConflictResolutionError::ScopeMismatch)
        ));
        let mut wrong_account = command.clone();
        wrong_account.account_id = "other-account".into();
        assert!(matches!(
            apply_prepared_note_conflict_resolution(&mut wrong, &wrong_account),
            Err(PrepareNoteConflictResolutionError::ScopeMismatch)
        ));
        let mut wrong_project_value: serde_json::Value =
            serde_json::from_slice(&command.canonical_payload).unwrap();
        wrong_project_value["header"]["project_id"] =
            serde_json::Value::String("other-project".into());
        wrong_project_value["result"]["note"]["project_id"] =
            serde_json::Value::String("other-project".into());
        let wrong_project = PrepareNoteConflictResolutionCommand {
            canonical_payload: canonical_json(&wrong_project_value).unwrap().into_bytes(),
            ..command
        };
        assert!(matches!(
            apply_prepared_note_conflict_resolution(&mut wrong, &wrong_project),
            Err(PrepareNoteConflictResolutionError::ScopeMismatch)
        ));
    }

    #[test]
    fn applied_resolution_rolls_back_clone_collision_failure_and_missing_proof() {
        let mut collision = database();
        let (local, remote) = prepare_edit_conflict(&mut collision);
        let command = resolution_command(&collision, "keep_both", Some(&local), Some(&remote));
        prepare_note_conflict_resolution(collision.connection_mut_for_test(), &command).unwrap();
        collision.connection().execute(
            "INSERT INTO cloud_sync_entities(account_id,project_id,entity_id,entity_type,head_event_id,head_sync_revision,updated_at)
             VALUES(?1,'p','n-copy','note',?2,1,'now')",rusqlite::params![ACCOUNT,CREATE_EVENT],
        ).unwrap();
        let before: String = collision
            .connection()
            .query_row("SELECT payload_json FROM notes WHERE id='n'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(matches!(
            apply_prepared_note_conflict_resolution(&mut collision, &command),
            Err(PrepareNoteConflictResolutionError::ConflictingResolution)
        ));
        assert_eq!(
            collision
                .connection()
                .query_row("SELECT payload_json FROM notes WHERE id='n'", [], |row| row
                    .get::<_, String>(0))
                .unwrap(),
            before
        );
        assert_eq!(
            collision
                .connection()
                .query_row(
                    "SELECT count(*) FROM cloud_sync_note_resolution_outbox",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );

        let mut injected = database();
        let (_, remote) = prepare_edit_conflict(&mut injected);
        let command = resolution_command(&injected, "choose_version", Some(&remote), None);
        prepare_note_conflict_resolution(injected.connection_mut_for_test(), &command).unwrap();
        let before: String = injected
            .connection()
            .query_row("SELECT payload_json FROM notes WHERE id='n'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(matches!(
            apply_prepared_note_conflict_resolution_inner(&mut injected, &command, true),
            Err(PrepareNoteConflictResolutionError::InjectedFailure)
        ));
        assert_eq!(
            injected
                .connection()
                .query_row("SELECT payload_json FROM notes WHERE id='n'", [], |row| row
                    .get::<_, String>(0))
                .unwrap(),
            before
        );
        assert_eq!(
            injected
                .connection()
                .query_row(
                    "SELECT lifecycle FROM cloud_sync_note_pending_resolutions",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            "prepared"
        );
        assert_eq!(
            injected
                .connection()
                .query_row(
                    "SELECT lifecycle FROM cloud_sync_note_conflict_groups",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            "open"
        );

        let mut second_mutation = database();
        let (local, remote) = prepare_edit_conflict(&mut second_mutation);
        let command =
            resolution_command(&second_mutation, "keep_both", Some(&remote), Some(&local));
        prepare_note_conflict_resolution(second_mutation.connection_mut_for_test(), &command)
            .unwrap();
        let before: String = second_mutation
            .connection()
            .query_row("SELECT payload_json FROM notes WHERE id='n'", [], |row| {
                row.get(0)
            })
            .unwrap();
        second_mutation
            .connection()
            .execute_batch(
                "CREATE TRIGGER inject_clone_failure BEFORE INSERT ON notes
             WHEN NEW.id='n-copy'
             BEGIN SELECT RAISE(ABORT,'injected_clone_failure'); END;",
            )
            .unwrap();
        assert!(apply_prepared_note_conflict_resolution(&mut second_mutation, &command).is_err());
        assert_eq!(
            second_mutation
                .connection()
                .query_row("SELECT payload_json FROM notes WHERE id='n'", [], |row| row
                    .get::<_, String>(0),)
                .unwrap(),
            before
        );
        assert_eq!(
            second_mutation
                .connection()
                .query_row("SELECT count(*) FROM notes WHERE id='n-copy'", [], |row| {
                    row.get::<_, i64>(0)
                },)
                .unwrap(),
            0
        );
        assert_eq!(
            second_mutation
                .connection()
                .query_row(
                    "SELECT count(*) FROM cloud_sync_note_resolution_outbox",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
        assert_eq!(
            second_mutation
                .connection()
                .query_row(
                    "SELECT lifecycle FROM cloud_sync_note_pending_resolutions",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "prepared"
        );
        assert_eq!(
            second_mutation
                .connection()
                .query_row(
                    "SELECT lifecycle FROM cloud_sync_note_conflict_groups",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "open"
        );

        let mut proof = database();
        let (_, remote) = prepare_edit_conflict(&mut proof);
        let command = resolution_command(&proof, "choose_version", Some(&remote), None);
        prepare_note_conflict_resolution(proof.connection_mut_for_test(), &command).unwrap();
        proof
            .connection()
            .execute(
                "UPDATE cloud_sync_note_conflict_groups
             SET common_parent_event_id='123e4567-e89b-42d3-a456-426614174199'",
                [],
            )
            .unwrap();
        assert!(matches!(
            apply_prepared_note_conflict_resolution(&mut proof, &command),
            Err(PrepareNoteConflictResolutionError::MissingCausalProof)
        ));
    }
}

impl std::fmt::Display for NoteSyncError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Database(error) => write!(formatter, "SQLite error: {error}"),
            Self::InvalidSnapshot(message) => {
                write!(formatter, "Invalid Note sync snapshot: {message}")
            }
            Self::RevisionOverflow => write!(
                formatter,
                "Note sync revision exceeds the wire integer limit"
            ),
            Self::LocalOrdinalOverflow => write!(formatter, "Note sync local ordinal overflow"),
            Self::MutationGenerationOverflow => {
                write!(formatter, "Note sync mutation generation overflow")
            }
            Self::SealAttemptOverflow => write!(formatter, "Note sync seal attempt overflow"),
            Self::ConflictingHeads => write!(formatter, "Note sync entity has conflicting heads"),
            Self::InvalidListLimit => write!(formatter, "Invalid Note sync intent list limit"),
            Self::InvalidOutboxRead(message) => {
                write!(
                    formatter,
                    "Invalid sealed Note sync outbox state: {message}"
                )
            }
            Self::InvalidEnvelope(message) => {
                write!(formatter, "Invalid encrypted Note sync envelope: {message}")
            }
            Self::InvalidSealState(message) => {
                write!(formatter, "Invalid durable Note sealing state: {message}")
            }
            Self::MissingEvent => write!(formatter, "Note sync outbox event does not exist"),
            Self::MissingIntent => write!(formatter, "Unsealed Note sync intent is missing"),
            Self::UnexpectedLifecycle => {
                write!(formatter, "Note sync event has an unexpected lifecycle")
            }
            Self::SealedObjectMissing => {
                write!(formatter, "Sealed Note sync event has no encrypted object")
            }
            Self::ConflictingEncryptedObject => {
                write!(
                    formatter,
                    "Note sync event has a conflicting encrypted object"
                )
            }
            Self::ConflictingUploadReceipt => write!(
                formatter,
                "Note sync event has a conflicting server receipt"
            ),
            Self::Random(error) => {
                write!(formatter, "Could not generate Note sync event ID: {error}")
            }
        }
    }
}

impl std::error::Error for NoteSyncError {}

impl NoteSyncError {
    pub(crate) fn user_message(&self) -> &'static str {
        match self {
            Self::RevisionOverflow => "Исчерпан диапазон sync revision заметки.",
            Self::LocalOrdinalOverflow => "Исчерпан диапазон локального sync-порядка.",
            Self::MutationGenerationOverflow => {
                "Исчерпан диапазон поколений локального изменения заметки."
            }
            Self::SealAttemptOverflow => "Исчерпан диапазон попыток шифрования изменения заметки.",
            Self::ConflictingHeads => "Обнаружено конфликтующее локальное состояние синхронизации.",
            Self::Database(_)
            | Self::InvalidSnapshot(_)
            | Self::InvalidListLimit
            | Self::InvalidOutboxRead(_)
            | Self::InvalidEnvelope(_)
            | Self::InvalidSealState(_)
            | Self::MissingEvent
            | Self::MissingIntent
            | Self::UnexpectedLifecycle
            | Self::SealedObjectMissing
            | Self::ConflictingEncryptedObject
            | Self::ConflictingUploadReceipt
            | Self::Random(_) => {
                "Не удалось надёжно зарегистрировать изменение заметки для синхронизации."
            }
        }
    }
}

impl From<rusqlite::Error> for NoteSyncError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Database(error)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NoteSyncOperation {
    Upsert,
    Delete,
}

impl NoteSyncOperation {
    fn from_stored(value: &str) -> Result<Self, NoteSyncError> {
        match value {
            "upsert" => Ok(Self::Upsert),
            "delete" => Ok(Self::Delete),
            _ => Err(NoteSyncError::InvalidSealState("invalid operation")),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NoteSyncSealState {
    Pending,
    RetryableError,
    Blocked,
    InvariantError,
}

impl NoteSyncSealState {
    fn from_stored(value: &str) -> Result<Self, NoteSyncError> {
        match value {
            "pending" => Ok(Self::Pending),
            "retryable_error" => Ok(Self::RetryableError),
            "blocked" => Ok(Self::Blocked),
            "invariant_error" => Ok(Self::InvariantError),
            _ => Err(NoteSyncError::InvalidSealState("invalid seal state")),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NoteSyncSealErrorCode {
    KeyUnavailable,
    PayloadTooLarge,
    EncryptedSyncObjectTooLarge,
    DependencyNotSynced,
    UnsupportedContentFormat,
    InvalidNotePayload,
    CryptoContextInvalid,
    InvalidSyncMetadata,
    InvalidEnvelope,
    MetadataMismatch,
    RuntimeUnavailable,
}

impl NoteSyncSealErrorCode {
    fn as_str(self) -> &'static str {
        match self {
            Self::KeyUnavailable => "key_unavailable",
            Self::PayloadTooLarge => "payload_too_large",
            Self::EncryptedSyncObjectTooLarge => "encrypted_sync_object_too_large",
            Self::DependencyNotSynced => "dependency_not_synced",
            Self::UnsupportedContentFormat => "unsupported_content_format",
            Self::InvalidNotePayload => "invalid_note_payload",
            Self::CryptoContextInvalid => "crypto_context_invalid",
            Self::InvalidSyncMetadata => "invalid_sync_metadata",
            Self::InvalidEnvelope => "invalid_envelope",
            Self::MetadataMismatch => "metadata_mismatch",
            Self::RuntimeUnavailable => "runtime_unavailable",
        }
    }

    fn seal_state(self) -> NoteSyncSealState {
        match self {
            Self::KeyUnavailable
            | Self::PayloadTooLarge
            | Self::EncryptedSyncObjectTooLarge
            | Self::DependencyNotSynced
            | Self::UnsupportedContentFormat => NoteSyncSealState::Blocked,
            Self::RuntimeUnavailable => NoteSyncSealState::RetryableError,
            Self::InvalidNotePayload
            | Self::CryptoContextInvalid
            | Self::InvalidSyncMetadata
            | Self::InvalidEnvelope
            | Self::MetadataMismatch => NoteSyncSealState::InvariantError,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct UnsealedNoteSyncIntent {
    pub event_id: String,
    pub account_id: String,
    pub device_id: String,
    pub project_id: String,
    pub entity_id: String,
    pub entity_type: String,
    pub operation: NoteSyncOperation,
    pub revision: i64,
    pub parent_event_id: Option<String>,
    pub updated_at: String,
    pub deleted_at: Option<String>,
    pub local_ordinal: i64,
    pub mutation_generation: i64,
    pub snapshot_json: String,
    pub seal_state: NoteSyncSealState,
    pub seal_attempt_count: i64,
    pub last_error_code: Option<String>,
    pub next_attempt_at: Option<String>,
}

/// Read-only transport input assembled from one sealed outbox event and its
/// already-persisted encrypted object. This deliberately contains no intent
/// snapshot or key material.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct SealedNoteSyncOutboxItem {
    pub event_id: String,
    pub account_id: String,
    pub device_id: String,
    pub project_id: String,
    pub entity_id: String,
    pub entity_type: String,
    pub operation: NoteSyncOperation,
    pub revision: i64,
    pub parent_event_id: Option<String>,
    pub updated_at: String,
    pub deleted_at: Option<String>,
    pub local_ordinal: i64,
    pub envelope: EncryptedNoteSyncEnvelope,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NoteSyncUploadReceipt {
    pub event_id: String,
    pub server_sequence: i64,
    pub duplicate: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CommitNoteSyncUploadAcceptanceCommand {
    pub account_id: String,
    pub device_id: String,
    pub receipts: Vec<NoteSyncUploadReceipt>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NoteSyncUploadErrorCode {
    NetworkUnavailable,
    RequestTimeout,
    Http5xx,
    RateLimited,
    Unauthorized,
    DeviceNotRegistered,
    CloudProjectDisabled,
    InvalidProtocol,
    ConflictingEvent,
    MalformedReceipt,
    LocalAcceptanceFailed,
}

impl NoteSyncUploadErrorCode {
    fn as_str(self) -> &'static str {
        match self {
            Self::NetworkUnavailable => "network_unavailable",
            Self::RequestTimeout => "request_timeout",
            Self::Http5xx => "http_5xx",
            Self::RateLimited => "rate_limited",
            Self::Unauthorized => "unauthorized",
            Self::DeviceNotRegistered => "device_not_registered",
            Self::CloudProjectDisabled => "cloud_project_disabled",
            Self::InvalidProtocol => "invalid_protocol",
            Self::ConflictingEvent => "conflicting_event",
            Self::MalformedReceipt => "malformed_receipt",
            Self::LocalAcceptanceFailed => "local_acceptance_failed",
        }
    }

    fn retryable(self) -> bool {
        matches!(
            self,
            Self::NetworkUnavailable
                | Self::RequestTimeout
                | Self::Http5xx
                | Self::RateLimited
                | Self::LocalAcceptanceFailed
        )
    }
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct RecordNoteSyncUploadFailureCommand {
    pub account_id: String,
    pub device_id: String,
    pub event_ids: Vec<String>,
    pub error_code: NoteSyncUploadErrorCode,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CommitNoteSyncUploadAcceptanceResult {
    Accepted,
    AlreadyAccepted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NoteSyncPreflightIssueCode {
    MissingCreatedAt,
    InvalidCreatedAt,
    MissingUpdatedAt,
    InvalidUpdatedAt,
    InvalidNotePayload,
    DependencyNotSynced,
    UnsupportedContentFormat,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct NoteSyncPreflightIssue {
    pub note_id: String,
    pub code: NoteSyncPreflightIssueCode,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CloudProjectBootstrapMode {
    UploadExisting,
    ImportRemote,
}

impl CloudProjectBootstrapMode {
    fn as_str(&self) -> &'static str {
        match self {
            Self::UploadExisting => "upload_existing",
            Self::ImportRemote => "import_remote",
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PrepareCloudProjectBootstrapCommand {
    pub project_id: String,
    pub account_id: String,
    pub device_id: String,
    pub mode: CloudProjectBootstrapMode,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConfirmCloudProjectRegistrationCommand {
    pub project_id: String,
    pub account_id: String,
    pub device_id: String,
    pub bootstrap_id: String,
    pub remote_state: String,
    pub remote_high_water: i64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CloudProjectBootstrapScopeCommand {
    pub project_id: String,
    pub account_id: String,
    pub device_id: String,
    pub bootstrap_id: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ImportRemoteCloudProjectCommand {
    pub project_id: String,
    pub display_name: String,
    pub account_id: String,
    pub device_id: String,
    pub bootstrap_id: String,
    pub remote_high_water: i64,
    #[serde(default)]
    pub authenticated_metadata: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct CloudProjectBootstrapRecord {
    pub project_id: String,
    pub account_id: String,
    pub device_id: String,
    pub bootstrap_id: String,
    pub mode: String,
    pub phase: String,
    pub remote_state: Option<String>,
    pub initial_event_count: i64,
    pub initial_local_ordinal_hi: i64,
    pub remote_high_water: Option<i64>,
    pub initial_max_server_sequence: Option<i64>,
    pub blocked_reason: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct CloudProjectInitialCohortStatus {
    pub event_count: i64,
    pub accepted_count: i64,
    pub max_server_sequence: i64,
    pub complete: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecordNoteSyncSealFailureCommand {
    pub event_id: String,
    pub expected_mutation_generation: i64,
    pub error_code: NoteSyncSealErrorCode,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EncryptedNoteSyncEnvelope {
    pub crypto_version: i64,
    pub aad_version: i64,
    pub nonce: String,
    pub ciphertext: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CommitSealedNoteSyncEventCommand {
    pub event_id: String,
    pub expected_mutation_generation: i64,
    pub envelope: EncryptedNoteSyncEnvelope,
}

/// Private C17 queue input. Its plaintext is canonical v2 bytes, never a
/// reconstructed Note snapshot and never part of the v1 intent queue.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct UnsealedNoteResolutionIntent {
    pub event_id: String,
    pub account_id: String,
    pub device_id: String,
    pub project_id: String,
    pub entity_id: String,
    pub canonical_payload: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CommitSealedNoteResolutionCommand {
    pub event_id: String,
    pub account_id: String,
    pub canonical_user_id: String,
    pub device_id: String,
    pub project_id: String,
    pub entity_id: String,
    pub expected_canonical_payload: String,
    pub envelope: EncryptedNoteSyncEnvelope,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CommitSealedNoteResolutionResult {
    Sealed,
    AlreadySealed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct NoteResolutionReadiness {
    pub event_id: String,
    pub ready: bool,
}

/// Opaque v2 transport input. Canonical resolution plaintext remains in the
/// private outbox; this reader exposes only the frozen routing metadata and
/// durable envelope needed by a future uploader.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ListSealedNoteResolutionUploadsCommand {
    pub account_id: String,
    pub device_id: String,
    pub canonical_user_id: String,
    pub limit: u32,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct SealedNoteResolutionUploadItem {
    pub event_id: String,
    pub account_id: String,
    pub device_id: String,
    pub project_id: String,
    pub entity_id: String,
    pub entity_type: String,
    pub operation: String,
    pub revision: i64,
    pub updated_at: String,
    pub envelope: StoredEncryptedNoteSyncEnvelope,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CommitNoteResolutionUploadAcceptanceCommand {
    pub account_id: String,
    pub device_id: String,
    pub canonical_user_id: String,
    pub receipts: Vec<NoteSyncUploadReceipt>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CommitNoteResolutionUploadAcceptanceResult {
    Accepted,
    AlreadyAccepted,
}

/// Opaque, already transport-validated data accepted into the durable inbox.
/// This is intentionally separate from the outbox commands: receipt never
/// decrypts or applies a user note.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReadNoteSyncPullStateCommand {
    pub account_id: String,
    pub device_id: String,
    pub canonical_user_id: String,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct NoteSyncPullState {
    pub pull_cursor: i64,
    pub ack_cursor: i64,
}

/// A read-only, locally proven prefix that a later transport layer may ACK.
/// It is deliberately not evidence of a remote ACK.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct NoteSyncAckCandidate {
    pub current_ack_cursor: i64,
    pub candidate_cursor: i64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PrepareNoteSyncAckCommand {
    pub account_id: String,
    pub device_id: String,
    pub canonical_user_id: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CommitNoteSyncAckCommand {
    pub account_id: String,
    pub device_id: String,
    pub canonical_user_id: String,
    pub expected_old_ack_cursor: i64,
    pub acknowledged_cursor: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CommitNoteSyncAckResult {
    Advanced,
    AlreadyAcknowledged,
    AlreadyAdvanced,
    Stale,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CommitNoteSyncInboundPageCommand {
    pub account_id: String,
    pub device_id: String,
    pub canonical_user_id: String,
    pub expected_cursor: i64,
    pub next_cursor: i64,
    pub has_more: bool,
    pub items: Vec<InboundNoteSyncItem>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InboundNoteSyncItem {
    pub event_id: String,
    pub server_sequence: i64,
    pub source_device_id: String,
    pub project_id: String,
    pub entity_id: String,
    pub entity_type: String,
    pub operation: String,
    pub revision: i64,
    pub updated_at: String,
    pub deleted_at: Option<String>,
    pub envelope: Option<EncryptedNoteSyncEnvelope>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CommitMixedSyncInboundPageCommand {
    pub account_id: String,
    pub device_id: String,
    pub canonical_user_id: String,
    pub expected_cursor: i64,
    pub next_cursor: i64,
    pub has_more: bool,
    pub items: Vec<InboundSyncItem>,
}
impl From<&CommitNoteSyncInboundPageCommand> for CommitMixedSyncInboundPageCommand {
    fn from(c: &CommitNoteSyncInboundPageCommand) -> Self {
        Self {
            account_id: c.account_id.clone(),
            device_id: c.device_id.clone(),
            canonical_user_id: c.canonical_user_id.clone(),
            expected_cursor: c.expected_cursor,
            next_cursor: c.next_cursor,
            has_more: c.has_more,
            items: c
                .items
                .iter()
                .cloned()
                .map(InboundSyncItem::Project)
                .collect(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum InboundSyncItem {
    Project(InboundNoteSyncItem),
    Account(crate::account_sync::InboundAccountItem),
}
#[derive(Clone, Debug, Serialize)]
pub(crate) struct CommitNoteSyncInboundPageResult {
    pub committed_cursor: i64,
    pub new_events: u32,
    pub replayed_events: u32,
    pub has_more: bool,
}

/// Opaque received Note events for the TypeScript authenticated decryptor.
/// This command deliberately neither decrypts nor changes inbox state.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ListReceivedNoteSyncInboxCommand {
    pub account_id: String,
    pub device_id: String,
    pub canonical_user_id: String,
    pub limit: u32,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReceivedNoteSyncInboxPageKind {
    V1,
    Resolution,
    Content,
}

/// A bounded, independently paginated received Note stream. The `kind`
/// discriminator is intentionally closed so renderer input cannot select an
/// arbitrary inbox operation.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ListReceivedNoteSyncInboxPageCommand {
    pub account_id: String,
    pub device_id: String,
    pub canonical_user_id: String,
    pub kind: ReceivedNoteSyncInboxPageKind,
    pub limit: u32,
    pub after_server_sequence: i64,
}

/// Opaque orphaned v2 resolution events for a future TypeScript retry
/// coordinator. This command deliberately neither decrypts nor changes inbox
/// state.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ListOrphanNoteResolutionInboxCommand {
    pub account_id: String,
    pub device_id: String,
    pub canonical_user_id: String,
    pub limit: u32,
    pub after_server_sequence: i64,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ReceivedNoteSyncInboxItem {
    pub event_id: String,
    pub server_sequence: i64,
    pub source_device_id: String,
    pub project_id: String,
    pub entity_id: String,
    pub entity_type: String,
    pub operation: String,
    pub revision: i64,
    pub updated_at: String,
    pub deleted_at: Option<String>,
    pub envelope: StoredEncryptedNoteSyncEnvelope,
}

/// Inputs already authenticated by the TypeScript crypto boundary.  Rust
/// treats every routing/envelope field as untrusted until it matches the
/// durable received inbox row in the same SQLite transaction.
#[derive(Clone, Debug)]
pub(crate) struct ApplyVerifiedReceivedNoteCommand {
    pub account_id: String,
    pub canonical_user_id: String,
    pub pulling_device_id: String,
    pub event_id: String,
    pub server_sequence: i64,
    pub source_device_id: String,
    pub crypto_version: i64,
    pub aad_version: i64,
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
    pub plaintext: NoteSyncPlaintext,
}

/// Narrow renderer-to-Rust IPC shape. `plaintext` is canonical plaintext JSON
/// emitted only after TypeScript authenticated decryption; it is decoded again
/// immediately and is never persisted separately.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ApplyVerifiedReceivedNoteIpcCommand {
    pub account_id: String,
    pub canonical_user_id: String,
    pub pulling_device_id: String,
    pub event_id: String,
    pub server_sequence: i64,
    pub source_device_id: String,
    pub crypto_version: i64,
    pub aad_version: i64,
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
    pub plaintext: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ApplyVerifiedReceivedNoteResult {
    Applied,
    AlreadyApplied,
    SelfEchoApplied,
    Orphan,
    Conflict,
    Rejected,
}

#[derive(Clone, Debug)]
pub(crate) struct PrepareNoteConflictResolutionCommand {
    pub account_id: String,
    pub canonical_user_id: String,
    pub device_id: String,
    pub canonical_payload: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PrepareNoteConflictResolutionResult {
    Prepared,
    AlreadyPrepared,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ApplyPreparedNoteConflictResolutionResult {
    Applied,
    AlreadyApplied,
}

/// Result of the dormant peer-only resolution path. Self echoes remain
/// ACK-blocking until the separate D2B2 reconciliation command handles them.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ApplyVerifiedReceivedResolutionV2Result {
    Applied,
    AlreadyApplied,
    Orphan,
    SelfEchoPending,
}

/// IPC input after authenticated decryption in the TypeScript AMK lease.
/// Every field remains untrusted until it matches the durable inbox/object in
/// the same native transaction. The plaintext byte copy is zeroed by the IPC
/// boundary before this command returns.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ApplyVerifiedReceivedResolutionV2IpcCommand {
    pub account_id: String,
    pub canonical_user_id: String,
    pub pulling_device_id: String,
    pub event_id: String,
    pub server_sequence: i64,
    pub source_device_id: String,
    pub crypto_version: i64,
    pub aad_version: i64,
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
    pub plaintext: Vec<u8>,
}

/// Separate dormant boundary for a locally-created resolution returned by
/// pull. No local conflict identity, receipt semantics, or apply decision is
/// accepted from the renderer.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReconcileVerifiedReceivedResolutionSelfEchoIpcCommand {
    pub account_id: String,
    pub canonical_user_id: String,
    pub pulling_device_id: String,
    pub event_id: String,
    pub server_sequence: i64,
    pub source_device_id: String,
    pub crypto_version: i64,
    pub aad_version: i64,
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
    pub plaintext: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReconcileVerifiedReceivedResolutionSelfEchoResult {
    Reconciled,
    AlreadyReconciled,
}

#[derive(Debug)]
pub(crate) enum ApplyVerifiedReceivedResolutionV2Error {
    Database(rusqlite::Error),
    InvalidPayload,
    ScopeMismatch,
    ImmutableMismatch,
    StaleConflict,
    MissingCausalProof,
    CloneCollision,
    SelfEchoMismatch,
    InjectedFailure,
    ProtectedStorageFailure,
}

impl std::fmt::Display for ApplyVerifiedReceivedResolutionV2Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Database(error) => write!(formatter, "SQLite error: {error}"),
            Self::InvalidPayload => write!(formatter, "invalid canonical resolution v2 payload"),
            Self::ScopeMismatch => write!(formatter, "resolution account or device scope mismatch"),
            Self::ImmutableMismatch => {
                write!(formatter, "resolution inbox or encrypted object mismatch")
            }
            Self::StaleConflict => {
                write!(formatter, "resolution no longer matches the local conflict")
            }
            Self::MissingCausalProof => write!(formatter, "resolution causal proof is incomplete"),
            Self::CloneCollision => write!(formatter, "resolution clone identity collides"),
            Self::SelfEchoMismatch => write!(
                formatter,
                "resolution self echo does not match local outbox"
            ),
            Self::InjectedFailure => write!(formatter, "injected peer resolution failure"),
            Self::ProtectedStorageFailure => {
                write!(formatter, "protected resolution storage failed")
            }
        }
    }
}

impl std::error::Error for ApplyVerifiedReceivedResolutionV2Error {}

impl From<rusqlite::Error> for ApplyVerifiedReceivedResolutionV2Error {
    fn from(error: rusqlite::Error) -> Self {
        Self::Database(error)
    }
}

impl From<StorageError> for ApplyVerifiedReceivedResolutionV2Error {
    fn from(error: StorageError) -> Self {
        match error {
            StorageError::Database(error) => Self::Database(error),
            _ => Self::ProtectedStorageFailure,
        }
    }
}

#[derive(Debug)]
pub(crate) enum PrepareNoteConflictResolutionError {
    Database(rusqlite::Error),
    InvalidPayload,
    ScopeMismatch,
    StaleConflict,
    ConflictingResolution,
    MissingCausalProof,
    InjectedFailure,
    ProtectedStorageFailure,
}

impl From<rusqlite::Error> for PrepareNoteConflictResolutionError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Database(error)
    }
}

impl From<StorageError> for PrepareNoteConflictResolutionError {
    fn from(error: StorageError) -> Self {
        match error {
            StorageError::Database(error) => Self::Database(error),
            _ => Self::ProtectedStorageFailure,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct StoredEncryptedNoteSyncEnvelope {
    pub crypto_version: i64,
    pub aad_version: i64,
    pub nonce: String,
    pub ciphertext: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RecordNoteSyncSealFailureResult {
    Recorded,
    StaleGeneration,
    AlreadySealed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CommitSealedNoteSyncEventResult {
    Sealed,
    StaleGeneration,
    AlreadySealed,
}

#[derive(Debug, Eq, PartialEq)]
struct DecodedEncryptedNoteSyncEnvelope {
    crypto_version: i64,
    aad_version: i64,
    nonce: Vec<u8>,
    ciphertext: Vec<u8>,
}

impl NoteSyncOperation {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Upsert => "upsert",
            Self::Delete => "delete",
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ProjectCloudBinding {
    pub account_id: String,
    pub device_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct EntitySyncHead {
    pub event_id: String,
    pub revision: i64,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct PreparedNoteIntent {
    pub event_id: String,
    pub revision: i64,
    pub parent_event_id: Option<String>,
    pub local_ordinal: i64,
    pub mutation_generation: i64,
    pub coalesced: bool,
}

pub(crate) struct PrepareNoteIntent<'a> {
    pub project_id: &'a str,
    pub entity_id: &'a str,
    pub operation: NoteSyncOperation,
    pub updated_at: &'a str,
    pub deleted_at: Option<&'a str>,
    pub snapshot_json: &'a str,
    pub state_updated_at: &'a str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NoteDeleteScope<'a> {
    Project,
    Stage(&'a str),
}

pub(crate) fn build_note_tombstone(
    note: &serde_json::Value,
    deleted_at: &str,
) -> Result<String, NoteSyncError> {
    let source = note
        .as_object()
        .ok_or(NoteSyncError::InvalidSnapshot("snapshot is not an object"))?;
    let mut tombstone = serde_json::Map::new();
    for key in [
        "id",
        "project_id",
        "stage_id",
        "source_type",
        "source_map_id",
        "source_node_id",
        "content_format",
    ] {
        tombstone.insert(
            key.to_string(),
            source
                .get(key)
                .cloned()
                .ok_or(NoteSyncError::InvalidSnapshot("incomplete note route"))?,
        );
    }
    tombstone.insert(
        "deleted_at".to_string(),
        serde_json::Value::String(deleted_at.to_string()),
    );
    serde_json::to_string(&tombstone)
        .map_err(|_| NoteSyncError::InvalidSnapshot("tombstone cannot be encoded"))
}

pub(crate) fn prepare_note_delete_intent(
    transaction: &Transaction<'_>,
    project_id: &str,
    note_id: &str,
    note: &serde_json::Value,
    deleted_at: &str,
) -> Result<Option<PreparedNoteIntent>, NoteSyncError> {
    if resolve_project_cloud_binding(transaction, project_id)?.is_none() {
        return Ok(None);
    }
    let tombstone = build_note_tombstone(note, deleted_at)?;
    prepare_unsealed_note_intent(
        transaction,
        PrepareNoteIntent {
            project_id,
            entity_id: note_id,
            operation: NoteSyncOperation::Delete,
            updated_at: deleted_at,
            deleted_at: Some(deleted_at),
            snapshot_json: &tombstone,
            state_updated_at: deleted_at,
        },
    )
}

pub(crate) fn prepare_note_delete_intents_for_scope(
    transaction: &Transaction<'_>,
    project_id: &str,
    scope: NoteDeleteScope<'_>,
    deleted_at: &str,
) -> Result<usize, NoteSyncError> {
    // Preserve the pre-sync local-only deletion behavior, including for legacy
    // rows whose payload cannot form a cloud tombstone.
    if resolve_project_cloud_binding(transaction, project_id)?.is_none() {
        return Ok(0);
    }
    let mut notes = Vec::new();
    match scope {
        NoteDeleteScope::Project => {
            let mut statement = transaction
                .prepare("SELECT id,payload_json FROM notes WHERE project_id=?1 ORDER BY rowid")?;
            let rows = statement.query_map([project_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            notes.extend(rows.collect::<Result<Vec<_>, _>>()?);
        }
        NoteDeleteScope::Stage(stage_id) => {
            let mut statement = transaction.prepare(
                "SELECT id,payload_json FROM notes
                 WHERE project_id=?1 AND stage_id=?2 ORDER BY rowid",
            )?;
            let rows = statement.query_map(rusqlite::params![project_id, stage_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            notes.extend(rows.collect::<Result<Vec<_>, _>>()?);
        }
    }
    for (note_id, raw) in &notes {
        let note: serde_json::Value = serde_json::from_str(raw)
            .map_err(|_| NoteSyncError::InvalidSnapshot("snapshot is not valid JSON"))?;
        prepare_note_delete_intent(transaction, project_id, note_id, &note, deleted_at)?;
    }
    Ok(notes.len())
}

pub(crate) fn resolve_project_cloud_binding(
    transaction: &Transaction<'_>,
    project_id: &str,
) -> Result<Option<ProjectCloudBinding>, NoteSyncError> {
    transaction
        .query_row(
            "SELECT binding.account_id,state.device_id
             FROM cloud_sync_project_bindings AS binding
             JOIN cloud_sync_state AS state ON state.account_id=binding.account_id
             WHERE binding.project_id=?1",
            [project_id],
            |row| {
                Ok(ProjectCloudBinding {
                    account_id: row.get(0)?,
                    device_id: row.get(1)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

fn parse_ascii_number(bytes: &[u8]) -> Option<i64> {
    bytes.iter().try_fold(0_i64, |value, byte| {
        byte.is_ascii_digit()
            .then(|| value * 10 + i64::from(byte - b'0'))
    })
}

fn leap_year(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        2 if leap_year(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

// Proleptic Gregorian civil date to days relative to 1970-01-01. This mirrors
// the frozen TypeScript syncTimestamp contract without adding a date dependency.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let adjusted_year = year - i64::from(month <= 2);
    let era = adjusted_year.div_euclid(400);
    let year_of_era = adjusted_year - era * 400;
    let shifted_month = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn parse_note_sync_timestamp(value: &str) -> Option<(i64, i64)> {
    let bytes = value.as_bytes();
    if !value.is_ascii()
        || bytes.len() < 20
        || bytes.get(4) != Some(&b'-')
        || bytes.get(7) != Some(&b'-')
        || bytes.get(10) != Some(&b'T')
        || bytes.get(13) != Some(&b':')
        || bytes.get(16) != Some(&b':')
    {
        return None;
    }
    let Some(year) = parse_ascii_number(&bytes[0..4]) else {
        return None;
    };
    let Some(month) = parse_ascii_number(&bytes[5..7]) else {
        return None;
    };
    let Some(day) = parse_ascii_number(&bytes[8..10]) else {
        return None;
    };
    let Some(hour) = parse_ascii_number(&bytes[11..13]) else {
        return None;
    };
    let Some(minute) = parse_ascii_number(&bytes[14..16]) else {
        return None;
    };
    let Some(second) = parse_ascii_number(&bytes[17..19]) else {
        return None;
    };
    if !(1..=9999).contains(&year)
        || !(1..=12).contains(&month)
        || !(1..=days_in_month(year, month)).contains(&day)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }

    let mut cursor = 19;
    let mut fractional_microseconds = 0_i64;
    if bytes.get(cursor) == Some(&b'.') {
        cursor += 1;
        let fraction_start = cursor;
        while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
            cursor += 1;
        }
        if !(1..=6).contains(&(cursor - fraction_start)) {
            return None;
        }
        let digits = cursor - fraction_start;
        fractional_microseconds =
            parse_ascii_number(&bytes[fraction_start..cursor])? * 10_i64.pow((6 - digits) as u32);
    }

    let offset_seconds = if bytes.get(cursor) == Some(&b'Z') && cursor + 1 == bytes.len() {
        0
    } else if cursor + 6 == bytes.len()
        && matches!(bytes.get(cursor), Some(b'+') | Some(b'-'))
        && bytes.get(cursor + 3) == Some(&b':')
    {
        let Some(offset_hour) = parse_ascii_number(&bytes[cursor + 1..cursor + 3]) else {
            return None;
        };
        let Some(offset_minute) = parse_ascii_number(&bytes[cursor + 4..cursor + 6]) else {
            return None;
        };
        if offset_hour > 23
            || offset_minute > 59
            || (bytes[cursor] == b'-' && offset_hour == 0 && offset_minute == 0)
        {
            return None;
        }
        let seconds = (offset_hour * 60 + offset_minute) * 60;
        if bytes[cursor] == b'+' {
            seconds
        } else {
            -seconds
        }
    } else {
        return None;
    };

    let utc_seconds =
        days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second
            - offset_seconds;
    let minimum = days_from_civil(1, 1, 1) * 86_400;
    let maximum = days_from_civil(10_000, 1, 1) * 86_400;
    (minimum..maximum)
        .contains(&utc_seconds)
        .then_some((utc_seconds, fractional_microseconds))
}

fn valid_note_sync_timestamp(value: &str) -> bool {
    parse_note_sync_timestamp(value).is_some()
}

pub(crate) fn note_sync_timestamps_equal(left: &str, right: &str) -> bool {
    parse_note_sync_timestamp(left)
        .is_some_and(|left| parse_note_sync_timestamp(right).is_some_and(|right| right == left))
}

fn note_snapshot_semantically_matches_visible(snapshot: &str, visible: &str, source: &str) -> bool {
    const FIELDS: [&str; 19] = [
        "id",
        "project_id",
        "stage_id",
        "source_type",
        "source_map_id",
        "source_node_id",
        "content_format",
        "title",
        "content",
        "checklist",
        "color",
        "pinned",
        "archived",
        "sort_order",
        "tags",
        "created_at",
        "updated_at",
        "revision",
        "metadata",
    ];
    let (Ok(serde_json::Value::Object(mut snapshot)), Ok(serde_json::Value::Object(mut visible))) = (
        serde_json::from_str::<serde_json::Value>(snapshot),
        serde_json::from_str::<serde_json::Value>(visible),
    ) else {
        return false;
    };
    if snapshot.len() != FIELDS.len()
        || visible.len() != FIELDS.len()
        || FIELDS
            .iter()
            .any(|field| !snapshot.contains_key(*field) || !visible.contains_key(*field))
    {
        return false;
    }
    let Some(snapshot_revision) = snapshot.remove("revision").and_then(|value| value.as_i64())
    else {
        return false;
    };
    let Some(visible_revision) = visible.remove("revision").and_then(|value| value.as_i64()) else {
        return false;
    };
    if !(0..=MAX_SYNC_INTEGER).contains(&snapshot_revision)
        || !(0..=MAX_SYNC_INTEGER).contains(&visible_revision)
        || match source {
            "remote_applied" => snapshot_revision != 0,
            "local_unsealed" => snapshot_revision != visible_revision,
            _ => true,
        }
    {
        return false;
    }
    let Ok(record) = serde_json::from_value::<crate::note_sync_plaintext::NoteSyncRecord>(
        serde_json::Value::Object(snapshot.clone()),
    ) else {
        return false;
    };
    if record.route.id.is_empty()
        || record.route.project_id.is_empty()
        || !matches!(record.route.source_type.as_str(), "project" | "mindmap")
        || !matches!(record.route.content_format.as_str(), "html" | "plain")
        || record.checklist.iter().any(|item| item.id.is_empty())
    {
        return false;
    }
    for field in ["created_at", "updated_at"] {
        let (Some(stored), Some(current)) = (snapshot.remove(field), visible.remove(field)) else {
            return false;
        };
        let (Some(stored), Some(current)) = (stored.as_str(), current.as_str()) else {
            return false;
        };
        if !note_sync_timestamps_equal(stored, current) {
            return false;
        }
    }
    snapshot == visible
}

pub(crate) fn preflight_note_sync_project(
    connection: &Connection,
    project_id: &str,
) -> Result<Vec<NoteSyncPreflightIssue>, NoteSyncError> {
    if project_id.is_empty() {
        return Err(NoteSyncError::InvalidSnapshot("missing project identity"));
    }
    let mut statement =
        connection.prepare("SELECT id,payload_json FROM notes WHERE project_id=?1 ORDER BY id")?;
    let rows = statement.query_map([project_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut issues = Vec::new();
    for row in rows {
        let (note_id, snapshot_json) = row?;
        let Ok(snapshot) = serde_json::from_str::<serde_json::Value>(&snapshot_json) else {
            issues.push(NoteSyncPreflightIssue {
                note_id,
                code: NoteSyncPreflightIssueCode::InvalidNotePayload,
            });
            continue;
        };
        let Some(object) = snapshot.as_object() else {
            issues.push(NoteSyncPreflightIssue {
                note_id,
                code: NoteSyncPreflightIssueCode::InvalidNotePayload,
            });
            continue;
        };
        for (field, missing, invalid) in [
            (
                "created_at",
                NoteSyncPreflightIssueCode::MissingCreatedAt,
                NoteSyncPreflightIssueCode::InvalidCreatedAt,
            ),
            (
                "updated_at",
                NoteSyncPreflightIssueCode::MissingUpdatedAt,
                NoteSyncPreflightIssueCode::InvalidUpdatedAt,
            ),
        ] {
            let code = match object.get(field) {
                None => Some(missing),
                Some(serde_json::Value::String(value)) if value.is_empty() => Some(missing),
                Some(serde_json::Value::String(value)) if valid_note_sync_timestamp(value) => None,
                _ => Some(invalid),
            };
            if let Some(code) = code {
                issues.push(NoteSyncPreflightIssue {
                    note_id: note_id.clone(),
                    code,
                });
            }
        }
        if object.get("stage_id").is_some_and(|value| !value.is_null())
            || object
                .get("source_type")
                .and_then(serde_json::Value::as_str)
                != Some("project")
            || object
                .get("source_map_id")
                .is_some_and(|value| !value.is_null())
            || object
                .get("source_node_id")
                .is_some_and(|value| !value.is_null())
        {
            issues.push(NoteSyncPreflightIssue {
                note_id: note_id.clone(),
                code: NoteSyncPreflightIssueCode::DependencyNotSynced,
            });
        } else if object
            .get("content_format")
            .and_then(serde_json::Value::as_str)
            != Some("html")
        {
            issues.push(NoteSyncPreflightIssue {
                note_id: note_id.clone(),
                code: NoteSyncPreflightIssueCode::UnsupportedContentFormat,
            });
        }
    }
    Ok(issues)
}

fn generate_bootstrap_id() -> Result<String, NoteSyncError> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|error| NoteSyncError::Random(error.to_string()))?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Ok(format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
    ))
}

fn valid_bootstrap_id(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte),
        })
        && value.as_bytes()[14] == b'4'
        && matches!(value.as_bytes()[19], b'8' | b'9' | b'a' | b'b')
}

fn read_bootstrap_record(
    transaction: &Transaction<'_>,
    project_id: &str,
) -> Result<Option<CloudProjectBootstrapRecord>, NoteSyncError> {
    transaction
        .query_row(
            "SELECT project_id,account_id,device_id,bootstrap_id,mode,phase,remote_state,
                initial_event_count,initial_local_ordinal_hi,remote_high_water,
                initial_max_server_sequence,blocked_reason
         FROM cloud_sync_project_bootstraps WHERE project_id=?1",
            [project_id],
            |row| {
                Ok(CloudProjectBootstrapRecord {
                    project_id: row.get(0)?,
                    account_id: row.get(1)?,
                    device_id: row.get(2)?,
                    bootstrap_id: row.get(3)?,
                    mode: row.get(4)?,
                    phase: row.get(5)?,
                    remote_state: row.get(6)?,
                    initial_event_count: row.get(7)?,
                    initial_local_ordinal_hi: row.get(8)?,
                    remote_high_water: row.get(9)?,
                    initial_max_server_sequence: row.get(10)?,
                    blocked_reason: row.get(11)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

fn validate_bootstrap_scope(
    transaction: &Transaction<'_>,
    project_id: &str,
    account_id: &str,
    device_id: &str,
) -> Result<(), NoteSyncError> {
    if project_id.is_empty() || account_id.is_empty() || device_id.len() != 36 {
        return Err(NoteSyncError::InvalidSealState("invalid bootstrap scope"));
    }
    let stored_device: Option<String> = transaction
        .query_row(
            "SELECT device_id FROM cloud_sync_state WHERE account_id=?1",
            [account_id],
            |row| row.get(0),
        )
        .optional()?;
    if stored_device.as_deref() != Some(device_id) {
        return Err(NoteSyncError::InvalidSealState(
            "bootstrap account or device mismatch",
        ));
    }
    Ok(())
}

pub(crate) fn prepare_cloud_project_bootstrap(
    connection: &mut Connection,
    command: &PrepareCloudProjectBootstrapCommand,
) -> Result<CloudProjectBootstrapRecord, NoteSyncError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    validate_bootstrap_scope(
        &transaction,
        &command.project_id,
        &command.account_id,
        &command.device_id,
    )?;
    if transaction
        .query_row(
            "SELECT 1 FROM projects WHERE id=?1",
            [&command.project_id],
            |_| Ok(()),
        )
        .optional()?
        .is_none()
    {
        return Err(NoteSyncError::InvalidSealState(
            "bootstrap project is missing",
        ));
    }
    if let Some(existing) = read_bootstrap_record(&transaction, &command.project_id)? {
        if existing.account_id != command.account_id
            || existing.device_id != command.device_id
            || existing.mode != command.mode.as_str()
        {
            return Err(NoteSyncError::InvalidSealState(
                "conflicting local bootstrap lineage",
            ));
        }
        transaction.commit()?;
        return Ok(existing);
    }
    if !preflight_note_sync_project(&transaction, &command.project_id)?.is_empty() {
        return Err(NoteSyncError::InvalidSealState(
            "project contains unsupported notes",
        ));
    }
    let bootstrap_id = generate_bootstrap_id()?;
    transaction.execute(
        "INSERT INTO cloud_sync_project_bootstraps(
            project_id,account_id,device_id,bootstrap_id,mode,phase,remote_state,
            initial_event_count,initial_local_ordinal_hi,remote_high_water,
            initial_max_server_sequence,blocked_reason,created_at,updated_at
         ) VALUES(?1,?2,?3,?4,?5,'prepared',NULL,0,0,NULL,NULL,NULL,
                  strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        rusqlite::params![
            command.project_id,
            command.account_id,
            command.device_id,
            bootstrap_id,
            command.mode.as_str()
        ],
    )?;
    let record = read_bootstrap_record(&transaction, &command.project_id)?.ok_or(
        NoteSyncError::InvalidSealState("bootstrap preparation was not persisted"),
    )?;
    transaction.commit()?;
    Ok(record)
}

pub(crate) fn confirm_cloud_project_registration(
    connection: &mut Connection,
    command: &ConfirmCloudProjectRegistrationCommand,
) -> Result<CloudProjectBootstrapRecord, NoteSyncError> {
    if !valid_bootstrap_id(&command.bootstrap_id)
        || !matches!(command.remote_state.as_str(), "initializing" | "active")
        || !(0..=MAX_SYNC_INTEGER).contains(&command.remote_high_water)
    {
        return Err(NoteSyncError::InvalidSealState(
            "invalid remote bootstrap registration",
        ));
    }
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    validate_bootstrap_scope(
        &transaction,
        &command.project_id,
        &command.account_id,
        &command.device_id,
    )?;
    let current = read_bootstrap_record(&transaction, &command.project_id)?.ok_or(
        NoteSyncError::InvalidSealState("local bootstrap is missing"),
    )?;
    if current.account_id != command.account_id
        || current.device_id != command.device_id
        || current.bootstrap_id != command.bootstrap_id
    {
        return Err(NoteSyncError::InvalidSealState(
            "conflicting local/server bootstrap lineage",
        ));
    }
    if current
        .remote_state
        .as_deref()
        .is_some_and(|state| state == "active" && command.remote_state != "active")
    {
        return Err(NoteSyncError::InvalidSealState(
            "remote bootstrap state regressed",
        ));
    }
    let phase = if current.phase == "prepared" {
        "registered"
    } else {
        current.phase.as_str()
    };
    transaction.execute(
        "UPDATE cloud_sync_project_bootstraps SET phase=?1,remote_state=?2,remote_high_water=MAX(COALESCE(remote_high_water,0),?3),updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE project_id=?4",
        rusqlite::params![phase, command.remote_state, command.remote_high_water, command.project_id],
    )?;
    let record = read_bootstrap_record(&transaction, &command.project_id)?.unwrap();
    transaction.commit()?;
    Ok(record)
}

pub(crate) fn capture_initial_note_sync_intents(
    connection: &mut Connection,
    command: &CloudProjectBootstrapScopeCommand,
) -> Result<CloudProjectBootstrapRecord, NoteSyncError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    validate_bootstrap_scope(
        &transaction,
        &command.project_id,
        &command.account_id,
        &command.device_id,
    )?;
    let current = read_bootstrap_record(&transaction, &command.project_id)?.ok_or(
        NoteSyncError::InvalidSealState("local bootstrap is missing"),
    )?;
    if current.account_id != command.account_id
        || current.device_id != command.device_id
        || current.bootstrap_id != command.bootstrap_id
        || current.mode != "upload_existing"
    {
        return Err(NoteSyncError::InvalidSealState(
            "conflicting initial capture scope",
        ));
    }
    if matches!(
        current.phase.as_str(),
        "captured" | "completing" | "ready" | "paused"
    ) {
        transaction.commit()?;
        return Ok(current);
    }
    if current.phase != "registered" || current.remote_state.as_deref() != Some("initializing") {
        return Err(NoteSyncError::InvalidSealState(
            "server registration is not initializing",
        ));
    }
    if !preflight_note_sync_project(&transaction, &command.project_id)?.is_empty() {
        return Err(NoteSyncError::InvalidSealState(
            "project contains unsupported notes",
        ));
    }
    if transaction
        .query_row(
            "SELECT 1 FROM cloud_sync_project_bindings WHERE project_id=?1",
            [&command.project_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some()
    {
        return Err(NoteSyncError::InvalidSealState(
            "unexpected existing project binding",
        ));
    }
    transaction.execute(
        "INSERT INTO cloud_sync_project_bindings(project_id,account_id,created_at,updated_at) VALUES(?1,?2,strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        rusqlite::params![command.project_id, command.account_id],
    )?;
    let notes = {
        let mut statement = transaction.prepare(
            "SELECT id,updated_at,payload_json FROM notes WHERE project_id=?1 ORDER BY id",
        )?;
        let rows = statement
            .query_map([&command.project_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    };
    let mut high_water = 0_i64;
    for (note_id, updated_at, snapshot) in &notes {
        let prepared = prepare_unsealed_note_intent(
            &transaction,
            PrepareNoteIntent {
                project_id: &command.project_id,
                entity_id: note_id,
                operation: NoteSyncOperation::Upsert,
                updated_at,
                deleted_at: None,
                snapshot_json: snapshot,
                state_updated_at: updated_at,
            },
        )?
        .ok_or(NoteSyncError::InvalidSealState(
            "initial Note intent was not created",
        ))?;
        high_water = high_water.max(prepared.local_ordinal);
    }
    transaction.execute(
        "UPDATE cloud_sync_project_bootstraps SET phase='captured',initial_event_count=?1,initial_local_ordinal_hi=?2,updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE project_id=?3",
        rusqlite::params![notes.len() as i64, high_water, command.project_id],
    )?;
    let record = read_bootstrap_record(&transaction, &command.project_id)?.unwrap();
    transaction.commit()?;
    Ok(record)
}

pub(crate) fn read_initial_note_cohort_status(
    connection: &mut Connection,
    command: &CloudProjectBootstrapScopeCommand,
) -> Result<CloudProjectInitialCohortStatus, NoteSyncError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
    validate_bootstrap_scope(
        &transaction,
        &command.project_id,
        &command.account_id,
        &command.device_id,
    )?;
    let current = read_bootstrap_record(&transaction, &command.project_id)?.ok_or(
        NoteSyncError::InvalidSealState("local bootstrap is missing"),
    )?;
    if current.bootstrap_id != command.bootstrap_id
        || current.account_id != command.account_id
        || current.device_id != command.device_id
    {
        return Err(NoteSyncError::InvalidSealState(
            "conflicting initial cohort scope",
        ));
    }
    let (event_count, accepted_count, max_sequence): (i64, i64, i64) = transaction.query_row(
        "SELECT count(*),
                sum(CASE WHEN event.lifecycle='accepted' AND receipt.event_id IS NOT NULL THEN 1 ELSE 0 END),
                COALESCE(max(receipt.server_sequence),0)
         FROM cloud_sync_outbox AS event
         LEFT JOIN cloud_sync_upload_receipts AS receipt ON receipt.event_id=event.event_id
         WHERE event.account_id=?1 AND event.device_id=?2 AND event.project_id=?3
           AND event.entity_type='note' AND event.local_ordinal>0
           AND event.local_ordinal<=?4",
        rusqlite::params![command.account_id, command.device_id, command.project_id,
                          current.initial_local_ordinal_hi],
        |row| Ok((row.get(0)?, row.get::<_, Option<i64>>(1)?.unwrap_or(0), row.get(2)?)),
    )?;
    let status = CloudProjectInitialCohortStatus {
        event_count,
        accepted_count,
        max_server_sequence: max_sequence,
        complete: event_count == current.initial_event_count && accepted_count == event_count,
    };
    transaction.commit()?;
    Ok(status)
}

pub(crate) fn mark_cloud_project_bootstrap_completing(
    connection: &mut Connection,
    command: &CloudProjectBootstrapScopeCommand,
) -> Result<CloudProjectBootstrapRecord, NoteSyncError> {
    let cohort = read_initial_note_cohort_status(connection, command)?;
    if !cohort.complete {
        return Err(NoteSyncError::InvalidSealState(
            "initial Note cohort is not accepted",
        ));
    }
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let current = read_bootstrap_record(&transaction, &command.project_id)?.ok_or(
        NoteSyncError::InvalidSealState("local bootstrap is missing"),
    )?;
    if current.bootstrap_id != command.bootstrap_id
        || current.account_id != command.account_id
        || current.device_id != command.device_id
    {
        return Err(NoteSyncError::InvalidSealState(
            "conflicting completion scope",
        ));
    }
    transaction.execute(
        "UPDATE cloud_sync_project_bootstraps SET phase='completing',initial_max_server_sequence=?1,updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE project_id=?2",
        rusqlite::params![cohort.max_server_sequence, command.project_id],
    )?;
    let record = read_bootstrap_record(&transaction, &command.project_id)?.unwrap();
    transaction.commit()?;
    Ok(record)
}

pub(crate) fn mark_cloud_project_bootstrap_ready(
    connection: &mut Connection,
    command: &CloudProjectBootstrapScopeCommand,
) -> Result<CloudProjectBootstrapRecord, NoteSyncError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    validate_bootstrap_scope(
        &transaction,
        &command.project_id,
        &command.account_id,
        &command.device_id,
    )?;
    let current = read_bootstrap_record(&transaction, &command.project_id)?.ok_or(
        NoteSyncError::InvalidSealState("local bootstrap is missing"),
    )?;
    if current.bootstrap_id != command.bootstrap_id
        || current.remote_state.as_deref() != Some("active")
    {
        return Err(NoteSyncError::InvalidSealState(
            "active remote bootstrap is not confirmed",
        ));
    }
    let required_cursor = current
        .remote_high_water
        .unwrap_or(0)
        .max(current.initial_max_server_sequence.unwrap_or(0));
    let ack_cursor: i64 = transaction.query_row(
        "SELECT ack_cursor FROM cloud_sync_state WHERE account_id=?1",
        [&command.account_id],
        |row| row.get(0),
    )?;
    if ack_cursor < required_cursor {
        return Err(NoteSyncError::InvalidSealState(
            "required remote events are not durably acknowledged",
        ));
    }
    transaction.execute(
        "UPDATE cloud_sync_project_bootstraps SET phase='ready',updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE project_id=?1",
        [&command.project_id],
    )?;
    let record = read_bootstrap_record(&transaction, &command.project_id)?.unwrap();
    transaction.commit()?;
    Ok(record)
}

pub(crate) fn set_cloud_project_bootstrap_paused(
    connection: &mut Connection,
    command: &CloudProjectBootstrapScopeCommand,
    paused: bool,
) -> Result<CloudProjectBootstrapRecord, NoteSyncError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    validate_bootstrap_scope(
        &transaction,
        &command.project_id,
        &command.account_id,
        &command.device_id,
    )?;
    let current = read_bootstrap_record(&transaction, &command.project_id)?.ok_or(
        NoteSyncError::InvalidSealState("local bootstrap is missing"),
    )?;
    if current.bootstrap_id != command.bootstrap_id
        || (paused && current.phase != "ready")
        || (!paused && current.phase != "paused")
    {
        return Err(NoteSyncError::InvalidSealState(
            "invalid bootstrap pause transition",
        ));
    }
    let phase = if paused { "paused" } else { "ready" };
    transaction.execute(
        "UPDATE cloud_sync_project_bootstraps SET phase=?1,updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE project_id=?2",
        rusqlite::params![phase, command.project_id],
    )?;
    let record = read_bootstrap_record(&transaction, &command.project_id)?.unwrap();
    transaction.commit()?;
    Ok(record)
}

pub(crate) fn list_cloud_project_bootstraps(
    connection: &mut Connection,
    account_id: &str,
) -> Result<Vec<CloudProjectBootstrapRecord>, NoteSyncError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
    let mut statement = transaction.prepare(
        "SELECT project_id,account_id,device_id,bootstrap_id,mode,phase,remote_state,
                initial_event_count,initial_local_ordinal_hi,remote_high_water,
                initial_max_server_sequence,blocked_reason
         FROM cloud_sync_project_bootstraps WHERE account_id=?1 ORDER BY project_id",
    )?;
    let records = statement
        .query_map([account_id], |row| {
            Ok(CloudProjectBootstrapRecord {
                project_id: row.get(0)?,
                account_id: row.get(1)?,
                device_id: row.get(2)?,
                bootstrap_id: row.get(3)?,
                mode: row.get(4)?,
                phase: row.get(5)?,
                remote_state: row.get(6)?,
                initial_event_count: row.get(7)?,
                initial_local_ordinal_hi: row.get(8)?,
                remote_high_water: row.get(9)?,
                initial_max_server_sequence: row.get(10)?,
                blocked_reason: row.get(11)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    transaction.commit()?;
    Ok(records)
}

pub(crate) fn import_remote_cloud_project(
    connection: &mut Connection,
    command: &ImportRemoteCloudProjectCommand,
) -> Result<CloudProjectBootstrapRecord, NoteSyncError> {
    if !valid_bootstrap_id(&command.bootstrap_id)
        || !(0..=MAX_SYNC_INTEGER).contains(&command.remote_high_water)
        || command.display_name.trim().is_empty()
    {
        return Err(NoteSyncError::InvalidSealState(
            "invalid remote project import",
        ));
    }
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    validate_bootstrap_scope(
        &transaction,
        &command.project_id,
        &command.account_id,
        &command.device_id,
    )?;
    if let Some(existing) = read_bootstrap_record(&transaction, &command.project_id)? {
        if existing.account_id == command.account_id
            && existing.bootstrap_id == command.bootstrap_id
            && existing.mode == "import_remote"
        {
            transaction.commit()?;
            return Ok(existing);
        }
        return Err(NoteSyncError::InvalidSealState(
            "local project has conflicting bootstrap lineage",
        ));
    }
    if transaction
        .query_row(
            "SELECT 1 FROM projects WHERE id=?1",
            [&command.project_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some()
    {
        return Err(NoteSyncError::InvalidSealState(
            "local project ID collision",
        ));
    }
    if transaction
        .query_row(
            "SELECT 1 FROM projects WHERE name=?1",
            [command.display_name.trim()],
            |_| Ok(()),
        )
        .optional()?
        .is_some()
    {
        return Err(NoteSyncError::InvalidSealState(
            "local project name collision",
        ));
    }
    let now: String =
        transaction.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')", [], |row| {
            row.get(0)
        })?;
    let name = command.display_name.trim();
    let payload = serde_json::json!({
        "id": command.project_id, "name": name, "goal": null, "infinite": true,
        "total": 0, "progress": 0, "deadline": null, "status": "активен",
        "unit": "symbols", "created_at": now, "updated_at": now,
        "notes_updated_at": null, "mindmap_updated_at": null, "completed_at": null,
        "personal_goal": 0, "today_goal": null, "planning_date": null,
        "plan_daily_goal": null, "added_today": 0, "remaining": null,
        "streak_enabled": true, "streak_status": "No", "streak_length": 0,
        "max_streak": 0, "auto_freeze": true, "progress_entries": [],
        "project_notes": [], "mindmap": null, "stages": [], "stages_enabled": false,
        "combine_stage_mindmaps": false, "cover_image": null, "folder_id": null,
        "sync_available": false, "work_method": "manual", "parent_project_id": null
    });
    transaction.execute(
        "INSERT INTO projects(id,name,goal,infinite,unit,status,created_at,updated_at,payload_json) VALUES(?1,?2,NULL,1,'symbols','активен',?3,?3,?4)",
        rusqlite::params![command.project_id, name, now, payload.to_string()],
    )?;
    transaction.execute(
        "INSERT INTO cloud_sync_project_bindings(project_id,account_id,created_at,updated_at) VALUES(?1,?2,?3,?3)",
        rusqlite::params![command.project_id, command.account_id, now],
    )?;
    if let Some(metadata) = command.authenticated_metadata.as_ref() {
        crate::project_metadata_sync::write_visible_metadata(
            &transaction,
            &command.project_id,
            metadata,
            &now,
        )
        .map_err(|_| NoteSyncError::InvalidSealState("invalid authenticated import metadata"))?;
        // The writer preserves locally initialized bindings and extension fields.
    }
    let position: i64 = transaction.query_row(
        "SELECT COALESCE(MAX(position),-1)+1 FROM project_order",
        [],
        |row| row.get(0),
    )?;
    transaction.execute(
        "INSERT INTO project_order(project_id,position) VALUES(?1,?2)",
        rusqlite::params![command.project_id, position],
    )?;
    transaction.execute(
        "INSERT INTO cloud_sync_project_bootstraps(
            project_id,account_id,device_id,bootstrap_id,mode,phase,remote_state,
            initial_event_count,initial_local_ordinal_hi,remote_high_water,
            initial_max_server_sequence,blocked_reason,created_at,updated_at
         ) VALUES(?1,?2,?3,?4,'import_remote','captured','active',0,0,?5,NULL,NULL,?6,?6)",
        rusqlite::params![
            command.project_id,
            command.account_id,
            command.device_id,
            command.bootstrap_id,
            command.remote_high_water,
            now
        ],
    )?;
    let record = read_bootstrap_record(&transaction, &command.project_id)?.unwrap();
    transaction.commit()?;
    Ok(record)
}

pub(crate) fn next_local_ordinal(
    transaction: &Transaction<'_>,
    account_id: &str,
    device_id: &str,
) -> Result<i64, NoteSyncError> {
    let current: i64 = transaction.query_row(
        "SELECT COALESCE(MAX(local_ordinal),0)
         FROM cloud_sync_outbox
         WHERE account_id=?1 AND device_id=?2 AND lifecycle!='legacy'",
        rusqlite::params![account_id, device_id],
        |row| row.get(0),
    )?;
    current
        .checked_add(1)
        .filter(|value| (1..=MAX_SYNC_INTEGER).contains(value))
        .ok_or(NoteSyncError::LocalOrdinalOverflow)
}

pub(crate) fn read_entity_sync_head(
    transaction: &Transaction<'_>,
    account_id: &str,
    project_id: &str,
    entity_id: &str,
) -> Result<Option<EntitySyncHead>, NoteSyncError> {
    let local = transaction
        .query_row(
            "SELECT event_id,revision
             FROM cloud_sync_outbox
             WHERE account_id=?1 AND project_id=?2 AND entity_id=?3
               AND entity_type='note' AND lifecycle IN ('sealed','accepted')
               AND local_ordinal>0
             ORDER BY revision DESC,local_ordinal DESC,event_id DESC LIMIT 1",
            rusqlite::params![account_id, project_id, entity_id],
            |row| {
                Ok(EntitySyncHead {
                    event_id: row.get(0)?,
                    revision: row.get(1)?,
                })
            },
        )
        .optional()?;
    let applied = transaction
        .query_row(
            "SELECT head_event_id,head_sync_revision
             FROM cloud_sync_entities
             WHERE account_id=?1 AND project_id=?2 AND entity_id=?3 AND entity_type='note'",
            rusqlite::params![account_id, project_id, entity_id],
            |row| {
                Ok(EntitySyncHead {
                    event_id: row.get(0)?,
                    revision: row.get(1)?,
                })
            },
        )
        .optional()?;
    match (local, applied) {
        (None, None) => Ok(None),
        (Some(head), None) | (None, Some(head)) => Ok(Some(head)),
        (Some(local), Some(applied)) if local.revision > applied.revision => Ok(Some(local)),
        (Some(local), Some(applied)) if applied.revision > local.revision => Ok(Some(applied)),
        (Some(local), Some(applied)) if local.event_id == applied.event_id => Ok(Some(local)),
        (Some(_), Some(_)) => Err(NoteSyncError::ConflictingHeads),
    }
}

// A resolution is one causal node for either its original Note or its exact
// keep_both clone. Its direct parents remain in the immutable v2 ledger.
pub(crate) fn applied_resolution_parent_revision(
    transaction: &Transaction<'_>,
    account_id: &str,
    project_id: &str,
    entity_id: &str,
    event_id: &str,
) -> Result<Option<i64>, NoteSyncError> {
    transaction.query_row(
        "SELECT resolution.revision
         FROM cloud_sync_note_applied_resolutions AS resolution
         JOIN cloud_sync_inbox AS inbox
           ON inbox.account_id=resolution.account_id
          AND inbox.event_id=resolution.resolution_event_id
         JOIN cloud_sync_note_conflict_groups AS conflict_group
           ON conflict_group.group_id=resolution.local_conflict_group_id
         WHERE resolution.account_id=?1 AND resolution.resolution_event_id=?2
           AND resolution.project_id=?3
           AND (resolution.entity_id=?4 OR
                (resolution.strategy='keep_both' AND resolution.clone_entity_id=?4))
           AND resolution.lifecycle='applied'
           AND resolution.revision BETWEEN 2 AND 9007199254740991
           AND (inbox.operation='resolution' OR (inbox.operation='event' AND EXISTS(SELECT 1 FROM cloud_content_note_receipts r WHERE r.account_id=inbox.account_id AND r.event_id=inbox.event_id AND r.outcome='applied'))) AND inbox.state='applied'
           AND inbox.project_id=resolution.project_id
           AND inbox.entity_id=resolution.entity_id
           AND inbox.device_id=resolution.source_device_id
           AND inbox.server_sequence=resolution.server_sequence
           AND inbox.sync_revision=resolution.revision
           AND inbox.updated_at=resolution.event_updated_at
           AND inbox.applied_at=resolution.applied_at
           AND conflict_group.account_id=resolution.account_id
           AND conflict_group.project_id=resolution.project_id
           AND conflict_group.entity_id=resolution.entity_id
           AND conflict_group.lifecycle='resolved'
           AND conflict_group.generation=resolution.local_conflict_generation
           AND resolution.parent_count BETWEEN 2 AND 64
           AND resolution.parent_count=json_array_length(resolution.parent_event_ids_json)
           AND resolution.parent_count=(
               SELECT count(*) FROM cloud_sync_note_applied_resolution_parents AS parent
               WHERE parent.account_id=resolution.account_id
                 AND parent.resolution_event_id=resolution.resolution_event_id)
           AND resolution.parent_count=(
               SELECT count(*) FROM cloud_sync_note_conflict_tips AS tip
               WHERE tip.group_id=resolution.local_conflict_group_id)
           AND resolution.revision=(
               SELECT max(parent.parent_revision)+1
               FROM cloud_sync_note_applied_resolution_parents AS parent
               WHERE parent.account_id=resolution.account_id
                 AND parent.resolution_event_id=resolution.resolution_event_id)
           AND NOT EXISTS(
               SELECT 1 FROM json_each(resolution.parent_event_ids_json) AS expected
               LEFT JOIN cloud_sync_note_applied_resolution_parents AS parent
                 ON parent.account_id=resolution.account_id
                AND parent.resolution_event_id=resolution.resolution_event_id
                AND parent.parent_ordinal=CAST(expected.key AS INTEGER)
                AND parent.parent_event_id=expected.value
               LEFT JOIN cloud_sync_note_conflict_versions AS version
                 ON version.version_id=parent.conflict_version_id
                AND version.group_id=resolution.local_conflict_group_id
                AND version.account_id=resolution.account_id
                AND version.project_id=resolution.project_id
                AND version.entity_id=resolution.entity_id
                AND version.event_id=parent.parent_event_id
                AND version.revision=parent.parent_revision
                AND version.operation=parent.parent_operation
                AND version.snapshot_json=parent.parent_snapshot_json
                AND version.parent_event_id=conflict_group.common_parent_event_id
               LEFT JOIN cloud_sync_note_conflict_tips AS tip
                 ON tip.group_id=resolution.local_conflict_group_id
                AND tip.version_id=parent.conflict_version_id
                AND tip.event_id=parent.parent_event_id
               WHERE parent.parent_event_id IS NULL OR version.version_id IS NULL
                  OR tip.event_id IS NULL)",
        rusqlite::params![account_id, event_id, project_id, entity_id],
        |row| row.get(0),
    ).optional().map_err(Into::into)
}

fn is_resolution_event(
    transaction: &Transaction<'_>,
    event_id: &str,
) -> Result<bool, NoteSyncError> {
    Ok(transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM cloud_sync_inbox
                       WHERE event_id=?1 AND operation='resolution'
                       UNION ALL
                       SELECT 1 FROM cloud_sync_note_applied_resolutions
                       WHERE resolution_event_id=?1)",
        [event_id],
        |row| row.get::<_, i64>(0),
    )? != 0)
}

fn has_unreconciled_local_resolution(
    transaction: &Transaction<'_>,
    account_id: &str,
    project_id: &str,
    entity_id: &str,
) -> Result<bool, NoteSyncError> {
    let prepared: i64 = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM cloud_sync_note_pending_resolutions
         WHERE account_id=?1 AND project_id=?2 AND lifecycle='prepared'
           AND (entity_id=?3 OR (strategy='keep_both' AND
                json_extract(CAST(canonical_payload AS TEXT),
                    '$.resolution.retained_note.id')=?3)))",
        rusqlite::params![account_id, project_id, entity_id],
        |row| row.get(0),
    )?;
    if prepared != 0 {
        return Ok(true);
    }
    let mut statement = transaction.prepare(
        "SELECT resolution_event_id,revision
         FROM cloud_sync_note_resolution_outbox
         WHERE account_id=?1 AND project_id=?2
           AND (entity_id=?3 OR clone_entity_id=?3)",
    )?;
    let rows = statement.query_map(
        rusqlite::params![account_id, project_id, entity_id],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
    )?;
    for row in rows {
        let (event_id, revision) = row?;
        if applied_resolution_parent_revision(
            transaction,
            account_id,
            project_id,
            entity_id,
            &event_id,
        )? != Some(revision)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn validate_snapshot(input: &PrepareNoteIntent<'_>) -> Result<(), NoteSyncError> {
    if input.project_id.is_empty() || input.entity_id.is_empty() || input.updated_at.is_empty() {
        return Err(NoteSyncError::InvalidSnapshot("missing event identity"));
    }
    if (input.operation == NoteSyncOperation::Delete) != input.deleted_at.is_some() {
        return Err(NoteSyncError::InvalidSnapshot(
            "operation/deleted_at mismatch",
        ));
    }
    let snapshot: serde_json::Value = serde_json::from_str(input.snapshot_json)
        .map_err(|_| NoteSyncError::InvalidSnapshot("snapshot is not valid JSON"))?;
    let object = snapshot
        .as_object()
        .ok_or(NoteSyncError::InvalidSnapshot("snapshot is not an object"))?;
    if object.get("id").and_then(serde_json::Value::as_str) != Some(input.entity_id)
        || object.get("project_id").and_then(serde_json::Value::as_str) != Some(input.project_id)
    {
        return Err(NoteSyncError::InvalidSnapshot("snapshot identity mismatch"));
    }
    if input.operation == NoteSyncOperation::Delete {
        for key in [
            "stage_id",
            "source_type",
            "source_map_id",
            "source_node_id",
            "content_format",
            "deleted_at",
        ] {
            if !object.contains_key(key) {
                return Err(NoteSyncError::InvalidSnapshot("incomplete tombstone"));
            }
        }
        if object.get("deleted_at").and_then(serde_json::Value::as_str) != input.deleted_at {
            return Err(NoteSyncError::InvalidSnapshot(
                "tombstone timestamp mismatch",
            ));
        }
    }
    Ok(())
}

fn canonical_uuid_v4() -> Result<String, NoteSyncError> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|error| NoteSyncError::Random(error.to_string()))?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let mut value = String::with_capacity(36);
    for (index, byte) in bytes.iter().enumerate() {
        if matches!(index, 4 | 6 | 8 | 10) {
            value.push('-');
        }
        write!(&mut value, "{byte:02x}").expect("writing to a String cannot fail");
    }
    Ok(value)
}

pub(crate) fn prepare_unsealed_note_intent(
    transaction: &Transaction<'_>,
    input: PrepareNoteIntent<'_>,
) -> Result<Option<PreparedNoteIntent>, NoteSyncError> {
    if let Some(binding) = resolve_project_cloud_binding(transaction, input.project_id)? {
        if has_unreconciled_local_resolution(
            transaction,
            &binding.account_id,
            input.project_id,
            input.entity_id,
        )? {
            return Err(NoteSyncError::InvalidSealState(
                "resolution is not durably applied",
            ));
        }
    }
    let value: serde_json::Value = serde_json::from_str(input.snapshot_json)
        .map_err(|_| NoteSyncError::InvalidSnapshot("invalid Note source"))?;
    if value["source_type"] != "project" {
        return Ok(None);
    }
    if value["stage_id"].is_null() && value["content_format"] == "html" {
        return prepare_legacy_note_intent(transaction, input);
    }
    crate::content_note_writer::normal(transaction, input)
}

pub(crate) fn prepare_legacy_note_intent(
    transaction: &Transaction<'_>,
    input: PrepareNoteIntent<'_>,
) -> Result<Option<PreparedNoteIntent>, NoteSyncError> {
    prepare_legacy_note_intent_with_id(transaction, input, None)
}
pub(crate) fn prepare_legacy_note_intent_with_id(
    transaction: &Transaction<'_>,
    input: PrepareNoteIntent<'_>,
    reserved: Option<&str>,
) -> Result<Option<PreparedNoteIntent>, NoteSyncError> {
    let Some(binding) = resolve_project_cloud_binding(transaction, input.project_id)? else {
        return Ok(None);
    };
    validate_snapshot(&input)?;

    if has_unreconciled_local_resolution(
        transaction,
        &binding.account_id,
        input.project_id,
        input.entity_id,
    )? {
        return Err(NoteSyncError::InvalidSealState(
            "resolution is not durably applied",
        ));
    }

    let existing = transaction
        .query_row(
            "SELECT event.event_id,event.revision,event.parent_event_id,event.local_ordinal,
                    intent.mutation_generation
             FROM cloud_sync_outbox AS event
             JOIN cloud_sync_note_intents AS intent ON intent.event_id=event.event_id
             WHERE event.account_id=?1 AND event.project_id=?2 AND event.entity_id=?3
               AND event.entity_type='note' AND event.lifecycle='unsealed'
               AND event.local_ordinal>0",
            rusqlite::params![binding.account_id, input.project_id, input.entity_id],
            |row| {
                Ok(PreparedNoteIntent {
                    event_id: row.get(0)?,
                    revision: row.get(1)?,
                    parent_event_id: row.get(2)?,
                    local_ordinal: row.get(3)?,
                    mutation_generation: row.get(4)?,
                    coalesced: true,
                })
            },
        )
        .optional()?;
    if let Some(mut intent) = existing {
        if let Some(parent) = intent.parent_event_id.as_deref() {
            if is_resolution_event(transaction, parent)?
                && applied_resolution_parent_revision(
                    transaction,
                    &binding.account_id,
                    input.project_id,
                    input.entity_id,
                    parent,
                )? != intent.revision.checked_sub(1)
            {
                return Err(NoteSyncError::InvalidSealState(
                    "resolution parent proof is incomplete",
                ));
            }
        }
        intent.mutation_generation = intent
            .mutation_generation
            .checked_add(1)
            .filter(|generation| *generation <= MAX_SYNC_INTEGER)
            .ok_or(NoteSyncError::MutationGenerationOverflow)?;
        transaction.execute(
            "UPDATE cloud_sync_outbox
             SET operation=?1,updated_at=?2,deleted_at=?3,attempt_count=0,
                 last_error=NULL,next_attempt_at=NULL
             WHERE event_id=?4",
            rusqlite::params![
                input.operation.as_str(),
                input.updated_at,
                input.deleted_at,
                intent.event_id,
            ],
        )?;
        transaction.execute(
            "UPDATE cloud_sync_note_intents
             SET mutation_generation=?1,snapshot_json=?2,seal_state='pending',
                 seal_attempt_count=0,last_error_code=NULL,next_attempt_at=NULL,
                 state_updated_at=?3
             WHERE event_id=?4",
            rusqlite::params![
                intent.mutation_generation,
                input.snapshot_json,
                input.state_updated_at,
                intent.event_id,
            ],
        )?;
        return Ok(Some(intent));
    }

    let head = read_entity_sync_head(
        transaction,
        &binding.account_id,
        input.project_id,
        input.entity_id,
    )?;
    if let Some(value) = &head {
        if is_resolution_event(transaction, &value.event_id)?
            && applied_resolution_parent_revision(
                transaction,
                &binding.account_id,
                input.project_id,
                input.entity_id,
                &value.event_id,
            )? != Some(value.revision)
        {
            return Err(NoteSyncError::InvalidSealState(
                "resolution parent proof is incomplete",
            ));
        }
    }
    let revision = match &head {
        Some(value) => value
            .revision
            .checked_add(1)
            .filter(|revision| *revision <= MAX_SYNC_INTEGER)
            .ok_or(NoteSyncError::RevisionOverflow)?,
        None => 1,
    };
    let parent_event_id = head.map(|value| value.event_id);
    let local_ordinal = next_local_ordinal(transaction, &binding.account_id, &binding.device_id)?;
    let event_id = match reserved {
        Some(id) => id.to_string(),
        None => canonical_uuid_v4()?,
    };
    transaction.execute(
        "INSERT INTO cloud_sync_outbox(
            event_id,account_id,device_id,project_id,entity_id,entity_type,
            operation,revision,updated_at,deleted_at,created_at,parent_event_id,
            local_ordinal,lifecycle
         ) VALUES(?1,?2,?3,?4,?5,'note',?6,?7,?8,?9,?10,?11,?12,'unsealed')",
        rusqlite::params![
            event_id,
            binding.account_id,
            binding.device_id,
            input.project_id,
            input.entity_id,
            input.operation.as_str(),
            revision,
            input.updated_at,
            input.deleted_at,
            input.state_updated_at,
            parent_event_id,
            local_ordinal,
        ],
    )?;
    transaction.execute(
        "INSERT INTO cloud_sync_note_intents(
            event_id,mutation_generation,snapshot_json,seal_state,
            seal_attempt_count,last_error_code,next_attempt_at,state_updated_at
         ) VALUES(?1,1,?2,'pending',0,NULL,NULL,?3)",
        rusqlite::params![event_id, input.snapshot_json, input.state_updated_at],
    )?;
    Ok(Some(PreparedNoteIntent {
        event_id,
        revision,
        parent_event_id,
        local_ordinal,
        mutation_generation: 1,
        coalesced: false,
    }))
}

pub(crate) fn list_unsealed_note_sync_intents(
    connection: &mut Connection,
    limit: u32,
    retry_blocked: bool,
) -> Result<Vec<UnsealedNoteSyncIntent>, NoteSyncError> {
    if !(1..=MAX_UNSEALED_INTENT_LIST_LIMIT).contains(&limit) {
        return Err(NoteSyncError::InvalidListLimit);
    }
    // Advancing the mode-specific cursor in the same transaction makes a
    // bounded pass durable without changing any intent or event lifecycle.
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let cursor_mode = if retry_blocked {
        "retry_blocked"
    } else {
        "regular"
    };
    let intents = {
        let mut statement = transaction.prepare(
            "WITH cursor AS (
                 SELECT account_id,device_id,local_ordinal,event_id
                 FROM cloud_sync_note_intent_cursors WHERE mode=?1
             )
             SELECT event.event_id,event.account_id,event.device_id,event.project_id,
                event.entity_id,event.entity_type,event.operation,event.revision,
                event.parent_event_id,event.updated_at,event.deleted_at,event.local_ordinal,
                intent.mutation_generation,intent.snapshot_json,intent.seal_state,
                intent.seal_attempt_count,intent.last_error_code,intent.next_attempt_at
         FROM cloud_sync_outbox AS event
         JOIN cloud_sync_note_intents AS intent ON intent.event_id=event.event_id
         CROSS JOIN cursor
         WHERE event.entity_type='note' AND event.lifecycle='unsealed'
           AND NOT EXISTS(SELECT 1 FROM cloud_content_note_writer_events c WHERE c.event_id=event.event_id)
           AND (
               intent.seal_state='pending'
               OR (
                   intent.seal_state='retryable_error'
                   AND (
                       intent.next_attempt_at IS NULL
                       OR intent.next_attempt_at <= strftime('%Y-%m-%dT%H:%M:%fZ','now')
                   )
               )
               OR (?2 AND intent.seal_state='blocked')
           )
         ORDER BY CASE WHEN cursor.event_id IS NULL OR
                    (event.account_id,event.device_id,event.local_ordinal,event.event_id) >
                    (cursor.account_id,cursor.device_id,cursor.local_ordinal,cursor.event_id)
                  THEN 0 ELSE 1 END,
                  event.account_id,event.device_id,event.local_ordinal,event.event_id
         LIMIT ?3",
        )?;
        let rows = statement.query_map(
            rusqlite::params![cursor_mode, retry_blocked, i64::from(limit)],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, Option<String>>(10)?,
                    row.get::<_, i64>(11)?,
                    row.get::<_, i64>(12)?,
                    row.get::<_, String>(13)?,
                    row.get::<_, String>(14)?,
                    row.get::<_, i64>(15)?,
                    row.get::<_, Option<String>>(16)?,
                    row.get::<_, Option<String>>(17)?,
                ))
            },
        )?;
        rows.map(|row| {
            let (
                event_id,
                account_id,
                device_id,
                project_id,
                entity_id,
                entity_type,
                operation,
                revision,
                parent_event_id,
                updated_at,
                deleted_at,
                local_ordinal,
                mutation_generation,
                snapshot_json,
                seal_state,
                seal_attempt_count,
                last_error_code,
                next_attempt_at,
            ) = row?;
            if !(1..=MAX_SYNC_INTEGER).contains(&mutation_generation) {
                return Err(NoteSyncError::InvalidSealState(
                    "mutation generation exceeds the wire integer limit",
                ));
            }
            Ok(UnsealedNoteSyncIntent {
                event_id,
                account_id,
                device_id,
                project_id,
                entity_id,
                entity_type,
                operation: NoteSyncOperation::from_stored(&operation)?,
                revision,
                parent_event_id,
                updated_at,
                deleted_at,
                local_ordinal,
                mutation_generation,
                snapshot_json,
                seal_state: NoteSyncSealState::from_stored(&seal_state)?,
                seal_attempt_count,
                last_error_code,
                next_attempt_at,
            })
        })
        .collect::<Result<Vec<_>, NoteSyncError>>()?
    };
    if let Some(last) = intents.last() {
        let changed = transaction.execute(
            "UPDATE cloud_sync_note_intent_cursors
             SET account_id=?1,device_id=?2,local_ordinal=?3,event_id=?4,
                 updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
             WHERE mode=?5",
            rusqlite::params![
                last.account_id,
                last.device_id,
                last.local_ordinal,
                last.event_id,
                cursor_mode,
            ],
        )?;
        if changed != 1 {
            return Err(NoteSyncError::InvalidSealState("missing fairness cursor"));
        }
    }
    transaction.commit()?;
    Ok(intents)
}

/// Lists one account's sealed Note events in a dependency-safe order. Its only
/// durable side effect is the per-account device round-robin cursor.
/// The ciphertext batch cap is applied before the DTOs leave SQLite.
pub(crate) fn list_sealed_note_sync_outbox(
    connection: &mut Connection,
    account_id: &str,
    limit: u32,
) -> Result<Vec<SealedNoteSyncOutboxItem>, NoteSyncError> {
    if account_id.is_empty() || account_id.len() > 512 {
        return Err(NoteSyncError::InvalidOutboxRead("invalid account scope"));
    }
    if !(1..=MAX_SEALED_OUTBOX_LIST_LIMIT).contains(&limit) {
        return Err(NoteSyncError::InvalidListLimit);
    }

    let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
    let rows = {
        let mut statement = transaction.prepare(
            "WITH eligible AS (
                SELECT event.* FROM cloud_sync_outbox AS event
                WHERE event.account_id=?1 AND event.entity_type='note' AND event.lifecycle='sealed'
                  AND NOT EXISTS(SELECT 1 FROM cloud_content_note_writer_events c WHERE c.event_id=event.event_id)
                  AND (event.next_attempt_at IS NULL OR event.next_attempt_at <= strftime('%Y-%m-%dT%H:%M:%fZ','now'))
                  AND (event.parent_event_id IS NULL OR EXISTS (
                       SELECT 1 FROM cloud_sync_outbox AS eligible_parent
                       WHERE eligible_parent.event_id=event.parent_event_id
                         AND (eligible_parent.lifecycle='accepted' OR (
                              eligible_parent.lifecycle='sealed'
                              AND (eligible_parent.next_attempt_at IS NULL OR eligible_parent.next_attempt_at <= strftime('%Y-%m-%dT%H:%M:%fZ','now'))
                         ))
                  ) OR EXISTS (
                       SELECT 1 FROM cloud_sync_note_causal_history AS remote_parent
                       WHERE remote_parent.account_id=event.account_id
                         AND remote_parent.event_id=event.parent_event_id
                         AND remote_parent.project_id=event.project_id
                         AND remote_parent.entity_id=event.entity_id
                         AND remote_parent.entity_type=event.entity_type
                         AND remote_parent.revision=event.revision-1
                  ))
             ), selected_device AS (
                SELECT device_id FROM eligible
                WHERE device_id > COALESCE((SELECT device_id FROM cloud_sync_note_upload_cursors WHERE account_id=?1), '')
                ORDER BY device_id LIMIT 1
             ), chosen_device AS (
                SELECT device_id FROM selected_device
                UNION ALL
                SELECT device_id FROM eligible
                WHERE NOT EXISTS (SELECT 1 FROM selected_device)
                ORDER BY device_id LIMIT 1
             )
             SELECT event.event_id,event.account_id,event.device_id,event.project_id,
                event.entity_id,event.entity_type,event.operation,event.revision,
                event.parent_event_id,event.updated_at,event.deleted_at,event.local_ordinal,
                object.crypto_version,object.aad_version,object.nonce,object.ciphertext,
                (SELECT count(*) FROM cloud_sync_event_objects AS duplicate
                 WHERE duplicate.event_id=event.event_id),
                (SELECT count(*) FROM cloud_sync_note_intents AS intent
                 WHERE intent.event_id=event.event_id),
                COALESCE(parent.account_id,remote_parent.account_id),
                COALESCE(parent.project_id,remote_parent.project_id),
                COALESCE(parent.entity_id,remote_parent.entity_id),
                COALESCE(parent.entity_type,remote_parent.entity_type),
                COALESCE(parent.revision,remote_parent.revision),
                CASE WHEN parent.event_id IS NOT NULL THEN parent.lifecycle
                     WHEN remote_parent.event_id IS NOT NULL THEN 'accepted' END
             FROM eligible AS event
             LEFT JOIN cloud_sync_event_objects AS object
               ON object.account_id=event.account_id AND object.event_id=event.event_id
             LEFT JOIN cloud_sync_outbox AS parent ON parent.event_id=event.parent_event_id
             LEFT JOIN cloud_sync_note_causal_history AS remote_parent
               ON remote_parent.account_id=event.account_id
              AND remote_parent.event_id=event.parent_event_id
             WHERE event.device_id=(SELECT device_id FROM chosen_device)
             ORDER BY event.revision,event.device_id,event.local_ordinal,event.event_id
             LIMIT ?2",
        )?;
        let rows = statement
            .query_map(rusqlite::params![account_id, i64::from(limit)], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, Option<String>>(10)?,
                    row.get::<_, i64>(11)?,
                    row.get::<_, Option<i64>>(12)?,
                    row.get::<_, Option<i64>>(13)?,
                    row.get::<_, Option<Vec<u8>>>(14)?,
                    row.get::<_, Option<Vec<u8>>>(15)?,
                    row.get::<_, i64>(16)?,
                    row.get::<_, i64>(17)?,
                    row.get::<_, Option<String>>(18)?,
                    row.get::<_, Option<String>>(19)?,
                    row.get::<_, Option<String>>(20)?,
                    row.get::<_, Option<String>>(21)?,
                    row.get::<_, Option<i64>>(22)?,
                    row.get::<_, Option<String>>(23)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    };

    let mut items = Vec::with_capacity(rows.len());
    let mut ciphertext_total = 0_usize;
    for row in rows {
        let (
            event_id,
            stored_account_id,
            device_id,
            project_id,
            entity_id,
            entity_type,
            operation,
            revision,
            parent_event_id,
            updated_at,
            deleted_at,
            local_ordinal,
            crypto_version,
            aad_version,
            nonce,
            ciphertext,
            object_count,
            sidecar_count,
            parent_account_id,
            parent_project_id,
            parent_entity_id,
            parent_entity_type,
            parent_revision,
            parent_lifecycle,
        ) = row;
        if sidecar_count != 0 || object_count != 1 {
            return Err(NoteSyncError::InvalidOutboxRead(
                "sealed event/object relation is inconsistent",
            ));
        }
        let (crypto_version, aad_version, nonce, ciphertext) =
            match (crypto_version, aad_version, nonce, ciphertext) {
                (Some(crypto_version), Some(aad_version), Some(nonce), Some(ciphertext)) => {
                    (crypto_version, aad_version, nonce, ciphertext)
                }
                _ => return Err(NoteSyncError::SealedObjectMissing),
            };
        if crypto_version != SUPPORTED_CRYPTO_VERSION
            || aad_version != SUPPORTED_AAD_VERSION
            || nonce.len() != 24
            || !(16..=MAX_ENCRYPTED_SYNC_CIPHERTEXT_BYTES).contains(&ciphertext.len())
        {
            return Err(NoteSyncError::InvalidOutboxRead(
                "sealed encrypted object is invalid",
            ));
        }
        match (revision, parent_event_id.as_deref()) {
            (1, None) => {}
            (1, Some(_)) | (_, None) => {
                return Err(NoteSyncError::InvalidOutboxRead(
                    "invalid revision dependency",
                ))
            }
            (_, Some(_)) => {
                if parent_account_id.as_deref() != Some(account_id)
                    || parent_project_id.as_deref() != Some(project_id.as_str())
                    || parent_entity_id.as_deref() != Some(entity_id.as_str())
                    || parent_entity_type.as_deref() != Some("note")
                    || parent_revision != Some(revision - 1)
                    || !matches!(
                        parent_lifecycle.as_deref(),
                        Some("sealed") | Some("accepted")
                    )
                {
                    return Err(NoteSyncError::InvalidOutboxRead(
                        "parent dependency is unavailable",
                    ));
                }
            }
        }
        let next_total = ciphertext_total.checked_add(ciphertext.len()).ok_or(
            NoteSyncError::InvalidOutboxRead("ciphertext batch size overflow"),
        )?;
        if next_total > MAX_ENCRYPTED_SYNC_BATCH_CIPHERTEXT_BYTES {
            break;
        }
        ciphertext_total = next_total;
        items.push(SealedNoteSyncOutboxItem {
            event_id,
            account_id: stored_account_id,
            device_id,
            project_id,
            entity_id,
            entity_type,
            operation: NoteSyncOperation::from_stored(&operation)?,
            revision,
            parent_event_id,
            updated_at,
            deleted_at,
            local_ordinal,
            envelope: EncryptedNoteSyncEnvelope {
                crypto_version,
                aad_version,
                nonce: encode_canonical_base64url(&nonce),
                ciphertext: encode_canonical_base64url(&ciphertext),
            },
        });
    }
    if serde_json::to_vec(&items)
        .map_err(|_| NoteSyncError::InvalidOutboxRead("could not encode outbox DTO"))?
        .len()
        > MAX_ENCRYPTED_SYNC_WIRE_BODY_BYTES
    {
        return Err(NoteSyncError::InvalidOutboxRead(
            "outbox DTO exceeds wire body limit",
        ));
    }
    if let Some(item) = items.first() {
        transaction.execute(
            "INSERT INTO cloud_sync_note_upload_cursors(account_id,device_id,updated_at)
             VALUES(?1,?2,strftime('%Y-%m-%dT%H:%M:%fZ','now'))
             ON CONFLICT(account_id) DO UPDATE SET device_id=excluded.device_id,updated_at=excluded.updated_at",
            rusqlite::params![account_id, item.device_id],
        )?;
    }
    transaction.commit()?;
    Ok(items)
}

pub(crate) fn record_note_sync_seal_failure(
    connection: &mut Connection,
    command: &RecordNoteSyncSealFailureCommand,
) -> Result<RecordNoteSyncSealFailureResult, NoteSyncError> {
    if command.event_id.is_empty()
        || !(1..=MAX_SYNC_INTEGER).contains(&command.expected_mutation_generation)
    {
        return Err(NoteSyncError::InvalidSealState("invalid failure identity"));
    }
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let lifecycle = transaction
        .query_row(
            "SELECT lifecycle FROM cloud_sync_outbox
             WHERE event_id=?1 AND entity_type='note'",
            [&command.event_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .ok_or(NoteSyncError::MissingEvent)?;
    if lifecycle == "sealed" {
        ensure_consistent_sealed_event(&transaction, &command.event_id)?;
        transaction.commit()?;
        return Ok(RecordNoteSyncSealFailureResult::AlreadySealed);
    }
    if lifecycle != "unsealed" {
        return Err(NoteSyncError::UnexpectedLifecycle);
    }
    let current_generation = transaction
        .query_row(
            "SELECT mutation_generation FROM cloud_sync_note_intents WHERE event_id=?1",
            [&command.event_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .ok_or(NoteSyncError::MissingIntent)?;
    if current_generation != command.expected_mutation_generation {
        transaction.commit()?;
        return Ok(RecordNoteSyncSealFailureResult::StaleGeneration);
    }
    let attempt_count = transaction.query_row(
        "SELECT seal_attempt_count FROM cloud_sync_note_intents WHERE event_id=?1",
        [&command.event_id],
        |row| row.get::<_, i64>(0),
    )?;
    let next_attempt_count = attempt_count
        .checked_add(1)
        .filter(|value| *value <= MAX_SYNC_INTEGER)
        .ok_or(NoteSyncError::SealAttemptOverflow)?;
    let seal_state = command.error_code.seal_state();
    let next_attempt_seconds = (seal_state == NoteSyncSealState::RetryableError)
        .then(|| retry_backoff_seconds(next_attempt_count));
    let changed = transaction.execute(
        "UPDATE cloud_sync_note_intents
         SET seal_state=?1,seal_attempt_count=?2,last_error_code=?3,
             next_attempt_at=CASE WHEN ?4 IS NULL THEN NULL ELSE
                 strftime('%Y-%m-%dT%H:%M:%fZ','now',printf('+%d seconds',?4)) END,
             state_updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE event_id=?5 AND mutation_generation=?6",
        rusqlite::params![
            match seal_state {
                NoteSyncSealState::Pending => "pending",
                NoteSyncSealState::RetryableError => "retryable_error",
                NoteSyncSealState::Blocked => "blocked",
                NoteSyncSealState::InvariantError => "invariant_error",
            },
            next_attempt_count,
            command.error_code.as_str(),
            next_attempt_seconds,
            command.event_id,
            command.expected_mutation_generation,
        ],
    )?;
    if changed != 1 {
        return Err(NoteSyncError::MissingIntent);
    }
    transaction.commit()?;
    Ok(RecordNoteSyncSealFailureResult::Recorded)
}

pub(crate) fn commit_sealed_note_sync_event(
    connection: &mut Connection,
    command: &CommitSealedNoteSyncEventCommand,
) -> Result<CommitSealedNoteSyncEventResult, NoteSyncError> {
    if command.event_id.is_empty()
        || !(1..=MAX_SYNC_INTEGER).contains(&command.expected_mutation_generation)
    {
        return Err(NoteSyncError::InvalidSealState("invalid sealing identity"));
    }
    let envelope = decode_encrypted_note_sync_envelope(&command.envelope)?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let event = transaction
        .query_row(
            "SELECT account_id,entity_type,lifecycle FROM cloud_sync_outbox WHERE event_id=?1",
            [&command.event_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?
        .ok_or(NoteSyncError::MissingEvent)?;
    let (account_id, entity_type, lifecycle) = event;
    if entity_type != "note" {
        return Err(NoteSyncError::UnexpectedLifecycle);
    }
    if lifecycle == "sealed" {
        ensure_matching_sealed_envelope(&transaction, &account_id, &command.event_id, &envelope)?;
        transaction.commit()?;
        return Ok(CommitSealedNoteSyncEventResult::AlreadySealed);
    }
    if lifecycle != "unsealed" {
        return Err(NoteSyncError::UnexpectedLifecycle);
    }
    let current_generation = transaction
        .query_row(
            "SELECT mutation_generation FROM cloud_sync_note_intents WHERE event_id=?1",
            [&command.event_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .ok_or(NoteSyncError::MissingIntent)?;
    if current_generation != command.expected_mutation_generation {
        transaction.commit()?;
        return Ok(CommitSealedNoteSyncEventResult::StaleGeneration);
    }
    if transaction
        .query_row(
            "SELECT 1 FROM cloud_sync_event_objects WHERE event_id=?1",
            [&command.event_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some()
    {
        return Err(NoteSyncError::ConflictingEncryptedObject);
    }
    transaction.execute(
        "INSERT INTO cloud_sync_event_objects(
            account_id,event_id,crypto_version,aad_version,nonce,ciphertext,stored_at
         ) VALUES(?1,?2,?3,?4,?5,?6,strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        rusqlite::params![
            account_id,
            command.event_id,
            envelope.crypto_version,
            envelope.aad_version,
            envelope.nonce,
            envelope.ciphertext,
        ],
    )?;
    let deleted = transaction.execute(
        "DELETE FROM cloud_sync_note_intents
         WHERE event_id=?1 AND mutation_generation=?2",
        rusqlite::params![command.event_id, command.expected_mutation_generation],
    )?;
    if deleted != 1 {
        return Err(NoteSyncError::MissingIntent);
    }
    let updated = transaction.execute(
        "UPDATE cloud_sync_outbox SET lifecycle='sealed'
         WHERE event_id=?1 AND entity_type='note' AND lifecycle='unsealed'",
        [&command.event_id],
    )?;
    if updated != 1 {
        return Err(NoteSyncError::UnexpectedLifecycle);
    }
    transaction.commit()?;
    Ok(CommitSealedNoteSyncEventResult::Sealed)
}

pub(crate) fn list_unsealed_note_resolution_intents(
    connection: &mut Connection,
    account_id: &str,
    limit: u32,
) -> Result<Vec<UnsealedNoteResolutionIntent>, NoteSyncError> {
    if account_id.is_empty() || !(1..=MAX_UNSEALED_INTENT_LIST_LIMIT).contains(&limit) {
        return Err(NoteSyncError::InvalidListLimit);
    }
    let mut statement = connection.prepare(
        "SELECT resolution_event_id,account_id,device_id,project_id,entity_id,canonical_payload
         FROM cloud_sync_note_resolution_outbox WHERE account_id=?1 AND lifecycle='local_pending'
           AND NOT EXISTS(SELECT 1 FROM cloud_content_note_resolution_events c WHERE c.event_id=cloud_sync_note_resolution_outbox.resolution_event_id)
         ORDER BY applied_at,resolution_event_id LIMIT ?2",
    )?;
    let intents = statement
        .query_map(rusqlite::params![account_id, i64::from(limit)], |row| {
            Ok(UnsealedNoteResolutionIntent {
                event_id: row.get(0)?,
                account_id: row.get(1)?,
                device_id: row.get(2)?,
                project_id: row.get(3)?,
                entity_id: row.get(4)?,
                canonical_payload: encode_canonical_base64url(&row.get::<_, Vec<u8>>(5)?),
            })
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(NoteSyncError::Database)?;
    Ok(intents)
}

pub(crate) fn commit_sealed_note_resolution_event(
    connection: &mut Connection,
    command: &CommitSealedNoteResolutionCommand,
) -> Result<CommitSealedNoteResolutionResult, NoteSyncError> {
    commit_sealed_note_resolution_event_inner(connection, command, false)
}

fn commit_sealed_note_resolution_event_inner(
    connection: &mut Connection,
    command: &CommitSealedNoteResolutionCommand,
    inject_failure_after_object: bool,
) -> Result<CommitSealedNoteResolutionResult, NoteSyncError> {
    let envelope = decode_encrypted_note_sync_envelope(&command.envelope)?;
    let expected_payload =
        decode_canonical_base64url(&command.expected_canonical_payload, 1, 8_388_608)?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    validate_pull_scope(
        &transaction,
        &command.account_id,
        &command.device_id,
        &command.canonical_user_id,
    )?;
    let project_bound:i64=transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM cloud_sync_project_bindings WHERE project_id=?1 AND account_id=?2)",
        rusqlite::params![command.project_id,command.account_id],|row|row.get(0),
    )?;
    if project_bound != 1 {
        return Err(NoteSyncError::InvalidSealState(
            "resolution project scope mismatch",
        ));
    }
    let row:Option<(String,String,String,String,Vec<u8>,String)>=transaction.query_row(
        "SELECT account_id,device_id,project_id,entity_id,canonical_payload,lifecycle FROM cloud_sync_note_resolution_outbox WHERE resolution_event_id=?1",
        [&command.event_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?)),
    ).optional()?;
    let Some((account, device, project, entity, payload, lifecycle)) = row else {
        return Err(NoteSyncError::MissingEvent);
    };
    if account != command.account_id
        || device != command.device_id
        || project != command.project_id
        || entity != command.entity_id
        || payload != expected_payload
    {
        return Err(NoteSyncError::InvalidSealState(
            "resolution identity changed",
        ));
    }
    if lifecycle == "sealed_local" {
        ensure_matching_sealed_envelope(&transaction, &account, &command.event_id, &envelope)?;
        transaction.commit()?;
        return Ok(CommitSealedNoteResolutionResult::AlreadySealed);
    }
    if lifecycle != "local_pending" {
        return Err(NoteSyncError::UnexpectedLifecycle);
    }
    if transaction
        .query_row(
            "SELECT 1 FROM cloud_sync_event_objects WHERE event_id=?1",
            [&command.event_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some()
    {
        return Err(NoteSyncError::ConflictingEncryptedObject);
    }
    transaction.execute("INSERT INTO cloud_sync_event_objects(account_id,event_id,crypto_version,aad_version,nonce,ciphertext,stored_at) VALUES(?1,?2,?3,?4,?5,?6,strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        rusqlite::params![account,command.event_id,envelope.crypto_version,envelope.aad_version,envelope.nonce,envelope.ciphertext])?;
    if inject_failure_after_object {
        return Err(NoteSyncError::InvalidSealState(
            "injected resolution sealing failure",
        ));
    }
    let changed=transaction.execute("UPDATE cloud_sync_note_resolution_outbox SET lifecycle='sealed_local',updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE resolution_event_id=?1 AND lifecycle='local_pending'",[&command.event_id])?;
    if changed != 1 {
        return Err(NoteSyncError::UnexpectedLifecycle);
    }
    transaction.commit()?;
    Ok(CommitSealedNoteResolutionResult::Sealed)
}

/// This is intentionally a read-only proof for a future v2 dispatcher. A
/// sealed local parent is insufficient: local branches require acceptance.
pub(crate) fn read_note_resolution_readiness(
    connection: &mut Connection,
    account_id: &str,
    event_id: &str,
) -> Result<NoteResolutionReadiness, NoteSyncError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
    let ready = resolution_dependencies_ready(&transaction, account_id, event_id)?;
    transaction.commit()?;
    Ok(NoteResolutionReadiness {
        event_id: event_id.into(),
        ready,
    })
}

/// Re-evaluates every frozen parent from durable evidence in the caller's
/// transaction. This deliberately has no cached readiness state.
pub(crate) fn resolution_dependencies_ready(
    transaction: &Transaction<'_>,
    account_id: &str,
    event_id: &str,
) -> Result<bool, NoteSyncError> {
    resolution_dependencies_ready_for(transaction, account_id, event_id, false)
}
pub(crate) fn resolution_dependencies_ready_for(
    transaction: &Transaction<'_>,
    account_id: &str,
    event_id: &str,
    allow_unsealed: bool,
) -> Result<bool, NoteSyncError> {
    let lifecycle:Option<String>=transaction.query_row("SELECT lifecycle FROM cloud_sync_note_resolution_outbox WHERE resolution_event_id=?1 AND account_id=?2",rusqlite::params![event_id,account_id],|r|r.get(0)).optional()?;
    let Some(lifecycle) = lifecycle else {
        return Err(NoteSyncError::MissingEvent);
    };
    let invalid:i64=transaction.query_row(
        "SELECT count(*) FROM cloud_sync_note_resolution_dependencies d WHERE d.resolution_event_id=?1 AND NOT (
          (d.source='remote_applied' AND EXISTS(SELECT 1 FROM cloud_sync_note_causal_history h JOIN cloud_sync_note_conflict_versions v ON v.event_id=h.event_id AND v.group_id=(SELECT conflict_group_id FROM cloud_sync_note_resolution_outbox WHERE resolution_event_id=?1) WHERE h.account_id=?2 AND h.event_id=d.parent_event_id AND h.server_sequence=d.server_sequence AND h.project_id=(SELECT project_id FROM cloud_sync_note_resolution_outbox WHERE resolution_event_id=?1) AND h.entity_id=(SELECT entity_id FROM cloud_sync_note_resolution_outbox WHERE resolution_event_id=?1) AND h.entity_type='note' AND h.revision=v.revision AND h.snapshot_json=d.snapshot_json AND v.snapshot_json=d.snapshot_json))
          OR (d.source='remote' AND EXISTS(SELECT 1 FROM cloud_sync_inbox i JOIN cloud_sync_note_conflict_versions v ON v.event_id=i.event_id AND v.group_id=(SELECT conflict_group_id FROM cloud_sync_note_resolution_outbox WHERE resolution_event_id=?1) WHERE i.account_id=?2 AND i.event_id=d.parent_event_id AND i.server_sequence=d.server_sequence AND i.project_id=(SELECT project_id FROM cloud_sync_note_resolution_outbox WHERE resolution_event_id=?1) AND i.entity_id=(SELECT entity_id FROM cloud_sync_note_resolution_outbox WHERE resolution_event_id=?1) AND i.entity_type='note' AND i.sync_revision=v.revision AND i.state='conflict_preserved' AND v.snapshot_json=d.snapshot_json))
          OR (d.source='local_unsealed' AND EXISTS(SELECT 1 FROM cloud_sync_outbox o JOIN cloud_sync_upload_receipts r ON r.account_id=o.account_id AND r.event_id=o.event_id JOIN cloud_sync_note_conflict_versions v ON v.event_id=o.event_id AND v.group_id=(SELECT conflict_group_id FROM cloud_sync_note_resolution_outbox WHERE resolution_event_id=?1) WHERE o.account_id=?2 AND o.event_id=d.parent_event_id AND o.project_id=(SELECT project_id FROM cloud_sync_note_resolution_outbox WHERE resolution_event_id=?1) AND o.entity_id=(SELECT entity_id FROM cloud_sync_note_resolution_outbox WHERE resolution_event_id=?1) AND o.entity_type='note' AND o.revision=v.revision AND o.lifecycle='accepted' AND v.local_mutation_generation=d.local_mutation_generation AND v.snapshot_json=d.snapshot_json))
        )",rusqlite::params![event_id,account_id],|r|r.get(0))?;
    Ok(
        (lifecycle == "sealed_local" || (allow_unsealed && lifecycle == "local_pending"))
            && invalid == 0,
    )
}

/// Reads a bounded, freshly-proven set of sealed v2 resolution envelopes.
/// It never returns the canonical v2 plaintext, parent set, AMK, or a durable
/// readiness flag. A future uploader must repeat this query immediately before
/// dispatch because no SQLite transaction is held across HTTP.
pub(crate) fn list_sealed_note_resolution_uploads(
    connection: &mut Connection,
    command: &ListSealedNoteResolutionUploadsCommand,
) -> Result<Vec<SealedNoteResolutionUploadItem>, NoteSyncError> {
    if !(1..=MAX_SEALED_RESOLUTION_UPLOAD_LIST_LIMIT).contains(&command.limit) {
        return Err(NoteSyncError::InvalidListLimit);
    }
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
    validate_pull_scope(
        &transaction,
        &command.account_id,
        &command.device_id,
        &command.canonical_user_id,
    )?;
    let mut statement=transaction.prepare(
        "SELECT resolution.resolution_event_id,resolution.account_id,resolution.device_id,
                resolution.project_id,resolution.entity_id,resolution.revision,resolution.canonical_payload,
                object.crypto_version,object.aad_version,object.nonce,object.ciphertext
         FROM cloud_sync_note_resolution_outbox AS resolution
         LEFT JOIN cloud_sync_event_objects AS object
           ON object.account_id=resolution.account_id AND object.event_id=resolution.resolution_event_id
         WHERE resolution.account_id=?1 AND resolution.device_id=?2 AND resolution.lifecycle='sealed_local'
           AND NOT EXISTS(SELECT 1 FROM cloud_content_note_resolution_events c WHERE c.event_id=resolution.resolution_event_id)
         ORDER BY resolution.applied_at,resolution.resolution_event_id LIMIT ?3"
    )?;
    let rows = statement.query_map(
        rusqlite::params![
            command.account_id,
            command.device_id,
            i64::from(command.limit)
        ],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, Vec<u8>>(6)?,
                row.get::<_, Option<i64>>(7)?,
                row.get::<_, Option<i64>>(8)?,
                row.get::<_, Option<Vec<u8>>>(9)?,
                row.get::<_, Option<Vec<u8>>>(10)?,
            ))
        },
    )?;
    let mut items = Vec::new();
    let mut aggregate = 0usize;
    for row in rows {
        let (
            event_id,
            account_id,
            device_id,
            project_id,
            entity_id,
            revision,
            payload,
            crypto_version,
            aad_version,
            nonce,
            ciphertext,
        ) = row?;
        if !resolution_dependencies_ready(&transaction, &command.account_id, &event_id)? {
            continue;
        }
        let (crypto_version, aad_version, nonce, ciphertext) =
            match (crypto_version, aad_version, nonce, ciphertext) {
                (Some(crypto), Some(aad), Some(nonce), Some(ciphertext)) => {
                    (crypto, aad, nonce, ciphertext)
                }
                _ => return Err(NoteSyncError::SealedObjectMissing),
            };
        let decoded = decode_encrypted_note_sync_envelope(&EncryptedNoteSyncEnvelope {
            crypto_version,
            aad_version,
            nonce: encode_canonical_base64url(&nonce),
            ciphertext: encode_canonical_base64url(&ciphertext),
        })?;
        aggregate = aggregate.checked_add(decoded.ciphertext.len()).ok_or(
            NoteSyncError::InvalidEnvelope("resolution upload object overflow"),
        )?;
        if aggregate > MAX_ENCRYPTED_SYNC_BATCH_CIPHERTEXT_BYTES {
            return Err(NoteSyncError::InvalidEnvelope(
                "resolution upload batch too large",
            ));
        }
        let plaintext = decode_note_sync_resolution_v2(&payload)
            .map_err(|_| NoteSyncError::InvalidSealState("invalid stored resolution payload"))?;
        if plaintext.header.event_id != event_id
            || plaintext.header.project_id != project_id
            || plaintext.header.entity_id != entity_id
            || plaintext.header.revision != revision
        {
            return Err(NoteSyncError::InvalidSealState(
                "resolution transport metadata mismatch",
            ));
        }
        items.push(SealedNoteResolutionUploadItem {
            event_id,
            account_id,
            device_id,
            project_id,
            entity_id,
            entity_type: "note".into(),
            operation: "resolution".into(),
            revision,
            updated_at: plaintext.header.updated_at,
            envelope: StoredEncryptedNoteSyncEnvelope {
                crypto_version: decoded.crypto_version,
                aad_version: decoded.aad_version,
                nonce: encode_canonical_base64url(&decoded.nonce),
                ciphertext: encode_canonical_base64url(&decoded.ciphertext),
            },
        });
    }
    drop(statement);
    transaction.commit()?;
    Ok(items)
}

pub(crate) fn commit_note_resolution_upload_acceptance(
    connection: &mut Connection,
    command: &CommitNoteResolutionUploadAcceptanceCommand,
) -> Result<Vec<CommitNoteResolutionUploadAcceptanceResult>, NoteSyncError> {
    commit_note_resolution_upload_acceptance_inner(connection, command, false)
}

fn commit_note_resolution_upload_acceptance_inner(
    connection: &mut Connection,
    command: &CommitNoteResolutionUploadAcceptanceCommand,
    inject_failure_after_receipt: bool,
) -> Result<Vec<CommitNoteResolutionUploadAcceptanceResult>, NoteSyncError> {
    if command.receipts.is_empty()
        || command.receipts.len() > MAX_SEALED_RESOLUTION_UPLOAD_LIST_LIMIT as usize
    {
        return Err(NoteSyncError::InvalidSealState(
            "invalid resolution receipt batch",
        ));
    }
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    validate_pull_scope(
        &transaction,
        &command.account_id,
        &command.device_id,
        &command.canonical_user_id,
    )?;
    let mut results = Vec::with_capacity(command.receipts.len());
    for receipt in &command.receipts {
        if receipt.event_id.len() != 36
            || !(1..=MAX_SYNC_INTEGER).contains(&receipt.server_sequence)
        {
            return Err(NoteSyncError::InvalidSealState(
                "invalid resolution receipt",
            ));
        }
        let row:Option<(String,String,String)>=transaction.query_row(
            "SELECT account_id,device_id,lifecycle FROM cloud_sync_note_resolution_outbox WHERE resolution_event_id=?1",[&receipt.event_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
        ).optional()?;
        let Some((account_id, device_id, lifecycle)) = row else {
            return Err(NoteSyncError::MissingEvent);
        };
        if account_id != command.account_id || device_id != command.device_id {
            return Err(NoteSyncError::ConflictingUploadReceipt);
        }
        let object:Option<(i64,i64,Vec<u8>,Vec<u8>)>=transaction.query_row(
            "SELECT crypto_version,aad_version,nonce,ciphertext FROM cloud_sync_event_objects WHERE account_id=?1 AND event_id=?2",
            rusqlite::params![account_id,receipt.event_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
        ).optional()?;
        let Some((crypto_version, aad_version, nonce, ciphertext)) = object else {
            return Err(NoteSyncError::SealedObjectMissing);
        };
        decode_encrypted_note_sync_envelope(&EncryptedNoteSyncEnvelope {
            crypto_version,
            aad_version,
            nonce: encode_canonical_base64url(&nonce),
            ciphertext: encode_canonical_base64url(&ciphertext),
        })?;
        let existing:Option<i64>=transaction.query_row(
            "SELECT server_sequence FROM cloud_sync_note_resolution_upload_receipts WHERE resolution_event_id=?1",[&receipt.event_id],|row|row.get(0),
        ).optional()?;
        if let Some(sequence) = existing {
            if sequence != receipt.server_sequence || lifecycle != "accepted" {
                return Err(NoteSyncError::ConflictingUploadReceipt);
            }
            results.push(CommitNoteResolutionUploadAcceptanceResult::AlreadyAccepted);
            continue;
        }
        if lifecycle != "sealed_local" {
            return Err(NoteSyncError::UnexpectedLifecycle);
        }
        let v1_sequence_in_use:bool=transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM cloud_sync_upload_receipts WHERE account_id=?1 AND server_sequence=?2)",
            rusqlite::params![command.account_id,receipt.server_sequence],|row|row.get(0),
        )?;
        if v1_sequence_in_use {
            return Err(NoteSyncError::ConflictingUploadReceipt);
        }
        transaction.execute("INSERT INTO cloud_sync_note_resolution_upload_receipts(account_id,resolution_event_id,device_id,server_sequence,duplicate,accepted_at) VALUES(?1,?2,?3,?4,?5,strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
            rusqlite::params![command.account_id,receipt.event_id,command.device_id,receipt.server_sequence,i64::from(receipt.duplicate)])?;
        if inject_failure_after_receipt {
            return Err(NoteSyncError::InvalidSealState(
                "injected resolution receipt failure",
            ));
        }
        let changed=transaction.execute("UPDATE cloud_sync_note_resolution_outbox SET lifecycle='accepted',updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE resolution_event_id=?1 AND lifecycle='sealed_local'",[&receipt.event_id])?;
        if changed != 1 {
            return Err(NoteSyncError::UnexpectedLifecycle);
        }
        results.push(CommitNoteResolutionUploadAcceptanceResult::Accepted);
    }
    transaction.commit()?;
    Ok(results)
}

fn valid_inbound_identity(value: &str, maximum: usize) -> bool {
    !value.is_empty() && value.len() <= maximum
}

pub(crate) fn validate_pull_scope(
    transaction: &Transaction<'_>,
    account_id: &str,
    device_id: &str,
    canonical_user_id: &str,
) -> Result<(), NoteSyncError> {
    if !valid_inbound_identity(account_id, 512)
        || device_id.len() != 36
        || canonical_user_id.len() != 36
    {
        return Err(NoteSyncError::InvalidEnvelope("invalid inbox scope"));
    }
    let bound: Option<String> = transaction
        .query_row(
            "SELECT canonical_user_id FROM cloud_account_bindings WHERE local_account_id=?1",
            [account_id],
            |row| row.get(0),
        )
        .optional()?;
    if bound.as_deref() != Some(canonical_user_id) {
        return Err(NoteSyncError::InvalidEnvelope("account binding mismatch"));
    }
    let stored_device: Option<String> = transaction
        .query_row(
            "SELECT device_id FROM cloud_sync_state WHERE account_id=?1",
            [account_id],
            |row| row.get(0),
        )
        .optional()?;
    if stored_device.as_deref() != Some(device_id) {
        return Err(NoteSyncError::InvalidEnvelope("pulling device mismatch"));
    }
    Ok(())
}

pub(crate) fn read_note_sync_pull_state(
    connection: &mut Connection,
    command: &ReadNoteSyncPullStateCommand,
) -> Result<NoteSyncPullState, NoteSyncError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    validate_pull_scope(
        &transaction,
        &command.account_id,
        &command.device_id,
        &command.canonical_user_id,
    )?;
    let state = transaction.query_row(
        "SELECT pull_cursor,ack_cursor FROM cloud_sync_state WHERE account_id=?1",
        [&command.account_id],
        |row| {
            Ok(NoteSyncPullState {
                pull_cursor: row.get(0)?,
                ack_cursor: row.get(1)?,
            })
        },
    )?;
    transaction.commit()?;
    Ok(state)
}

fn contiguous_applied_ack_prefix(
    transaction: &Transaction<'_>,
    account_id: &str,
    ack_cursor: i64,
    pull_cursor: i64,
) -> Result<i64, NoteSyncError> {
    if !(0..=MAX_SYNC_INTEGER).contains(&ack_cursor)
        || !(0..=MAX_SYNC_INTEGER).contains(&pull_cursor)
        || ack_cursor > pull_cursor
    {
        return Err(NoteSyncError::InvalidEnvelope("invalid durable ACK state"));
    }
    let mut candidate = ack_cursor;
    while candidate < pull_cursor {
        let next = candidate
            .checked_add(1)
            .ok_or(NoteSyncError::InvalidEnvelope("ACK cursor overflow"))?;
        let account_blocker: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM cloud_sync_account_inbox WHERE account_id=?1 AND server_sequence=?2)",
            rusqlite::params![account_id, next], |row| row.get(0),
        )?;
        if account_blocker {
            let game:bool=transaction.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_account_inbox WHERE account_id=?1 AND server_sequence=?2 AND entity_type='account_game')",rusqlite::params![account_id,next],|r|r.get(0))?;
            let proven = if game {
                crate::game_sync::ack_proven(transaction, account_id, next)?
            } else {
                crate::account_catalog::ack_proven(transaction, account_id, next)
                    .map_err(|_| NoteSyncError::InvalidEnvelope("catalog ACK proof unavailable"))?
            };
            if !proven {
                break;
            }
            candidate = next;
            continue;
        }
        let game:bool=transaction.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_inbox WHERE account_id=?1 AND server_sequence=?2 AND entity_type='project_game')",rusqlite::params![account_id,next],|r|r.get(0))?;
        if game {
            if !crate::game_sync::ack_proven(transaction, account_id, next)? {
                break;
            }
            candidate = next;
            continue;
        }
        let progress:bool=transaction.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_inbox WHERE account_id=?1 AND server_sequence=?2 AND entity_type='progress')",rusqlite::params![account_id,next],|r|r.get(0))?;
        if progress {
            if !crate::progress_sync::ack_proven(transaction, account_id, next)? {
                break;
            }
            candidate = next;
            continue;
        }
        let document:bool=transaction.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_inbox WHERE account_id=?1 AND server_sequence=?2 AND entity_type='document')",rusqlite::params![account_id,next],|r|r.get(0))?;
        if document {
            if !crate::document_sync::ack_proven(transaction, account_id, next)? {
                break;
            }
            candidate = next;
            continue;
        }
        let map: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_inbox WHERE account_id=?1 AND server_sequence=?2 AND entity_type='map')",rusqlite::params![account_id,next],|r|r.get(0))?;
        if map {
            if !crate::map_sync::ack_proven(transaction, account_id, next)? {
                break;
            }
            candidate = next;
            continue;
        }
        let framed: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_inbox WHERE account_id=?1 AND server_sequence=?2 AND entity_type='note' AND operation='event')",rusqlite::params![account_id,next],|r|r.get(0))?;
        if framed && !crate::content_note_sync::ack_proven(transaction, account_id, next)? {
            break;
        }
        let durable: Option<i64> = transaction.query_row(
            "SELECT CASE
                WHEN inbox.entity_type IN ('stage','stage_order')
                 AND inbox.state IN ('applied','conflict')
                 AND EXISTS(SELECT 1 FROM cloud_sync_structural_apply_ledger l
                   JOIN cloud_sync_structural_events e ON e.account_id=l.account_id AND e.event_id=l.event_id
                   JOIN cloud_sync_event_objects o ON o.account_id=l.account_id AND o.event_id=l.event_id
                   WHERE l.account_id=inbox.account_id AND l.event_id=inbox.event_id
                    AND e.project_id=inbox.project_id AND e.entity_type=inbox.entity_type AND e.entity_id=inbox.entity_id
                    AND e.server_sequence=inbox.server_sequence AND l.server_sequence=inbox.server_sequence
                    AND e.state=l.outcome AND l.outcome=CASE WHEN inbox.state='conflict' THEN 'conflict_preserved' ELSE 'applied' END
                    AND o.nonce=l.nonce AND o.ciphertext=l.ciphertext AND o.crypto_version=1 AND o.aad_version=1) THEN 1
                WHEN inbox.entity_type IN ('stage','stage_order') THEN 0
                WHEN inbox.entity_type='project_metadata'
                 AND inbox.state IN ('applied','conflict')
                 AND EXISTS(
                    SELECT 1 FROM cloud_sync_metadata_apply_ledger AS ledger
                    JOIN cloud_sync_metadata_events AS event
                      ON event.account_id=ledger.account_id AND event.event_id=ledger.event_id
                    WHERE ledger.account_id=inbox.account_id AND ledger.event_id=inbox.event_id
                      AND ledger.project_id=inbox.project_id
                      AND ledger.outcome=CASE WHEN inbox.state='conflict' THEN 'conflict_preserved' ELSE 'applied' END
                      AND event.project_id=inbox.project_id
                      AND event.server_sequence=inbox.server_sequence
                      AND event.state=ledger.outcome
                 ) THEN 1
                WHEN inbox.entity_type='project_metadata' THEN 0
                WHEN inbox.state='applied' THEN 1
                WHEN inbox.state='conflict_preserved'
                 AND inbox.conflict_preserved_at IS NOT NULL
                 AND EXISTS(
                    SELECT 1 FROM cloud_sync_note_conflict_groups AS conflict_group
                    JOIN cloud_sync_note_conflict_versions AS remote_version
                      ON remote_version.group_id=conflict_group.group_id
                     AND remote_version.account_id=inbox.account_id
                     AND remote_version.event_id=inbox.event_id
                     AND remote_version.source='remote'
                    WHERE conflict_group.group_id=inbox.conflict_group_id
                      AND conflict_group.account_id=inbox.account_id
                      AND conflict_group.project_id=inbox.project_id
                      AND conflict_group.entity_id=inbox.entity_id
                      AND conflict_group.entity_type=inbox.entity_type
                      AND (SELECT count(*) FROM cloud_sync_note_conflict_tips AS tip
                           WHERE tip.group_id=conflict_group.group_id) >= 2
                 ) THEN 1 ELSE 0 END
             FROM cloud_sync_inbox AS inbox
             WHERE inbox.account_id=?1 AND inbox.server_sequence=?2",
            rusqlite::params![account_id, next], |row| row.get(0),
        ).optional()?;
        if durable != Some(1) {
            break;
        }
        candidate = next;
    }
    Ok(candidate)
}

/// Reads the largest contiguous, durably applied inbound prefix. It neither
/// changes SQLite state nor proves a remote transport acknowledgement.
pub(crate) fn prepare_note_sync_ack(
    connection: &mut Connection,
    command: &PrepareNoteSyncAckCommand,
) -> Result<NoteSyncAckCandidate, NoteSyncError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    validate_pull_scope(
        &transaction,
        &command.account_id,
        &command.device_id,
        &command.canonical_user_id,
    )?;
    let (pull_cursor, ack_cursor): (i64, i64) = transaction.query_row(
        "SELECT pull_cursor,ack_cursor FROM cloud_sync_state WHERE account_id=?1",
        [&command.account_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let candidate_cursor =
        contiguous_applied_ack_prefix(&transaction, &command.account_id, ack_cursor, pull_cursor)?;
    transaction.commit()?;
    Ok(NoteSyncAckCandidate {
        current_ack_cursor: ack_cursor,
        candidate_cursor,
    })
}

/// Records a previously confirmed remote ACK only if the local proof is still
/// valid. Transport remains outside this Rust/SQLite substrate.
pub(crate) fn commit_note_sync_ack(
    connection: &mut Connection,
    command: &CommitNoteSyncAckCommand,
) -> Result<CommitNoteSyncAckResult, NoteSyncError> {
    if !(0..=MAX_SYNC_INTEGER).contains(&command.expected_old_ack_cursor)
        || !(0..=MAX_SYNC_INTEGER).contains(&command.acknowledged_cursor)
        || command.acknowledged_cursor < command.expected_old_ack_cursor
    {
        return Err(NoteSyncError::InvalidEnvelope(
            "invalid ACK advancement command",
        ));
    }
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    validate_pull_scope(
        &transaction,
        &command.account_id,
        &command.device_id,
        &command.canonical_user_id,
    )?;
    let (pull_cursor, current_ack_cursor): (i64, i64) = transaction.query_row(
        "SELECT pull_cursor,ack_cursor FROM cloud_sync_state WHERE account_id=?1",
        [&command.account_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if command.acknowledged_cursor > pull_cursor {
        return Err(NoteSyncError::InvalidEnvelope("ACK exceeds pull cursor"));
    }
    if current_ack_cursor == command.acknowledged_cursor {
        transaction.commit()?;
        return Ok(CommitNoteSyncAckResult::AlreadyAcknowledged);
    }
    if current_ack_cursor > command.acknowledged_cursor {
        transaction.commit()?;
        return Ok(CommitNoteSyncAckResult::AlreadyAdvanced);
    }
    if current_ack_cursor != command.expected_old_ack_cursor {
        transaction.commit()?;
        return Ok(CommitNoteSyncAckResult::Stale);
    }
    let safe_candidate = contiguous_applied_ack_prefix(
        &transaction,
        &command.account_id,
        current_ack_cursor,
        pull_cursor,
    )?;
    if safe_candidate < command.acknowledged_cursor {
        return Err(NoteSyncError::InvalidEnvelope(
            "ACK skips unresolved inbox event",
        ));
    }
    let updated = transaction.execute(
        "UPDATE cloud_sync_state SET ack_cursor=?1,updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE account_id=?2 AND ack_cursor=?3",
        rusqlite::params![command.acknowledged_cursor, command.account_id, current_ack_cursor],
    )?;
    if updated != 1 {
        return Err(NoteSyncError::InvalidEnvelope(
            "ACK cursor compare-and-swap failed",
        ));
    }
    transaction.commit()?;
    Ok(CommitNoteSyncAckResult::Advanced)
}

/// Lists one bounded page of durable received Note events.  Scope validation is
/// intentionally repeated here because this is a separate IPC boundary from
/// pull receipt.  No transaction is retained while TypeScript decrypts bytes.
fn list_note_sync_inbox_items(
    connection: &Connection,
    account_id: &str,
    device_id: &str,
    canonical_user_id: &str,
    limit: u32,
    after_server_sequence: Option<i64>,
    predicate: &'static str,
    allowed_operations: &[&str],
) -> Result<Vec<ReceivedNoteSyncInboxItem>, NoteSyncError> {
    if !(1..=MAX_RECEIVED_INBOX_LIST_LIMIT).contains(&limit)
        || after_server_sequence.is_some_and(|value| !(0..=MAX_SYNC_INTEGER).contains(&value))
    {
        return Err(NoteSyncError::InvalidListLimit);
    }
    // Historical v1/resolution paging stays stable. Only the newly admitted
    // content Note stream needs durable rotation across bounded cycles.
    let rotate = allowed_operations == ["event"];
    let transaction = connection.unchecked_transaction()?;
    validate_pull_scope(&transaction, account_id, device_id, canonical_user_id)?;
    let query = format!(
        "SELECT inbox.event_id,inbox.server_sequence,inbox.device_id,inbox.project_id,
                inbox.entity_id,inbox.entity_type,inbox.operation,inbox.sync_revision,
                inbox.updated_at,inbox.deleted_at,object.crypto_version,object.aad_version,
                object.nonce,object.ciphertext
         FROM cloud_sync_inbox AS inbox
         LEFT JOIN cloud_sync_event_objects AS object
           ON object.account_id=inbox.account_id AND object.event_id=inbox.event_id
         LEFT JOIN cloud_game_reader_visits AS visits
           ON visits.account_id=inbox.account_id AND visits.event_id=inbox.event_id
         WHERE inbox.account_id=?1 AND inbox.entity_type='note' {predicate}
           AND (?2 IS NULL OR inbox.server_sequence>?2)
         ORDER BY CASE WHEN ?4 THEN COALESCE(visits.ordinal,0) ELSE 0 END,inbox.server_sequence ASC LIMIT ?3",
    );
    let mut statement = transaction.prepare(&query)?;
    let rows = statement.query_map(
        rusqlite::params![account_id, after_server_sequence, limit, rotate],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, Option<String>>(9)?,
                row.get::<_, Option<i64>>(10)?,
                row.get::<_, Option<i64>>(11)?,
                row.get::<_, Option<Vec<u8>>>(12)?,
                row.get::<_, Option<Vec<u8>>>(13)?,
            ))
        },
    )?;
    let mut items = Vec::new();
    let mut aggregate = 0usize;
    for row in rows {
        let (
            event_id,
            server_sequence,
            source_device_id,
            project_id,
            entity_id,
            entity_type,
            operation,
            revision,
            updated_at,
            deleted_at,
            crypto_version,
            aad_version,
            nonce,
            ciphertext,
        ) = row?;
        let (crypto_version, aad_version, nonce, ciphertext) =
            match (crypto_version, aad_version, nonce, ciphertext) {
                (Some(version), Some(aad), Some(nonce), Some(ciphertext)) => {
                    (version, aad, nonce, ciphertext)
                }
                _ => return Err(NoteSyncError::SealedObjectMissing),
            };
        let envelope = decode_encrypted_note_sync_envelope(&EncryptedNoteSyncEnvelope {
            crypto_version,
            aad_version,
            nonce: encode_canonical_base64url(&nonce),
            ciphertext: encode_canonical_base64url(&ciphertext),
        })?;
        aggregate = aggregate
            .checked_add(envelope.ciphertext.len())
            .ok_or(NoteSyncError::InvalidEnvelope("inbox object overflow"))?;
        if aggregate > MAX_ENCRYPTED_SYNC_BATCH_CIPHERTEXT_BYTES {
            return Err(NoteSyncError::InvalidEnvelope("inbox aggregate too large"));
        }
        if !allowed_operations.contains(&operation.as_str()) {
            return Err(NoteSyncError::InvalidEnvelope("unsupported Note operation"));
        }
        items.push(ReceivedNoteSyncInboxItem {
            event_id,
            server_sequence,
            source_device_id,
            project_id,
            entity_id,
            entity_type,
            operation,
            revision,
            updated_at,
            deleted_at,
            envelope: StoredEncryptedNoteSyncEnvelope {
                crypto_version: envelope.crypto_version,
                aad_version: envelope.aad_version,
                nonce: encode_canonical_base64url(&envelope.nonce),
                ciphertext: encode_canonical_base64url(&envelope.ciphertext),
            },
        });
    }
    drop(statement);
    items.sort_by_key(|item| item.server_sequence);
    if rotate {
        crate::sqlite::record_sync_reader_visits(
            &transaction,
            account_id,
            items.iter().map(|item| item.event_id.as_str()),
        )?;
    }
    transaction.commit()?;
    Ok(items)
}

/// Lists one bounded page of durable received Note events. This legacy mixed
/// reader remains compatible for the v1 decryptor until D4A2 schedules the
/// independently paginated streams.
pub(crate) fn list_received_note_sync_inbox(
    connection: &Connection,
    command: &ListReceivedNoteSyncInboxCommand,
) -> Result<Vec<ReceivedNoteSyncInboxItem>, NoteSyncError> {
    list_note_sync_inbox_items(
        connection,
        &command.account_id,
        &command.device_id,
        &command.canonical_user_id,
        command.limit,
        None,
        "AND inbox.state='received'",
        &["upsert", "delete", "resolution"],
    )
}

/// Lists one bounded page from either received-v1 or received-resolution
/// stream. It never changes durable inbox state or cursors.
pub(crate) fn list_received_note_sync_inbox_page(
    connection: &Connection,
    command: &ListReceivedNoteSyncInboxPageCommand,
) -> Result<Vec<ReceivedNoteSyncInboxItem>, NoteSyncError> {
    let (predicate, allowed_operations): (&str, &[&str]) = match command.kind {
        ReceivedNoteSyncInboxPageKind::V1 => (
            "AND inbox.state='received' AND inbox.operation IN ('upsert','delete')",
            &["upsert", "delete"],
        ),
        ReceivedNoteSyncInboxPageKind::Content => (
            "AND inbox.state IN ('received','orphan') AND inbox.operation='event'",
            &["event"],
        ),
        ReceivedNoteSyncInboxPageKind::Resolution => (
            "AND inbox.state='received' AND inbox.operation='resolution'",
            &["resolution"],
        ),
    };
    list_note_sync_inbox_items(
        connection,
        &command.account_id,
        &command.device_id,
        &command.canonical_user_id,
        command.limit,
        Some(command.after_server_sequence),
        predicate,
        allowed_operations,
    )
}

/// Lists one bounded keyset page of durable orphaned v2 Note resolutions.
/// Scope validation is repeated at this IPC boundary. The read-only
/// transaction ensures retry discovery cannot acknowledge or alter an orphan.
pub(crate) fn list_orphan_note_resolution_inbox(
    connection: &Connection,
    command: &ListOrphanNoteResolutionInboxCommand,
) -> Result<Vec<ReceivedNoteSyncInboxItem>, NoteSyncError> {
    list_note_sync_inbox_items(
        connection,
        &command.account_id,
        &command.device_id,
        &command.canonical_user_id,
        command.limit,
        Some(command.after_server_sequence),
        "AND inbox.state='orphan' AND inbox.operation='resolution'",
        &["resolution"],
    )
}

pub(crate) fn commit_note_sync_inbound_page(
    connection: &mut Connection,
    command: &CommitNoteSyncInboundPageCommand,
) -> Result<CommitNoteSyncInboundPageResult, NoteSyncError> {
    commit_encrypted_sync_inbound_page(connection, &command.into(), false)
}

pub(crate) fn commit_v3_sync_inbound_page(
    connection: &mut Connection,
    command: &CommitNoteSyncInboundPageCommand,
) -> Result<CommitNoteSyncInboundPageResult, NoteSyncError> {
    commit_encrypted_sync_inbound_page(connection, &command.into(), true)
}

pub(crate) fn commit_mixed_sync_inbound_page(
    connection: &mut Connection,
    command: &CommitMixedSyncInboundPageCommand,
) -> Result<CommitNoteSyncInboundPageResult, NoteSyncError> {
    commit_encrypted_sync_inbound_page(connection, command, true)
}

fn commit_encrypted_sync_inbound_page(
    connection: &mut Connection,
    command: &CommitMixedSyncInboundPageCommand,
    allow_metadata: bool,
) -> Result<CommitNoteSyncInboundPageResult, NoteSyncError> {
    if command.items.len() > 200
        || !(0..=MAX_SYNC_INTEGER).contains(&command.expected_cursor)
        || !(0..=MAX_SYNC_INTEGER).contains(&command.next_cursor)
    {
        return Err(NoteSyncError::InvalidEnvelope(
            "invalid inbox cursor or page size",
        ));
    }
    if command.items.is_empty()
        && (command.next_cursor != command.expected_cursor || command.has_more)
    {
        return Err(NoteSyncError::InvalidEnvelope("invalid empty inbox page"));
    }
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    validate_pull_scope(
        &transaction,
        &command.account_id,
        &command.device_id,
        &command.canonical_user_id,
    )?;
    let current: i64 = transaction.query_row(
        "SELECT pull_cursor FROM cloud_sync_state WHERE account_id=?1",
        [&command.account_id],
        |row| row.get(0),
    )?;
    // A lost IPC result may replay the exact page after its transaction
    // committed.  Permit only that non-regressing shape; every item is still
    // compared below and any missing/conflicting row rolls the transaction back.
    let exact_replay = current == command.next_cursor && current > command.expected_cursor;
    if current != command.expected_cursor && !exact_replay {
        return Err(NoteSyncError::InvalidEnvelope("stale pull cursor"));
    }
    if exact_replay {
        let stored_page_events: i64 = transaction.query_row(
            "SELECT (SELECT COUNT(*) FROM cloud_sync_inbox WHERE account_id=?1 AND server_sequence>?2 AND server_sequence<=?3) +
             (SELECT COUNT(*) FROM cloud_sync_account_inbox WHERE account_id=?1 AND server_sequence>?2 AND server_sequence<=?3)",
            rusqlite::params![command.account_id, command.expected_cursor, command.next_cursor],
            |row| row.get(0),
        )?;
        if stored_page_events != command.items.len() as i64 {
            return Err(NoteSyncError::InvalidEnvelope("incomplete inbox replay"));
        }
    }
    let mut previous = if exact_replay {
        0
    } else {
        command.expected_cursor
    };
    let mut aggregate = 0usize;
    let mut new_events = 0u32;
    let mut replayed_events = 0u32;
    for incoming in &command.items {
        let item = match incoming {
            InboundSyncItem::Project(item) => item,
            InboundSyncItem::Account(item) => {
                if !allow_metadata {
                    return Err(NoteSyncError::InvalidEnvelope(
                        "account requires mixed mode-3 inbox",
                    ));
                }
                if item.server_sequence <= previous
                    || item.server_sequence > MAX_SYNC_INTEGER
                    || !valid_note_sync_timestamp(&item.updated_at)
                    || item
                        .deleted_at
                        .as_deref()
                        .is_some_and(|value| !valid_note_sync_timestamp(value))
                {
                    return Err(NoteSyncError::InvalidEnvelope(
                        "invalid account sequence or timestamp",
                    ));
                }
                let (replay, size) =
                    crate::account_sync::preserve(&transaction, command, item, exact_replay)?;
                aggregate += size;
                if aggregate > MAX_ENCRYPTED_SYNC_BATCH_CIPHERTEXT_BYTES {
                    return Err(NoteSyncError::InvalidEnvelope("inbox aggregate too large"));
                }
                if replay {
                    replayed_events += 1;
                } else {
                    new_events += 1;
                }
                previous = item.server_sequence;
                continue;
            }
        };
        if item.server_sequence <= previous
            || item.server_sequence > MAX_SYNC_INTEGER
            || !valid_inbound_identity(&item.event_id, 36)
            || item.source_device_id.len() != 36
            || !valid_inbound_identity(&item.project_id, 512)
            || !valid_inbound_identity(&item.entity_id, 512)
            || !valid_inbound_identity(&item.entity_type, 128)
            || !(1..=MAX_SYNC_INTEGER).contains(&item.revision)
            || !valid_note_sync_timestamp(&item.updated_at)
            || item
                .deleted_at
                .as_deref()
                .is_some_and(|value| !valid_note_sync_timestamp(value))
            || !matches!(
                item.operation.as_str(),
                "upsert" | "delete" | "event" | "resolution"
            )
            || (item.operation == "delete") != item.deleted_at.is_some()
        {
            return Err(NoteSyncError::InvalidEnvelope("invalid inbound event"));
        }
        if item.operation == "resolution" && (item.entity_type != "note" || item.revision < 2) {
            return Err(NoteSyncError::InvalidEnvelope("invalid inbound resolution"));
        }
        if allow_metadata {
            if !matches!(
                item.entity_type.as_str(),
                "note"
                    | "project_metadata"
                    | "stage"
                    | "stage_order"
                    | "map"
                    | "document"
                    | "progress"
                    | "project_game"
            ) || item.entity_type == "project_metadata"
                && (item.entity_id != item.project_id || item.operation == "event")
            {
                return Err(NoteSyncError::InvalidEnvelope("unsupported mode-3 entity"));
            }
        } else if matches!(
            item.entity_type.as_str(),
            "project_metadata"
                | "stage"
                | "stage_order"
                | "map"
                | "document"
                | "progress"
                | "project_game"
        ) || item.entity_type == "note" && item.operation == "event"
        {
            return Err(NoteSyncError::InvalidEnvelope(
                "metadata requires mode-3 inbox",
            ));
        }
        if item.entity_type == "map"
            && (item.operation != "event"
                || item.deleted_at.is_some()
                || !(item.entity_id == "project-map"
                    || item.entity_id.starts_with("stage-map-")
                        && item.entity_id.len() == 74
                        && item.entity_id[10..]
                            .bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))))
        {
            return Err(NoteSyncError::InvalidEnvelope("invalid map descriptor"));
        }
        if matches!(
            item.entity_type.as_str(),
            "document" | "progress" | "project_game"
        ) && (item.operation != "event" || item.deleted_at.is_some())
        {
            return Err(NoteSyncError::InvalidEnvelope(
                "invalid document descriptor",
            ));
        }
        if item.entity_type == "project_game"
            && !(item.entity_id == format!("game:project:{}", item.event_id)
                || item.entity_id.starts_with("game:stage:")
                    && item.entity_id.ends_with(&format!(":{}", item.event_id))
                    && item.entity_id.len() > 48)
        {
            return Err(NoteSyncError::InvalidEnvelope("invalid game descriptor"));
        }
        if item.entity_type == "stage_order"
            && (item.entity_id != "stage_order" || item.operation != "upsert")
            || item.entity_type == "stage"
                && !matches!(item.operation.as_str(), "upsert" | "delete")
        {
            return Err(NoteSyncError::InvalidEnvelope(
                "unsupported structural descriptor",
            ));
        }
        if matches!(
            item.entity_type.as_str(),
            "note"
                | "project_metadata"
                | "stage"
                | "stage_order"
                | "map"
                | "document"
                | "progress"
                | "project_game"
        ) && item.envelope.is_none()
        {
            return Err(NoteSyncError::InvalidEnvelope("note object missing"));
        }
        if crate::account_sync::TYPES.contains(&item.entity_type.as_str()) {
            return Err(NoteSyncError::InvalidEnvelope(
                "account object on project inbox",
            ));
        }
        let decoded = item
            .envelope
            .as_ref()
            .map(decode_encrypted_note_sync_envelope)
            .transpose()?;
        if let Some(envelope) = &decoded {
            aggregate = aggregate
                .checked_add(envelope.ciphertext.len())
                .ok_or(NoteSyncError::InvalidEnvelope("inbox object overflow"))?;
            if aggregate > MAX_ENCRYPTED_SYNC_BATCH_CIPHERTEXT_BYTES {
                return Err(NoteSyncError::InvalidEnvelope("inbox aggregate too large"));
            }
        }
        let existing: Option<(i64, String, String, String, String, String, i64, String, Option<String>)> = transaction.query_row(
            "SELECT server_sequence,device_id,project_id,entity_id,entity_type,operation,sync_revision,updated_at,deleted_at FROM cloud_sync_inbox WHERE account_id=?1 AND event_id=?2",
            rusqlite::params![command.account_id, item.event_id],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?,row.get(8)?)),
        ).optional()?;
        if let Some(row) = existing {
            if row
                != (
                    item.server_sequence,
                    item.source_device_id.clone(),
                    item.project_id.clone(),
                    item.entity_id.clone(),
                    item.entity_type.clone(),
                    item.operation.clone(),
                    item.revision,
                    item.updated_at.clone(),
                    item.deleted_at.clone(),
                )
            {
                return Err(NoteSyncError::InvalidEnvelope("conflicting inbox replay"));
            }
            replayed_events += 1;
        } else {
            if exact_replay {
                return Err(NoteSyncError::InvalidEnvelope("incomplete inbox replay"));
            }
            let state = if matches!(
                item.entity_type.as_str(),
                "note"
                    | "project_metadata"
                    | "stage"
                    | "stage_order"
                    | "map"
                    | "document"
                    | "progress"
                    | "project_game"
            ) {
                "received"
            } else {
                "unknown_entity"
            };
            transaction.execute("INSERT INTO cloud_sync_inbox(account_id,event_id,server_sequence,device_id,project_id,entity_id,entity_type,operation,sync_revision,updated_at,deleted_at,state,received_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
                rusqlite::params![command.account_id,item.event_id,item.server_sequence,item.source_device_id,item.project_id,item.entity_id,item.entity_type,item.operation,item.revision,item.updated_at,item.deleted_at,state])?;
            new_events += 1;
        }
        if let Some(envelope) = decoded {
            let object: Option<(i64,i64,Vec<u8>,Vec<u8>)> = transaction.query_row(
                "SELECT crypto_version,aad_version,nonce,ciphertext FROM cloud_sync_event_objects WHERE account_id=?1 AND event_id=?2",
                rusqlite::params![command.account_id,item.event_id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
            ).optional()?;
            if let Some(object) = object {
                if object
                    != (
                        envelope.crypto_version,
                        envelope.aad_version,
                        envelope.nonce,
                        envelope.ciphertext,
                    )
                {
                    return Err(NoteSyncError::ConflictingEncryptedObject);
                }
            } else {
                transaction.execute("INSERT INTO cloud_sync_event_objects(account_id,event_id,crypto_version,aad_version,nonce,ciphertext,stored_at) VALUES(?1,?2,?3,?4,?5,?6,strftime('%Y-%m-%dT%H:%M:%fZ','now'))", rusqlite::params![command.account_id,item.event_id,envelope.crypto_version,envelope.aad_version,envelope.nonce,envelope.ciphertext])?;
            }
        }
        previous = item.server_sequence;
    }
    if !command.items.is_empty() && command.next_cursor != previous {
        return Err(NoteSyncError::InvalidEnvelope("next cursor mismatch"));
    }
    if !exact_replay {
        transaction.execute("UPDATE cloud_sync_state SET pull_cursor=?1,updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE account_id=?2 AND pull_cursor=?3", rusqlite::params![command.next_cursor,command.account_id,command.expected_cursor])?;
    }
    transaction.commit()?;
    Ok(CommitNoteSyncInboundPageResult {
        committed_cursor: command.next_cursor,
        new_events,
        replayed_events,
        has_more: command.has_more,
    })
}

/// Stores server receipts and transitions exactly their sealed Note events in
/// the same SQLite transaction. A retry of an identical receipt is harmless;
/// any conflicting receipt rolls back the complete batch.
pub(crate) fn commit_note_sync_upload_acceptance(
    connection: &mut Connection,
    command: &CommitNoteSyncUploadAcceptanceCommand,
) -> Result<Vec<CommitNoteSyncUploadAcceptanceResult>, NoteSyncError> {
    if command.account_id.is_empty()
        || command.account_id.len() > 512
        || command.device_id.len() != 36
        || command.receipts.is_empty()
        || command.receipts.len() > MAX_SEALED_OUTBOX_LIST_LIMIT as usize
    {
        return Err(NoteSyncError::InvalidSealState(
            "invalid upload acceptance command",
        ));
    }
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut results = Vec::with_capacity(command.receipts.len());
    for receipt in &command.receipts {
        if receipt.event_id.len() != 36
            || !(1..=MAX_SYNC_INTEGER).contains(&receipt.server_sequence)
        {
            return Err(NoteSyncError::InvalidSealState("invalid upload receipt"));
        }
        let (account_id, device_id, lifecycle): (String, String, String) = transaction.query_row(
            "SELECT account_id,device_id,lifecycle FROM cloud_sync_outbox WHERE event_id=?1 AND entity_type='note'",
            [&receipt.event_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).optional()?.ok_or(NoteSyncError::MissingEvent)?;
        if account_id != command.account_id || device_id != command.device_id {
            return Err(NoteSyncError::ConflictingUploadReceipt);
        }
        let stored: Option<i64> = transaction.query_row(
            "SELECT server_sequence,duplicate FROM cloud_sync_upload_receipts WHERE event_id=?1",
            [&receipt.event_id], |row| row.get(0),
        ).optional()?;
        if let Some(sequence) = stored {
            // `duplicate` describes this HTTP response.  The persisted receipt
            // keeps its first diagnostic value; idempotency is the stable server
            // sequence for this event/account/device identity.
            if sequence != receipt.server_sequence {
                return Err(NoteSyncError::ConflictingUploadReceipt);
            }
            if lifecycle != "accepted" {
                return Err(NoteSyncError::UnexpectedLifecycle);
            }
            results.push(CommitNoteSyncUploadAcceptanceResult::AlreadyAccepted);
            continue;
        }
        if lifecycle != "sealed" {
            return Err(NoteSyncError::UnexpectedLifecycle);
        }
        ensure_consistent_sealed_event(&transaction, &receipt.event_id)?;
        transaction.execute(
            "INSERT INTO cloud_sync_upload_receipts(account_id,event_id,device_id,server_sequence,duplicate,accepted_at)
             VALUES(?1,?2,?3,?4,?5,strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
            rusqlite::params![command.account_id, receipt.event_id, command.device_id,
                receipt.server_sequence, i64::from(receipt.duplicate)],
        )?;
        let changed = transaction.execute(
            "UPDATE cloud_sync_outbox SET lifecycle='accepted'
             WHERE event_id=?1 AND entity_type='note' AND lifecycle='sealed'",
            [&receipt.event_id],
        )?;
        if changed != 1 {
            return Err(NoteSyncError::UnexpectedLifecycle);
        }
        results.push(CommitNoteSyncUploadAcceptanceResult::Accepted);
    }
    transaction.commit()?;
    Ok(results)
}

/// Durably backs off a failed already-sealed upload without touching its opaque
/// object. Permanent protocol failures are retained as rejected evidence.
pub(crate) fn record_note_sync_upload_failure(
    connection: &mut Connection,
    command: &RecordNoteSyncUploadFailureCommand,
) -> Result<(), NoteSyncError> {
    if command.account_id.is_empty()
        || command.account_id.len() > 512
        || command.device_id.len() != 36
        || command.event_ids.is_empty()
        || command.event_ids.len() > MAX_SEALED_OUTBOX_LIST_LIMIT as usize
    {
        return Err(NoteSyncError::InvalidSealState(
            "invalid upload failure command",
        ));
    }
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    for event_id in &command.event_ids {
        if event_id.len() != 36 {
            return Err(NoteSyncError::InvalidSealState(
                "invalid upload failure event",
            ));
        }
        let changed = if command.error_code.retryable() {
            transaction.execute(
                "UPDATE cloud_sync_outbox
                 SET attempt_count=attempt_count+1,last_error=?1,
                     next_attempt_at=strftime('%Y-%m-%dT%H:%M:%fZ','now',printf('+%d seconds',MIN(300,5 * (1 << MIN(6,attempt_count)))))
                 WHERE event_id=?2 AND account_id=?3 AND device_id=?4
                   AND entity_type='note' AND lifecycle='sealed'",
                rusqlite::params![command.error_code.as_str(), event_id, command.account_id, command.device_id],
            )?
        } else {
            transaction.execute(
                "UPDATE cloud_sync_outbox
                 SET attempt_count=attempt_count+1,last_error=?1,next_attempt_at=NULL,lifecycle='rejected'
                 WHERE event_id=?2 AND account_id=?3 AND device_id=?4
                   AND entity_type='note' AND lifecycle='sealed'",
                rusqlite::params![command.error_code.as_str(), event_id, command.account_id, command.device_id],
            )?
        };
        if changed != 1 {
            return Err(NoteSyncError::UnexpectedLifecycle);
        }
    }
    transaction.commit()?;
    Ok(())
}

fn retry_backoff_seconds(attempt_count: i64) -> i64 {
    let exponent = u32::try_from(attempt_count.saturating_sub(1).min(6)).unwrap_or(6);
    5_i64
        .saturating_mul(2_i64.saturating_pow(exponent))
        .min(300)
}

fn ensure_consistent_sealed_event(
    transaction: &Transaction<'_>,
    event_id: &str,
) -> Result<(), NoteSyncError> {
    let (account_id, sidecar_count): (String, i64) = transaction.query_row(
        "SELECT event.account_id,
                (SELECT count(*) FROM cloud_sync_note_intents AS intent
                 WHERE intent.event_id=event.event_id)
         FROM cloud_sync_outbox AS event WHERE event.event_id=?1",
        [event_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if sidecar_count != 0 {
        return Err(NoteSyncError::InvalidSealState(
            "sealed event retains plaintext intent",
        ));
    }
    let object = transaction
        .query_row(
            "SELECT crypto_version,aad_version,length(nonce),length(ciphertext)
             FROM cloud_sync_event_objects WHERE account_id=?1 AND event_id=?2",
            rusqlite::params![account_id, event_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )
        .optional()?
        .ok_or(NoteSyncError::SealedObjectMissing)?;
    if object.0 != SUPPORTED_CRYPTO_VERSION
        || object.1 != SUPPORTED_AAD_VERSION
        || object.2 != 24
        || !(16..=MAX_ENCRYPTED_SYNC_CIPHERTEXT_BYTES as i64).contains(&object.3)
    {
        return Err(NoteSyncError::InvalidSealState(
            "sealed event has an invalid encrypted object",
        ));
    }
    ensure_single_encrypted_object(transaction, event_id)?;
    Ok(())
}

fn ensure_matching_sealed_envelope(
    transaction: &Transaction<'_>,
    account_id: &str,
    event_id: &str,
    envelope: &DecodedEncryptedNoteSyncEnvelope,
) -> Result<(), NoteSyncError> {
    let sidecar_exists = transaction
        .query_row(
            "SELECT 1 FROM cloud_sync_note_intents WHERE event_id=?1",
            [event_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if sidecar_exists {
        return Err(NoteSyncError::InvalidSealState(
            "sealed event retains plaintext intent",
        ));
    }
    let stored = transaction
        .query_row(
            "SELECT crypto_version,aad_version,nonce,ciphertext
             FROM cloud_sync_event_objects WHERE account_id=?1 AND event_id=?2",
            rusqlite::params![account_id, event_id],
            |row| {
                Ok(DecodedEncryptedNoteSyncEnvelope {
                    crypto_version: row.get(0)?,
                    aad_version: row.get(1)?,
                    nonce: row.get(2)?,
                    ciphertext: row.get(3)?,
                })
            },
        )
        .optional()?
        .ok_or(NoteSyncError::SealedObjectMissing)?;
    ensure_single_encrypted_object(transaction, event_id)?;
    if stored != *envelope {
        return Err(NoteSyncError::ConflictingEncryptedObject);
    }
    Ok(())
}

fn ensure_single_encrypted_object(
    transaction: &Transaction<'_>,
    event_id: &str,
) -> Result<(), NoteSyncError> {
    let object_count = transaction.query_row(
        "SELECT count(*) FROM cloud_sync_event_objects WHERE event_id=?1",
        [event_id],
        |row| row.get::<_, i64>(0),
    )?;
    if object_count != 1 {
        return Err(NoteSyncError::ConflictingEncryptedObject);
    }
    Ok(())
}

fn decode_encrypted_note_sync_envelope(
    envelope: &EncryptedNoteSyncEnvelope,
) -> Result<DecodedEncryptedNoteSyncEnvelope, NoteSyncError> {
    if envelope.crypto_version != SUPPORTED_CRYPTO_VERSION {
        return Err(NoteSyncError::InvalidEnvelope("unsupported crypto version"));
    }
    if envelope.aad_version != SUPPORTED_AAD_VERSION {
        return Err(NoteSyncError::InvalidEnvelope("unsupported AAD version"));
    }
    let nonce = decode_canonical_base64url(&envelope.nonce, 24, 24)?;
    let ciphertext = decode_canonical_base64url(
        &envelope.ciphertext,
        16,
        MAX_ENCRYPTED_SYNC_CIPHERTEXT_BYTES,
    )?;
    Ok(DecodedEncryptedNoteSyncEnvelope {
        crypto_version: envelope.crypto_version,
        aad_version: envelope.aad_version,
        nonce,
        ciphertext,
    })
}

pub(crate) fn decode_canonical_base64url(
    input: &str,
    minimum_length: usize,
    maximum_length: usize,
) -> Result<Vec<u8>, NoteSyncError> {
    let maximum_encoded_length = base64url_encoded_length(maximum_length).ok_or(
        NoteSyncError::InvalidEnvelope("encoded payload is too large"),
    )?;
    if input.len() > maximum_encoded_length || input.len() % 4 == 1 {
        return Err(NoteSyncError::InvalidEnvelope("invalid base64url length"));
    }
    let remainder = input.len() % 4;
    let decoded_length = input.len() / 4 * 3
        + match remainder {
            0 => 0,
            2 => 1,
            3 => 2,
            _ => unreachable!(),
        };
    if !(minimum_length..=maximum_length).contains(&decoded_length) {
        return Err(NoteSyncError::InvalidEnvelope(
            "decoded payload length is out of bounds",
        ));
    }
    let bytes = input.as_bytes();
    let mut decoded = Vec::with_capacity(decoded_length);
    let complete_length = input.len() - remainder;
    for chunk in bytes[..complete_length].chunks_exact(4) {
        let a = base64url_value(chunk[0])?;
        let b = base64url_value(chunk[1])?;
        let c = base64url_value(chunk[2])?;
        let d = base64url_value(chunk[3])?;
        decoded.push((a << 2) | (b >> 4));
        decoded.push(((b & 0x0f) << 4) | (c >> 2));
        decoded.push(((c & 0x03) << 6) | d);
    }
    if remainder == 2 {
        let a = base64url_value(bytes[complete_length])?;
        let b = base64url_value(bytes[complete_length + 1])?;
        if b & 0x0f != 0 {
            return Err(NoteSyncError::InvalidEnvelope(
                "non-canonical base64url tail",
            ));
        }
        decoded.push((a << 2) | (b >> 4));
    } else if remainder == 3 {
        let a = base64url_value(bytes[complete_length])?;
        let b = base64url_value(bytes[complete_length + 1])?;
        let c = base64url_value(bytes[complete_length + 2])?;
        if c & 0x03 != 0 {
            return Err(NoteSyncError::InvalidEnvelope(
                "non-canonical base64url tail",
            ));
        }
        decoded.push((a << 2) | (b >> 4));
        decoded.push(((b & 0x0f) << 4) | (c >> 2));
    }
    Ok(decoded)
}

fn base64url_encoded_length(byte_length: usize) -> Option<usize> {
    byte_length
        .checked_div(3)?
        .checked_mul(4)?
        .checked_add(match byte_length % 3 {
            0 => 0,
            1 => 2,
            2 => 3,
            _ => unreachable!(),
        })
}

pub(crate) fn encode_canonical_base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut encoded = String::with_capacity(base64url_encoded_length(bytes.len()).unwrap_or(0));
    let complete_length = bytes.len() / 3 * 3;
    for chunk in bytes[..complete_length].chunks_exact(3) {
        encoded.push(ALPHABET[(chunk[0] >> 2) as usize] as char);
        encoded.push(ALPHABET[(((chunk[0] & 0x03) << 4) | (chunk[1] >> 4)) as usize] as char);
        encoded.push(ALPHABET[(((chunk[1] & 0x0f) << 2) | (chunk[2] >> 6)) as usize] as char);
        encoded.push(ALPHABET[(chunk[2] & 0x3f) as usize] as char);
    }
    match bytes.len() - complete_length {
        1 => {
            encoded.push(ALPHABET[(bytes[complete_length] >> 2) as usize] as char);
            encoded.push(ALPHABET[((bytes[complete_length] & 0x03) << 4) as usize] as char);
        }
        2 => {
            encoded.push(ALPHABET[(bytes[complete_length] >> 2) as usize] as char);
            encoded.push(
                ALPHABET[(((bytes[complete_length] & 0x03) << 4)
                    | (bytes[complete_length + 1] >> 4)) as usize] as char,
            );
            encoded.push(ALPHABET[((bytes[complete_length + 1] & 0x0f) << 2) as usize] as char);
        }
        _ => {}
    }
    encoded
}

fn base64url_value(value: u8) -> Result<u8, NoteSyncError> {
    match value {
        b'A'..=b'Z' => Ok(value - b'A'),
        b'a'..=b'z' => Ok(value - b'a' + 26),
        b'0'..=b'9' => Ok(value - b'0' + 52),
        b'-' => Ok(62),
        b'_' => Ok(63),
        _ => Err(NoteSyncError::InvalidEnvelope(
            "invalid base64url character",
        )),
    }
}

#[derive(Clone, Debug)]
struct VerifiedIncomingNote {
    event_id: String,
    parent_event_id: Option<String>,
    project_id: String,
    entity_id: String,
    operation: String,
    revision: i64,
    updated_at: String,
    deleted_at: Option<String>,
    route: NoteSyncRoute,
    payload_json: Option<String>,
    version_json: String,
}

#[derive(Clone, Debug)]
enum RemoteNoteMutation {
    Insert {
        payload_json: String,
        updated_at: String,
    },
    Update {
        payload_json: String,
        updated_at: String,
    },
    Delete,
    None,
}

#[derive(Clone, Debug)]
struct RemoteApplyPlan {
    result: ApplyVerifiedReceivedNoteResult,
    mutation: RemoteNoteMutation,
    account_id: String,
    project_id: String,
    entity_id: String,
    event_id: String,
    revision: i64,
    updated_at: String,
}

fn note_payload_from_record(
    record: &crate::note_sync_plaintext::NoteSyncRecord,
) -> Result<String, NoteSyncError> {
    let mut payload = serde_json::to_value(record)
        .map_err(|_| NoteSyncError::InvalidSnapshot("could not serialize remote Note"))?;
    payload
        .as_object_mut()
        .ok_or(NoteSyncError::InvalidSnapshot(
            "remote Note payload is not an object",
        ))?
        // This is a local Notes payload field, not the cloud revision.
        .insert("revision".to_string(), serde_json::Value::from(0));
    serde_json::to_string(&payload)
        .map_err(|_| NoteSyncError::InvalidSnapshot("could not encode remote Note payload"))
}

fn verified_incoming_note(
    plaintext: &NoteSyncPlaintext,
) -> Result<VerifiedIncomingNote, NoteSyncError> {
    match plaintext {
        NoteSyncPlaintext::Create { header, note } | NoteSyncPlaintext::Update { header, note } => {
            Ok(VerifiedIncomingNote {
                event_id: header.event_id.clone(),
                parent_event_id: header.parent_event_id.clone(),
                project_id: header.project_id.clone(),
                entity_id: header.entity_id.clone(),
                operation: header.operation.clone(),
                revision: header.revision,
                updated_at: header.updated_at.clone(),
                deleted_at: header.deleted_at.clone(),
                route: note.route.clone(),
                payload_json: Some(note_payload_from_record(note)?),
                version_json: note_payload_from_record(note)?,
            })
        }
        NoteSyncPlaintext::Delete { header, note } => Ok(VerifiedIncomingNote {
            event_id: header.event_id.clone(),
            parent_event_id: header.parent_event_id.clone(),
            project_id: header.project_id.clone(),
            entity_id: header.entity_id.clone(),
            operation: header.operation.clone(),
            revision: header.revision,
            updated_at: header.updated_at.clone(),
            deleted_at: header.deleted_at.clone(),
            route: note.route.clone(),
            payload_json: None,
            version_json: serde_json::to_string(note)
                .map_err(|_| NoteSyncError::InvalidSnapshot("could not encode remote tombstone"))?,
        }),
    }
}

fn update_received_inbox_state(
    transaction: &Transaction<'_>,
    account_id: &str,
    event_id: &str,
    state: &str,
    error_code: Option<&str>,
    applied_at: Option<&str>,
) -> rusqlite::Result<()> {
    let changed = transaction.execute(
        "UPDATE cloud_sync_inbox
         SET state=?1,error_code=?2,applied_at=?3
         WHERE account_id=?4 AND event_id=?5 AND state IN ('received','orphan')",
        rusqlite::params![state, error_code, applied_at, account_id, event_id],
    )?;
    if changed != 1 {
        return Err(rusqlite::Error::QueryReturnedNoRows);
    }
    Ok(())
}

fn advance_remote_entity_head(
    transaction: &Transaction<'_>,
    plan: &RemoteApplyPlan,
) -> rusqlite::Result<()> {
    transaction.execute(
        "INSERT INTO cloud_sync_entities(
            account_id,project_id,entity_id,entity_type,head_event_id,
            head_sync_revision,conflict_event_id,conflict_state,updated_at
         ) VALUES(?1,?2,?3,'note',?4,?5,NULL,NULL,?6)
         ON CONFLICT(account_id,project_id,entity_id,entity_type) DO UPDATE SET
            head_event_id=excluded.head_event_id,
            head_sync_revision=excluded.head_sync_revision,
            conflict_event_id=NULL,conflict_state=NULL,updated_at=excluded.updated_at",
        rusqlite::params![
            plan.account_id,
            plan.project_id,
            plan.entity_id,
            plan.event_id,
            plan.revision,
            plan.updated_at,
        ],
    )?;
    Ok(())
}

fn classify_remote_apply(
    transaction: &Transaction<'_>,
    command: &ApplyVerifiedReceivedNoteCommand,
    result: ApplyVerifiedReceivedNoteResult,
    state: &str,
    error_code: &str,
) -> rusqlite::Result<(Option<OwnedRemoteApplyAuthorization>, RemoteApplyPlan)> {
    update_received_inbox_state(
        transaction,
        &command.account_id,
        &command.event_id,
        state,
        Some(error_code),
        None,
    )?;
    Ok((
        None,
        RemoteApplyPlan {
            result,
            mutation: RemoteNoteMutation::None,
            account_id: command.account_id.clone(),
            project_id: String::new(),
            entity_id: String::new(),
            event_id: command.event_id.clone(),
            revision: 0,
            updated_at: String::new(),
        },
    ))
}

fn update_preserved_conflict_inbox_state(
    transaction: &Transaction<'_>,
    account_id: &str,
    event_id: &str,
    group_id: &str,
) -> rusqlite::Result<()> {
    let changed = transaction.execute(
        "UPDATE cloud_sync_inbox
         SET state='conflict_preserved',error_code='causal_conflict_preserved',
             conflict_group_id=?1,
             conflict_preserved_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE account_id=?2 AND event_id=?3 AND state IN ('received','orphan')",
        rusqlite::params![group_id, account_id, event_id],
    )?;
    if changed != 1 {
        return Err(rusqlite::Error::QueryReturnedNoRows);
    }
    Ok(())
}

fn preserved_remote_version_matches(
    transaction: &Transaction<'_>,
    command: &ApplyVerifiedReceivedNoteCommand,
    incoming: &VerifiedIncomingNote,
) -> rusqlite::Result<bool> {
    transaction
        .query_row(
            "SELECT version.project_id,version.entity_id,version.parent_event_id,
                    version.revision,version.server_sequence,version.operation,
                    version.snapshot_json
             FROM cloud_sync_note_conflict_versions AS version
             JOIN cloud_sync_inbox AS inbox
               ON inbox.conflict_group_id=version.group_id
              AND inbox.account_id=version.account_id
              AND inbox.event_id=version.event_id
             WHERE version.account_id=?1 AND version.event_id=?2
               AND version.source='remote'",
            rusqlite::params![command.account_id, command.event_id],
            |row| {
                Ok(row.get::<_, String>(0)? == incoming.project_id
                    && row.get::<_, String>(1)? == incoming.entity_id
                    && Some(row.get::<_, String>(2)?) == incoming.parent_event_id
                    && row.get::<_, i64>(3)? == incoming.revision
                    && row.get::<_, i64>(4)? == command.server_sequence
                    && row.get::<_, String>(5)? == incoming.operation
                    && row.get::<_, String>(6)? == incoming.version_json)
            },
        )
        .optional()
        .map(|value| value == Some(true))
}

fn record_remote_note_causal_history(
    transaction: &Transaction<'_>,
    command: &ApplyVerifiedReceivedNoteCommand,
    incoming: &VerifiedIncomingNote,
) -> rusqlite::Result<()> {
    transaction.execute(
        "INSERT INTO cloud_sync_note_causal_history(
            account_id,event_id,project_id,entity_id,entity_type,parent_event_id,
            revision,server_sequence,operation,snapshot_json,recorded_at
         ) VALUES(?1,?2,?3,?4,'note',?5,?6,?7,?8,?9,
                  strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        rusqlite::params![
            command.account_id,
            incoming.event_id,
            incoming.project_id,
            incoming.entity_id,
            incoming.parent_event_id,
            incoming.revision,
            command.server_sequence,
            incoming.operation,
            incoming.version_json
        ],
    )?;
    Ok(())
}

fn applied_remote_history_matches(
    transaction: &Transaction<'_>,
    command: &ApplyVerifiedReceivedNoteCommand,
    incoming: &VerifiedIncomingNote,
) -> rusqlite::Result<bool> {
    transaction
        .query_row(
            "SELECT project_id,entity_id,parent_event_id,revision,server_sequence,
                operation,snapshot_json
         FROM cloud_sync_note_causal_history
         WHERE account_id=?1 AND event_id=?2",
            rusqlite::params![command.account_id, command.event_id],
            |row| {
                Ok(row.get::<_, String>(0)? == incoming.project_id
                    && row.get::<_, String>(1)? == incoming.entity_id
                    && row.get::<_, Option<String>>(2)? == incoming.parent_event_id
                    && row.get::<_, i64>(3)? == incoming.revision
                    && row.get::<_, i64>(4)? == command.server_sequence
                    && row.get::<_, String>(5)? == incoming.operation
                    && row.get::<_, String>(6)? == incoming.version_json)
            },
        )
        .optional()
        .map(|value| value == Some(true))
}

fn preserve_causal_note_conflict(
    transaction: &Transaction<'_>,
    command: &ApplyVerifiedReceivedNoteCommand,
    incoming: &VerifiedIncomingNote,
) -> Result<Option<RemoteApplyPlan>, StorageError> {
    let Some(common_parent) = incoming.parent_event_id.as_deref() else {
        return Ok(None);
    };
    if incoming.revision < 2 {
        return Ok(None);
    }
    let resolution_parent = is_resolution_event(transaction, common_parent)
        .map_err(|error| StorageError::RemoteApplyAuthorization(error.to_string()))?;
    if resolution_parent
        && applied_resolution_parent_revision(
            transaction,
            &command.account_id,
            &incoming.project_id,
            &incoming.entity_id,
            common_parent,
        )
        .map_err(|error| StorageError::RemoteApplyAuthorization(error.to_string()))?
        .and_then(|revision| revision.checked_add(1))
            != Some(incoming.revision)
    {
        return Ok(None);
    }

    let open_group: Option<(String, String, i64, i64)> = transaction
        .query_row(
            "SELECT group_id,common_parent_event_id,tip_revision,generation
             FROM cloud_sync_note_conflict_groups
             WHERE account_id=?1 AND project_id=?2 AND entity_id=?3
               AND entity_type='note' AND lifecycle='open'",
            rusqlite::params![command.account_id, incoming.project_id, incoming.entity_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;

    let (group_id, generation) = if let Some((group_id, stored_parent, tip_revision, generation)) =
        open_group
    {
        if stored_parent != common_parent || tip_revision != incoming.revision {
            return Ok(None);
        }
        let generation = generation
            .checked_add(1)
            .filter(|value| *value <= MAX_SYNC_INTEGER)
            .ok_or_else(|| {
                StorageError::RemoteApplyAuthorization("conflict generation overflow".into())
            })?;
        transaction.execute(
            "UPDATE cloud_sync_note_conflict_groups
             SET generation=?1,updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
             WHERE group_id=?2 AND generation=?3 AND lifecycle='open'",
            rusqlite::params![generation, group_id, generation - 1],
        )?;
        (group_id, generation)
    } else {
        let local: Option<(String, String, i64, String, i64, String)> = transaction
            .query_row(
                "SELECT outbox.event_id,outbox.parent_event_id,outbox.revision,
                        outbox.operation,COALESCE(intent.mutation_generation,content.mutation_generation),COALESCE(intent.snapshot_json,content.snapshot_json)
                 FROM cloud_sync_outbox AS outbox
                 LEFT JOIN cloud_sync_note_intents AS intent ON intent.event_id=outbox.event_id
                 LEFT JOIN cloud_content_note_writer_events AS content ON content.event_id=outbox.event_id
                 WHERE outbox.account_id=?1 AND outbox.project_id=?2
                   AND outbox.entity_id=?3 AND outbox.entity_type='note'
                   AND (outbox.lifecycle='unsealed' OR (outbox.lifecycle IN ('sealed','accepted') AND EXISTS(SELECT 1 FROM cloud_content_note_writer_events c WHERE c.event_id=outbox.event_id AND c.account_id=outbox.account_id))) AND outbox.local_ordinal>0
                   AND outbox.parent_event_id=?4 AND outbox.revision=?5 AND outbox.event_id<>?6
                   AND (intent.event_id IS NOT NULL OR content.event_id IS NOT NULL)",
                rusqlite::params![command.account_id, incoming.project_id, incoming.entity_id,common_parent,incoming.revision,incoming.event_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
            )
            .optional()?;
        let parent_known = transaction.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM cloud_sync_entities
                  WHERE account_id=?1 AND project_id=?2 AND entity_id=?3
                    AND entity_type='note' AND head_event_id=?4
                 UNION ALL
                 SELECT 1 FROM cloud_sync_outbox
                  WHERE account_id=?1 AND project_id=?2 AND entity_id=?3
                    AND entity_type='note' AND event_id=?4
                    AND lifecycle IN ('sealed','accepted')
                 UNION ALL
                 SELECT 1 FROM cloud_sync_note_causal_history
                  WHERE account_id=?1 AND project_id=?2 AND entity_id=?3
                    AND entity_type='note' AND event_id=?4
             )",
            rusqlite::params![
                command.account_id,
                incoming.project_id,
                incoming.entity_id,
                common_parent
            ],
            |row| row.get::<_, i64>(0),
        )? != 0;
        if !parent_known && !resolution_parent {
            return Ok(None);
        }

        let (
            local_event_id,
            local_revision,
            local_operation,
            local_snapshot,
            local_source,
            local_server_sequence,
            mutation_generation,
        ) = if let Some((
            local_event_id,
            local_parent,
            local_revision,
            local_operation,
            mutation_generation,
            local_snapshot,
        )) = local
        {
            if local_event_id == incoming.event_id
                || local_parent != common_parent
                || local_revision != incoming.revision
            {
                return Ok(None);
            }
            (
                local_event_id,
                local_revision,
                local_operation,
                local_snapshot,
                "local_unsealed",
                None,
                Some(mutation_generation),
            )
        } else {
            let applied: Option<(String, Option<String>, i64, i64, String, String)> = transaction
                .query_row(
                    "SELECT history.event_id,history.parent_event_id,history.revision,
                                history.server_sequence,history.operation,history.snapshot_json
                         FROM cloud_sync_entities AS entity
                         JOIN cloud_sync_note_causal_history AS history
                           ON history.account_id=entity.account_id
                          AND history.event_id=entity.head_event_id
                         WHERE entity.account_id=?1 AND entity.project_id=?2
                           AND entity.entity_id=?3 AND entity.entity_type='note'",
                    rusqlite::params![command.account_id, incoming.project_id, incoming.entity_id],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                            row.get(5)?,
                        ))
                    },
                )
                .optional()?;
            let Some((event_id, parent, revision, server_sequence, operation, snapshot)) = applied
            else {
                return Ok(None);
            };
            if event_id == incoming.event_id
                || parent.as_deref() != Some(common_parent)
                || revision != incoming.revision
            {
                return Ok(None);
            }
            (
                event_id,
                revision,
                operation,
                snapshot,
                "remote_applied",
                Some(server_sequence),
                None,
            )
        };
        let visible: Option<String> = transaction
            .query_row(
                "SELECT payload_json FROM notes WHERE id=?1 AND project_id=?2",
                rusqlite::params![incoming.entity_id, incoming.project_id],
                |row| row.get(0),
            )
            .optional()?;
        if (local_operation == "upsert"
            && !visible.as_deref().is_some_and(|current| {
                note_snapshot_semantically_matches_visible(&local_snapshot, current, local_source)
            }))
            || (local_operation == "delete" && visible.is_some())
        {
            return Ok(None);
        }

        let group_id = canonical_uuid_v4()
            .map_err(|error| StorageError::RemoteApplyAuthorization(format!("{error:?}")))?;
        transaction.execute(
            "INSERT INTO cloud_sync_note_conflict_groups(
                group_id,account_id,project_id,entity_id,entity_type,
                common_parent_event_id,tip_revision,generation,lifecycle,created_at,updated_at
             ) VALUES(?1,?2,?3,?4,'note',?5,?6,1,'open',
                      strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
            rusqlite::params![
                group_id,
                command.account_id,
                incoming.project_id,
                incoming.entity_id,
                common_parent,
                incoming.revision
            ],
        )?;
        let local_version_id = match mutation_generation {
            Some(value) => format!("local:{}:{local_event_id}:{value}", command.account_id),
            None => format!("applied:{}:{local_event_id}", command.account_id),
        };
        transaction.execute(
            "INSERT INTO cloud_sync_note_conflict_versions(
                version_id,group_id,account_id,project_id,entity_id,entity_type,
                event_id,parent_event_id,revision,server_sequence,operation,snapshot_json,
                source,local_mutation_generation,local_outbox_lifecycle,
                conflict_generation,preserved_at
             ) VALUES(?1,?2,?3,?4,?5,'note',?6,?7,?8,?9,?10,?11,
                      ?12,?13,?14,1,strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
            rusqlite::params![
                local_version_id,
                group_id,
                command.account_id,
                incoming.project_id,
                incoming.entity_id,
                local_event_id,
                common_parent,
                local_revision,
                local_server_sequence,
                local_operation,
                local_snapshot,
                local_source,
                mutation_generation,
                mutation_generation.map(|_| "unsealed")
            ],
        )?;
        transaction.execute(
            "INSERT INTO cloud_sync_note_conflict_tips(group_id,version_id,event_id,generation)
             VALUES(?1,?2,?3,1)",
            rusqlite::params![group_id, local_version_id, local_event_id],
        )?;
        (group_id, 1)
    };

    let remote_version_id = format!("remote:{}:{}", command.account_id, incoming.event_id);
    transaction.execute(
        "INSERT INTO cloud_sync_note_conflict_versions(
            version_id,group_id,account_id,project_id,entity_id,entity_type,
            event_id,parent_event_id,revision,server_sequence,operation,snapshot_json,
            source,local_mutation_generation,local_outbox_lifecycle,
            conflict_generation,preserved_at
         ) VALUES(?1,?2,?3,?4,?5,'note',?6,?7,?8,?9,?10,?11,
                  'remote',NULL,NULL,?12,strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        rusqlite::params![
            remote_version_id,
            group_id,
            command.account_id,
            incoming.project_id,
            incoming.entity_id,
            incoming.event_id,
            common_parent,
            incoming.revision,
            command.server_sequence,
            incoming.operation,
            incoming.version_json,
            generation
        ],
    )?;
    transaction.execute(
        "INSERT INTO cloud_sync_note_conflict_tips(group_id,version_id,event_id,generation)
         VALUES(?1,?2,?3,?4)",
        rusqlite::params![group_id, remote_version_id, incoming.event_id, generation],
    )?;
    update_preserved_conflict_inbox_state(
        transaction,
        &command.account_id,
        &command.event_id,
        &group_id,
    )?;
    Ok(Some(RemoteApplyPlan {
        result: ApplyVerifiedReceivedNoteResult::Conflict,
        mutation: RemoteNoteMutation::None,
        account_id: command.account_id.clone(),
        project_id: incoming.project_id.clone(),
        entity_id: incoming.entity_id.clone(),
        event_id: incoming.event_id.clone(),
        revision: incoming.revision,
        updated_at: incoming.updated_at.clone(),
    }))
}

#[derive(Debug)]
struct ResolutionTipProof {
    event_id: String,
    parent_event_id: String,
    revision: i64,
    operation: String,
    snapshot_json: String,
    source: String,
    local_mutation_generation: Option<i64>,
    server_sequence: Option<i64>,
}

#[derive(Debug)]
struct ValidatedResolution {
    resolution: crate::note_sync_plaintext::NoteSyncResolutionV2,
    tips: Vec<ResolutionTipProof>,
    result_operation: &'static str,
}

fn canonical_json(value: &serde_json::Value) -> Result<String, PrepareNoteConflictResolutionError> {
    match value {
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_) => serde_json::to_string(value)
            .map_err(|_| PrepareNoteConflictResolutionError::InvalidPayload),
        serde_json::Value::Array(values) => Ok(format!(
            "[{}]",
            values
                .iter()
                .map(canonical_json)
                .collect::<Result<Vec<_>, _>>()?
                .join(",")
        )),
        serde_json::Value::Object(values) => {
            let mut keys: Vec<&String> = values.keys().collect();
            keys.sort();
            let encoded = keys
                .into_iter()
                .map(|key| {
                    Ok(format!(
                        "{}:{}",
                        serde_json::to_string(key)
                            .map_err(|_| PrepareNoteConflictResolutionError::InvalidPayload)?,
                        canonical_json(&values[key])?
                    ))
                })
                .collect::<Result<Vec<_>, PrepareNoteConflictResolutionError>>()?;
            Ok(format!("{{{}}}", encoded.join(",")))
        }
    }
}

fn wire_snapshot(
    snapshot_json: &str,
    operation: &str,
) -> Result<serde_json::Value, PrepareNoteConflictResolutionError> {
    let mut value: serde_json::Value = serde_json::from_str(snapshot_json)
        .map_err(|_| PrepareNoteConflictResolutionError::MissingCausalProof)?;
    if operation == "upsert" {
        let object = value
            .as_object_mut()
            .ok_or(PrepareNoteConflictResolutionError::MissingCausalProof)?;
        let content = object.get("source_type") == Some(&serde_json::Value::from("project"))
            && (object.get("content_format") == Some(&serde_json::Value::from("plain"))
                || object.get("stage_id").is_some_and(|v| !v.is_null()));
        let revision = object.remove("revision");
        if if content {
            !revision
                .and_then(|v| v.as_i64())
                .is_some_and(|v| (0..=MAX_SYNC_INTEGER).contains(&v))
        } else {
            revision != Some(serde_json::Value::from(0))
        } {
            return Err(PrepareNoteConflictResolutionError::MissingCausalProof);
        }
    }
    if value["source_type"] == "project"
        && (!value["stage_id"].is_null() || value["content_format"] == "plain")
    {
        value = crate::content_note_writer::wire_note(value)
            .map_err(|_| PrepareNoteConflictResolutionError::MissingCausalProof)?;
    }
    Ok(value)
}

fn result_parts(
    result: &NoteSyncResolutionV2Result,
) -> Result<(&'static str, serde_json::Value), PrepareNoteConflictResolutionError> {
    match result {
        NoteSyncResolutionV2Result::Upsert(note) => Ok((
            "upsert",
            serde_json::to_value(note)
                .map_err(|_| PrepareNoteConflictResolutionError::InvalidPayload)?,
        )),
        NoteSyncResolutionV2Result::Delete(note) => Ok((
            "delete",
            serde_json::to_value(note)
                .map_err(|_| PrepareNoteConflictResolutionError::InvalidPayload)?,
        )),
    }
}

fn decode_canonical_resolution(
    command: &PrepareNoteConflictResolutionCommand,
) -> Result<crate::note_sync_plaintext::NoteSyncResolutionV2, PrepareNoteConflictResolutionError> {
    decode_canonical_resolution_bytes(&command.canonical_payload)
}

fn decode_canonical_resolution_bytes(
    canonical_payload: &[u8],
) -> Result<crate::note_sync_plaintext::NoteSyncResolutionV2, PrepareNoteConflictResolutionError> {
    let value: serde_json::Value = serde_json::from_slice(canonical_payload)
        .map_err(|_| PrepareNoteConflictResolutionError::InvalidPayload)?;
    if canonical_json(&value)?.as_bytes() != canonical_payload {
        return Err(PrepareNoteConflictResolutionError::InvalidPayload);
    }
    decode_note_sync_resolution_v2(canonical_payload)
        .map_err(|_| PrepareNoteConflictResolutionError::InvalidPayload)
}

fn validate_resolution_transaction(
    transaction: &Transaction<'_>,
    command: &PrepareNoteConflictResolutionCommand,
    resolution: crate::note_sync_plaintext::NoteSyncResolutionV2,
) -> Result<ValidatedResolution, PrepareNoteConflictResolutionError> {
    validate_pull_scope(
        transaction,
        &command.account_id,
        &command.device_id,
        &command.canonical_user_id,
    )
    .map_err(|_| PrepareNoteConflictResolutionError::ScopeMismatch)?;
    let project_bound: i64 = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM cloud_sync_project_bindings
         WHERE project_id=?1 AND account_id=?2)",
        rusqlite::params![resolution.header.project_id, command.account_id],
        |row| row.get(0),
    )?;
    if project_bound != 1 {
        return Err(PrepareNoteConflictResolutionError::ScopeMismatch);
    }
    let group: Option<(String, String, String, i64, String, String)> = transaction
        .query_row(
            "SELECT account_id,project_id,entity_id,generation,lifecycle,common_parent_event_id
         FROM cloud_sync_note_conflict_groups WHERE group_id=?1",
            [&resolution.conflict_group_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()?;
    let Some((account, project, entity, generation, lifecycle, common_parent)) = group else {
        return Err(PrepareNoteConflictResolutionError::MissingCausalProof);
    };
    if account != command.account_id
        || project != resolution.header.project_id
        || entity != resolution.header.entity_id
    {
        return Err(PrepareNoteConflictResolutionError::ScopeMismatch);
    }
    if lifecycle != "open" || generation != resolution.conflict_generation {
        return Err(PrepareNoteConflictResolutionError::StaleConflict);
    }
    let mut statement = transaction.prepare(
        "SELECT tip.event_id,version.parent_event_id,version.revision,version.operation,
                version.snapshot_json,version.source,version.local_mutation_generation,
                version.server_sequence
         FROM cloud_sync_note_conflict_tips AS tip
         JOIN cloud_sync_note_conflict_versions AS version
           ON version.version_id=tip.version_id AND version.group_id=tip.group_id
         WHERE tip.group_id=?1 ORDER BY tip.event_id",
    )?;
    let tips = statement
        .query_map([&resolution.conflict_group_id], |row| {
            Ok(ResolutionTipProof {
                event_id: row.get(0)?,
                parent_event_id: row.get(1)?,
                revision: row.get(2)?,
                operation: row.get(3)?,
                snapshot_json: row.get(4)?,
                source: row.get(5)?,
                local_mutation_generation: row.get(6)?,
                server_sequence: row.get(7)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    let actual_ids: Vec<String> = tips.iter().map(|tip| tip.event_id.clone()).collect();
    if actual_ids != resolution.resolved_event_ids {
        return Err(PrepareNoteConflictResolutionError::StaleConflict);
    }
    if tips.iter().any(|tip| tip.parent_event_id != common_parent) {
        return Err(PrepareNoteConflictResolutionError::MissingCausalProof);
    }
    let maximum = tips
        .iter()
        .map(|tip| tip.revision)
        .max()
        .ok_or(PrepareNoteConflictResolutionError::MissingCausalProof)?;
    let parent_known: i64 = transaction.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM cloud_sync_note_causal_history
             WHERE account_id=?1 AND project_id=?2 AND entity_id=?3 AND event_id=?4
            UNION ALL
            SELECT 1 FROM cloud_sync_outbox
             WHERE account_id=?1 AND project_id=?2 AND entity_id=?3 AND event_id=?4
               AND lifecycle IN ('sealed','accepted'))",
        rusqlite::params![command.account_id, project, entity, common_parent],
        |row| row.get(0),
    )?;
    let resolution_parent =
        is_resolution_event(transaction, &common_parent).map_err(|error| match error {
            NoteSyncError::Database(error) => PrepareNoteConflictResolutionError::Database(error),
            _ => PrepareNoteConflictResolutionError::MissingCausalProof,
        })?;
    let resolution_proven = if resolution_parent {
        applied_resolution_parent_revision(
            transaction,
            &command.account_id,
            &project,
            &entity,
            &common_parent,
        )
        .map_err(|error| match error {
            NoteSyncError::Database(error) => PrepareNoteConflictResolutionError::Database(error),
            _ => PrepareNoteConflictResolutionError::MissingCausalProof,
        })?
        .and_then(|revision| revision.checked_add(1))
            == Some(maximum)
    } else {
        false
    };
    if parent_known != 1 && !resolution_proven {
        return Err(PrepareNoteConflictResolutionError::MissingCausalProof);
    }
    if maximum.checked_add(1) != Some(resolution.header.revision) {
        return Err(PrepareNoteConflictResolutionError::MissingCausalProof);
    }
    for tip in tips.iter().filter(|tip| tip.source == "local_unsealed") {
        let state: Option<(String, Option<i64>, Option<String>)> = transaction
            .query_row(
                "SELECT outbox.lifecycle,intent.mutation_generation,intent.snapshot_json
             FROM cloud_sync_outbox AS outbox
             LEFT JOIN cloud_sync_note_intents AS intent ON intent.event_id=outbox.event_id
             WHERE outbox.account_id=?1 AND outbox.event_id=?2",
                rusqlite::params![command.account_id, tip.event_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((outbox_lifecycle, current_generation, current_snapshot)) = state else {
            return Err(PrepareNoteConflictResolutionError::MissingCausalProof);
        };
        if outbox_lifecycle == "unsealed"
            && (current_generation != tip.local_mutation_generation
                || current_snapshot.as_deref() != Some(tip.snapshot_json.as_str()))
        {
            return Err(PrepareNoteConflictResolutionError::StaleConflict);
        }
    }
    let unexpected_unsealed: i64 = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM cloud_sync_outbox
         WHERE account_id=?1 AND project_id=?2 AND entity_id=?3 AND entity_type='note'
           AND lifecycle='unsealed' AND event_id NOT IN (
               SELECT event_id FROM cloud_sync_note_conflict_tips WHERE group_id=?4))",
        rusqlite::params![
            command.account_id,
            project,
            entity,
            resolution.conflict_group_id
        ],
        |row| row.get(0),
    )?;
    if unexpected_unsealed != 0 {
        return Err(PrepareNoteConflictResolutionError::StaleConflict);
    }
    let (result_operation, result_value) = result_parts(&resolution.result)?;
    let find_tip = |event_id: &str| {
        tips.iter()
            .find(|tip| tip.event_id == event_id)
            .ok_or(PrepareNoteConflictResolutionError::MissingCausalProof)
    };
    match &resolution.strategy {
        NoteSyncResolutionV2Strategy::ChooseVersion { selected_event_id } => {
            let tip = find_tip(selected_event_id)?;
            if tip.operation != result_operation
                || wire_snapshot(&tip.snapshot_json, &tip.operation)? != result_value
            {
                return Err(PrepareNoteConflictResolutionError::MissingCausalProof);
            }
        }
        NoteSyncResolutionV2Strategy::ManualMerge => {
            if result_operation != "upsert" {
                return Err(PrepareNoteConflictResolutionError::InvalidPayload);
            }
            if let NoteSyncResolutionV2Result::Upsert(note) = &resolution.result {
                if eligibility(&note.route) != Eligibility::Eligible
                    && !(note.route.source_type == "project"
                        && note.route.source_map_id.is_none()
                        && note.route.source_node_id.is_none()
                        && matches!(note.route.content_format.as_str(), "plain" | "html"))
                {
                    return Err(PrepareNoteConflictResolutionError::InvalidPayload);
                }
            }
        }
        NoteSyncResolutionV2Strategy::Delete => {
            if result_operation != "delete" {
                return Err(PrepareNoteConflictResolutionError::InvalidPayload);
            }
        }
        NoteSyncResolutionV2Strategy::KeepBoth {
            selected_event_id,
            retained_event_id,
            retained_note,
        } => {
            if tips.len() != 2 || tips.iter().any(|tip| tip.operation != "upsert") {
                return Err(PrepareNoteConflictResolutionError::MissingCausalProof);
            }
            let selected = find_tip(selected_event_id)?;
            if wire_snapshot(&selected.snapshot_json, "upsert")? != result_value {
                return Err(PrepareNoteConflictResolutionError::MissingCausalProof);
            }
            let retained = find_tip(retained_event_id)?;
            let mut expected = wire_snapshot(&retained.snapshot_json, "upsert")?;
            let retained_value = serde_json::to_value(retained_note)
                .map_err(|_| PrepareNoteConflictResolutionError::InvalidPayload)?;
            let object = expected
                .as_object_mut()
                .ok_or(PrepareNoteConflictResolutionError::MissingCausalProof)?;
            object.insert(
                "id".into(),
                serde_json::Value::String(retained_note.route.id.clone()),
            );
            object.insert(
                "created_at".into(),
                serde_json::Value::String(retained_note.created_at.clone()),
            );
            object.insert(
                "updated_at".into(),
                serde_json::Value::String(retained_note.updated_at.clone()),
            );
            if expected != retained_value {
                return Err(PrepareNoteConflictResolutionError::MissingCausalProof);
            }
            let collision:i64=transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM notes WHERE id=?1
                 UNION ALL SELECT 1 FROM cloud_sync_entities WHERE project_id=?2 AND entity_id=?1
                 UNION ALL SELECT 1 FROM cloud_sync_outbox WHERE project_id=?2 AND entity_id=?1
                 UNION ALL SELECT 1 FROM cloud_sync_note_resolution_outbox WHERE clone_entity_id=?1)",
                rusqlite::params![retained_note.route.id,project],|row|row.get(0),
            )?;
            if collision != 0 {
                return Err(PrepareNoteConflictResolutionError::ConflictingResolution);
            }
        }
    }
    Ok(ValidatedResolution {
        resolution,
        tips,
        result_operation,
    })
}

pub(crate) fn prepare_note_conflict_resolution(
    connection: &mut Connection,
    command: &PrepareNoteConflictResolutionCommand,
) -> Result<PrepareNoteConflictResolutionResult, PrepareNoteConflictResolutionError> {
    prepare_note_conflict_resolution_inner(connection, command, false)
}

fn prepare_note_conflict_resolution_inner(
    connection: &mut Connection,
    command: &PrepareNoteConflictResolutionCommand,
    inject_failure: bool,
) -> Result<PrepareNoteConflictResolutionResult, PrepareNoteConflictResolutionError> {
    prepare_note_conflict_resolution_with_local(connection, command, inject_failure, None)
}

pub(crate) fn prepare_note_conflict_resolution_with_local(
    connection: &mut Connection,
    command: &PrepareNoteConflictResolutionCommand,
    inject_failure: bool,
    expected_local: Option<&serde_json::Value>,
) -> Result<PrepareNoteConflictResolutionResult, PrepareNoteConflictResolutionError> {
    let resolution = decode_canonical_resolution(command)?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Some(expected) = expected_local {
        let current: Option<String> = transaction
            .query_row(
                "SELECT payload_json FROM notes WHERE id=?1 AND project_id=?2",
                rusqlite::params![resolution.header.entity_id, resolution.header.project_id],
                |r| r.get(0),
            )
            .optional()?;
        let actual = current
            .as_deref()
            .map(serde_json::from_str::<serde_json::Value>)
            .transpose()
            .map_err(|_| PrepareNoteConflictResolutionError::InvalidPayload)?
            .unwrap_or(serde_json::Value::Null);
        if &actual != expected {
            return Err(PrepareNoteConflictResolutionError::StaleConflict);
        }
        transaction.execute("INSERT INTO cloud_content_note_resolution_decisions VALUES(?1,?2) ON CONFLICT(resolution_event_id) DO NOTHING",rusqlite::params![resolution.header.event_id,expected.to_string()])?;
    }
    validate_pull_scope(
        &transaction,
        &command.account_id,
        &command.device_id,
        &command.canonical_user_id,
    )
    .map_err(|_| PrepareNoteConflictResolutionError::ScopeMismatch)?;
    let project_bound: i64 = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM cloud_sync_project_bindings
         WHERE project_id=?1 AND account_id=?2)",
        rusqlite::params![resolution.header.project_id, command.account_id],
        |row| row.get(0),
    )?;
    if project_bound != 1 {
        return Err(PrepareNoteConflictResolutionError::ScopeMismatch);
    }
    let existing: Option<(String, i64, String, String, Vec<u8>)> = transaction.query_row(
        "SELECT conflict_group_id,expected_conflict_generation,account_id,device_id,canonical_payload FROM cloud_sync_note_pending_resolutions WHERE resolution_event_id=?1",
        [&resolution.header.event_id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)),
    ).optional()?;
    let replay = if let Some((group, generation, account, device, payload)) = existing {
        if group != resolution.conflict_group_id
            || generation != resolution.conflict_generation
            || account != command.account_id
            || device != command.device_id
            || payload != command.canonical_payload
        {
            return Err(PrepareNoteConflictResolutionError::ConflictingResolution);
        }
        true
    } else {
        false
    };
    let validated = validate_resolution_transaction(&transaction, command, resolution)?;
    if replay {
        transaction.commit()?;
        return Ok(PrepareNoteConflictResolutionResult::AlreadyPrepared);
    }
    let active:Option<String>=transaction.query_row("SELECT resolution_event_id FROM cloud_sync_note_pending_resolutions WHERE conflict_group_id=?1 AND lifecycle='prepared'",[&validated.resolution.conflict_group_id],|row|row.get(0)).optional()?;
    if let Some(active_id) = active {
        // A fresh rendered decision can supersede only an unapplied codec8 decision
        // whose local compare-and-swap evidence or conflict generation is stale.
        let stale = if let Some(expected) = expected_local {
            let prior:Option<(String,i64)>=transaction.query_row("SELECT d.expected_local_json,p.expected_conflict_generation FROM cloud_content_note_resolution_decisions d JOIN cloud_sync_note_pending_resolutions p ON p.resolution_event_id=d.resolution_event_id WHERE d.resolution_event_id=?1",[&active_id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            prior.is_some_and(|(local, generation)| {
                serde_json::from_str::<serde_json::Value>(&local)
                    .ok()
                    .as_ref()
                    != Some(expected)
                    || generation != validated.resolution.conflict_generation
            })
        } else {
            false
        };
        if !stale {
            return Err(PrepareNoteConflictResolutionError::ConflictingResolution);
        }
        transaction.execute("UPDATE cloud_sync_note_pending_resolutions SET lifecycle='superseded' WHERE resolution_event_id=?1 AND lifecycle='prepared'",[active_id])?;
    }
    let resolution = validated.resolution;
    let tips_json = serde_json::to_string(&resolution.resolved_event_ids)
        .map_err(|_| PrepareNoteConflictResolutionError::InvalidPayload)?;
    transaction.execute("INSERT INTO cloud_sync_note_pending_resolutions(resolution_event_id,account_id,device_id,project_id,entity_id,conflict_group_id,expected_conflict_generation,resolution_revision,tip_event_ids_json,strategy,result_operation,canonical_payload,lifecycle,prepared_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,'prepared',strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",rusqlite::params![resolution.header.event_id,command.account_id,command.device_id,resolution.header.project_id,resolution.header.entity_id,resolution.conflict_group_id,resolution.conflict_generation,resolution.header.revision,tips_json,match resolution.strategy{NoteSyncResolutionV2Strategy::ChooseVersion{..}=>"choose_version",NoteSyncResolutionV2Strategy::ManualMerge=>"manual_merge",NoteSyncResolutionV2Strategy::KeepBoth{..}=>"keep_both",NoteSyncResolutionV2Strategy::Delete=>"delete"},validated.result_operation,command.canonical_payload])?;
    if inject_failure {
        return Err(PrepareNoteConflictResolutionError::InjectedFailure);
    }
    transaction.commit()?;
    Ok(PrepareNoteConflictResolutionResult::Prepared)
}

#[derive(Debug)]
enum ResolutionNoteMutation {
    None,
    Upsert {
        id: String,
        project_id: String,
        stage_id: Option<String>,
        updated_at: String,
        payload_json: String,
        prior_payload_json: Option<String>,
    },
    Delete {
        id: String,
        project_id: String,
        prior_payload_json: String,
    },
}

#[derive(Debug)]
enum ResolutionApplicationPlan {
    Replay,
    Apply {
        validated: ValidatedResolution,
        original: ResolutionNoteMutation,
        clone: ResolutionNoteMutation,
        clone_entity_id: Option<String>,
    },
}

fn resolution_mutation_for_result(
    transaction: &Transaction<'_>,
    result: &NoteSyncResolutionV2Result,
) -> Result<ResolutionNoteMutation, PrepareNoteConflictResolutionError> {
    let (id, project_id) = match result {
        NoteSyncResolutionV2Result::Upsert(note) => {
            (note.route.id.as_str(), note.route.project_id.as_str())
        }
        NoteSyncResolutionV2Result::Delete(note) => {
            (note.route.id.as_str(), note.route.project_id.as_str())
        }
    };
    let current: Option<(String, String)> = transaction
        .query_row(
            "SELECT project_id,payload_json FROM notes WHERE id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if current.as_ref().is_some_and(|value| value.0 != project_id) {
        return Err(PrepareNoteConflictResolutionError::ScopeMismatch);
    }
    match result {
        NoteSyncResolutionV2Result::Upsert(note) => Ok(ResolutionNoteMutation::Upsert {
            id: note.route.id.clone(),
            project_id: note.route.project_id.clone(),
            stage_id: note.route.stage_id.clone(),
            updated_at: note.updated_at.clone(),
            payload_json: note_payload_from_record(note)
                .map_err(|_| PrepareNoteConflictResolutionError::InvalidPayload)?,
            prior_payload_json: current.map(|value| value.1),
        }),
        NoteSyncResolutionV2Result::Delete(_) => Ok(match current {
            Some((_, prior_payload_json)) => ResolutionNoteMutation::Delete {
                id: id.to_string(),
                project_id: project_id.to_string(),
                prior_payload_json,
            },
            None => ResolutionNoteMutation::None,
        }),
    }
}

fn resolution_authorization(
    mutation: &ResolutionNoteMutation,
    event_id: String,
    account_id: &str,
) -> Option<OwnedRemoteApplyAuthorization> {
    match mutation {
        ResolutionNoteMutation::None => None,
        ResolutionNoteMutation::Upsert {
            id,
            project_id,
            payload_json,
            prior_payload_json,
            ..
        } => Some(OwnedRemoteApplyAuthorization {
            event_id,
            account_id: account_id.to_string(),
            project_id: project_id.clone(),
            entity_id: id.clone(),
            operation: "upsert".into(),
            payload_json: Some(payload_json.clone()),
            prior_payload_json: prior_payload_json.clone(),
        }),
        ResolutionNoteMutation::Delete {
            id,
            project_id,
            prior_payload_json,
        } => Some(OwnedRemoteApplyAuthorization {
            event_id,
            account_id: account_id.to_string(),
            project_id: project_id.clone(),
            entity_id: id.clone(),
            operation: "delete".into(),
            payload_json: None,
            prior_payload_json: Some(prior_payload_json.clone()),
        }),
    }
}

fn apply_resolution_note_mutation(
    transaction: &Transaction<'_>,
    mutation: &ResolutionNoteMutation,
) -> rusqlite::Result<()> {
    let changed = match mutation {
        ResolutionNoteMutation::None => return Ok(()),
        ResolutionNoteMutation::Upsert {
            id,
            project_id,
            stage_id,
            updated_at,
            payload_json,
            prior_payload_json,
        } => {
            if prior_payload_json.is_some() {
                transaction.execute(
                    "UPDATE notes SET project_id=?1,stage_id=?2,updated_at=?3,payload_json=?4
                     WHERE id=?5",
                    rusqlite::params![project_id, stage_id, updated_at, payload_json, id],
                )?
            } else {
                transaction.execute(
                    "INSERT INTO notes(id,project_id,stage_id,updated_at,payload_json)
                     VALUES(?1,?2,?3,?4,?5)",
                    rusqlite::params![id, project_id, stage_id, updated_at, payload_json],
                )?
            }
        }
        ResolutionNoteMutation::Delete { id, .. } => {
            transaction.execute("DELETE FROM notes WHERE id=?1", [id])?
        }
    };
    if changed != 1 {
        return Err(rusqlite::Error::QueryReturnedNoRows);
    }
    Ok(())
}

pub(crate) fn apply_prepared_note_conflict_resolution(
    connection: &mut PrivilegedRemoteApplyConnection,
    command: &PrepareNoteConflictResolutionCommand,
) -> Result<ApplyPreparedNoteConflictResolutionResult, PrepareNoteConflictResolutionError> {
    apply_prepared_note_conflict_resolution_inner(connection, command, false)
}

fn apply_prepared_note_conflict_resolution_inner(
    connection: &mut PrivilegedRemoteApplyConnection,
    command: &PrepareNoteConflictResolutionCommand,
    inject_failure_after_original: bool,
) -> Result<ApplyPreparedNoteConflictResolutionResult, PrepareNoteConflictResolutionError> {
    let decoded = decode_canonical_resolution(command)?;
    connection.execute_planned_many_once(
        |transaction| {
            validate_pull_scope(transaction,&command.account_id,&command.device_id,&command.canonical_user_id)
                .map_err(|_|PrepareNoteConflictResolutionError::ScopeMismatch)?;
            let project_bound:i64=transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM cloud_sync_project_bindings
                 WHERE project_id=?1 AND account_id=?2)",
                rusqlite::params![decoded.header.project_id,command.account_id],|row|row.get(0),
            )?;
            if project_bound!=1 { return Err(PrepareNoteConflictResolutionError::ScopeMismatch); }
            let existing:Option<(String,String,String,String,i64,i64,String,Vec<u8>,String)>=transaction.query_row(
                "SELECT account_id,device_id,project_id,entity_id,conflict_generation,revision,
                        conflict_group_id,canonical_payload,lifecycle
                 FROM cloud_sync_note_resolution_outbox WHERE resolution_event_id=?1",
                [&decoded.header.event_id],
                |row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,
                    row.get(5)?,row.get(6)?,row.get(7)?,row.get(8)?)),
            ).optional()?;
            if let Some((account,device,project,entity,generation,revision,group,payload,lifecycle))=existing {
                if account!=command.account_id || device!=command.device_id
                    || project!=decoded.header.project_id || entity!=decoded.header.entity_id
                    || generation!=decoded.conflict_generation || revision!=decoded.header.revision
                    || group!=decoded.conflict_group_id || payload!=command.canonical_payload
                    || lifecycle!="local_pending"
                {
                    return Err(PrepareNoteConflictResolutionError::ConflictingResolution);
                }
                let state:Option<(String,String)>=transaction.query_row(
                    "SELECT pending.lifecycle,conflict.lifecycle
                     FROM cloud_sync_note_pending_resolutions AS pending
                     JOIN cloud_sync_note_conflict_groups AS conflict
                       ON conflict.group_id=pending.conflict_group_id
                     WHERE pending.resolution_event_id=?1",
                    [&decoded.header.event_id],|row|Ok((row.get(0)?,row.get(1)?)),
                ).optional()?;
                if state!=Some(("consumed".into(),"resolving".into())) {
                    return Err(PrepareNoteConflictResolutionError::ConflictingResolution);
                }
                return Ok((Vec::new(),ResolutionApplicationPlan::Replay));
            }
            let expected:Option<String>=transaction.query_row("SELECT expected_local_json FROM cloud_content_note_resolution_decisions WHERE resolution_event_id=?1",[&decoded.header.event_id],|r|r.get(0)).optional()?;
            if let Some(expected)=expected {
                let current:Option<String>=transaction.query_row("SELECT payload_json FROM notes WHERE id=?1 AND project_id=?2",rusqlite::params![decoded.header.entity_id,decoded.header.project_id],|r|r.get(0)).optional()?;
                let actual=current.as_deref().map(serde_json::from_str::<serde_json::Value>).transpose().map_err(|_|PrepareNoteConflictResolutionError::InvalidPayload)?.unwrap_or(serde_json::Value::Null);
                if actual.to_string()!=serde_json::from_str::<serde_json::Value>(&expected).map_err(|_|PrepareNoteConflictResolutionError::InvalidPayload)?.to_string(){return Err(PrepareNoteConflictResolutionError::StaleConflict)}
            }
            let active:Option<String>=transaction.query_row(
                "SELECT resolution_event_id FROM cloud_sync_note_resolution_outbox
                 WHERE conflict_group_id=?1",
                [&decoded.conflict_group_id],|row|row.get(0),
            ).optional()?;
            if active.is_some() { return Err(PrepareNoteConflictResolutionError::ConflictingResolution); }
            let pending:Option<(String,String,String,String,i64,i64,String,String,Vec<u8>,String)>=transaction.query_row(
                "SELECT account_id,device_id,project_id,entity_id,expected_conflict_generation,
                        resolution_revision,tip_event_ids_json,strategy,canonical_payload,lifecycle
                 FROM cloud_sync_note_pending_resolutions WHERE resolution_event_id=?1",
                [&decoded.header.event_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,
                    row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?,row.get(8)?,row.get(9)?)),
            ).optional()?;
            let Some((account,device,project,entity,generation,revision,tips_json,strategy,payload,lifecycle))=pending else {
                return Err(PrepareNoteConflictResolutionError::ConflictingResolution);
            };
            let expected_strategy=match &decoded.strategy {
                NoteSyncResolutionV2Strategy::ChooseVersion{..}=>"choose_version",
                NoteSyncResolutionV2Strategy::ManualMerge=>"manual_merge",
                NoteSyncResolutionV2Strategy::KeepBoth{..}=>"keep_both",
                NoteSyncResolutionV2Strategy::Delete=>"delete",
            };
            let expected_tips=serde_json::to_string(&decoded.resolved_event_ids)
                .map_err(|_|PrepareNoteConflictResolutionError::InvalidPayload)?;
            if account!=command.account_id || device!=command.device_id
                || project!=decoded.header.project_id || entity!=decoded.header.entity_id
                || generation!=decoded.conflict_generation || revision!=decoded.header.revision
                || tips_json!=expected_tips || strategy!=expected_strategy
                || payload!=command.canonical_payload || lifecycle!="prepared"
            {
                return Err(PrepareNoteConflictResolutionError::ConflictingResolution);
            }
            let validated=validate_resolution_transaction(transaction,command,
                decode_canonical_resolution(command)?)?;
            let original=resolution_mutation_for_result(transaction,&validated.resolution.result)?;
            let (clone,clone_entity_id)=match &validated.resolution.strategy {
                NoteSyncResolutionV2Strategy::KeepBoth{retained_note,..}=>{
                    let collision:i64=transaction.query_row(
                        "SELECT EXISTS(SELECT 1 FROM notes WHERE id=?1
                         UNION ALL SELECT 1 FROM cloud_sync_entities WHERE project_id=?2 AND entity_id=?1
                         UNION ALL SELECT 1 FROM cloud_sync_outbox WHERE project_id=?2 AND entity_id=?1
                         UNION ALL SELECT 1 FROM cloud_sync_note_resolution_outbox WHERE clone_entity_id=?1)",
                        rusqlite::params![retained_note.route.id,retained_note.route.project_id],|row|row.get(0),
                    )?;
                    if collision!=0 { return Err(PrepareNoteConflictResolutionError::ConflictingResolution); }
                    let clone_result=NoteSyncResolutionV2Result::Upsert(retained_note.clone());
                    (resolution_mutation_for_result(transaction,&clone_result)?,Some(retained_note.route.id.clone()))
                }
                _=>(ResolutionNoteMutation::None,None),
            };
            let mut authorizations=Vec::new();
            if let Some(value)=resolution_authorization(&original,decoded.header.event_id.clone(),&command.account_id) {
                authorizations.push(value);
            }
            if let Some(value)=resolution_authorization(&clone,format!("{}:clone",decoded.header.event_id),&command.account_id) {
                authorizations.push(value);
            }
            Ok((authorizations,ResolutionApplicationPlan::Apply{
                validated,original,clone,clone_entity_id,
            }))
        },
        |transaction,plan| match plan {
            ResolutionApplicationPlan::Replay=>Ok(ApplyPreparedNoteConflictResolutionResult::AlreadyApplied),
            ResolutionApplicationPlan::Apply{validated,original,clone,clone_entity_id}=>{
                apply_resolution_note_mutation(transaction,&original)?;
                if inject_failure_after_original {
                    return Err(PrepareNoteConflictResolutionError::InjectedFailure);
                }
                apply_resolution_note_mutation(transaction,&clone)?;
                let resolution=&validated.resolution;
                let strategy=match &resolution.strategy {
                    NoteSyncResolutionV2Strategy::ChooseVersion{..}=>"choose_version",
                    NoteSyncResolutionV2Strategy::ManualMerge=>"manual_merge",
                    NoteSyncResolutionV2Strategy::KeepBoth{..}=>"keep_both",
                    NoteSyncResolutionV2Strategy::Delete=>"delete",
                };
                let parents_json=serde_json::to_string(&resolution.resolved_event_ids)
                    .map_err(|_|PrepareNoteConflictResolutionError::InvalidPayload)?;
                transaction.execute(
                    "INSERT INTO cloud_sync_note_resolution_outbox(
                        resolution_event_id,account_id,device_id,project_id,entity_id,
                        clone_entity_id,conflict_group_id,conflict_generation,revision,
                        parent_event_ids_json,strategy,result_operation,canonical_payload,
                        lifecycle,applied_at,updated_at
                     ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,
                              'local_pending',strftime('%Y-%m-%dT%H:%M:%fZ','now'),
                              strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
                    rusqlite::params![resolution.header.event_id,command.account_id,command.device_id,
                        resolution.header.project_id,resolution.header.entity_id,clone_entity_id,
                        resolution.conflict_group_id,resolution.conflict_generation,
                        resolution.header.revision,parents_json,strategy,validated.result_operation,
                        command.canonical_payload],
                )?;
                crate::content_note_writer::wrap_resolution(transaction,&command.account_id,&command.device_id,&command.canonical_payload)
                    .map_err(|_|PrepareNoteConflictResolutionError::InvalidPayload)?;
                for tip in &validated.tips {
                    let (outbox_lifecycle,receipt_sequence)=if tip.source=="local_unsealed" {
                        let value:Option<(String,Option<i64>)>=transaction.query_row(
                            "SELECT outbox.lifecycle,receipt.server_sequence
                             FROM cloud_sync_outbox AS outbox
                             LEFT JOIN cloud_sync_upload_receipts AS receipt
                               ON receipt.account_id=outbox.account_id AND receipt.event_id=outbox.event_id
                             WHERE outbox.account_id=?1 AND outbox.event_id=?2",
                            rusqlite::params![command.account_id,tip.event_id],
                            |row|Ok((row.get(0)?,row.get(1)?)),
                        ).optional()?;
                        value.ok_or(PrepareNoteConflictResolutionError::MissingCausalProof)?
                    } else {(String::new(),None)};
                    transaction.execute(
                        "INSERT INTO cloud_sync_note_resolution_dependencies(
                            resolution_event_id,parent_event_id,source,server_sequence,
                            local_mutation_generation,local_outbox_lifecycle,
                            upload_receipt_sequence,snapshot_json,recorded_at
                         ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,
                                  strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
                        rusqlite::params![resolution.header.event_id,tip.event_id,tip.source,
                            tip.server_sequence,tip.local_mutation_generation,
                            if tip.source=="local_unsealed"{Some(outbox_lifecycle.as_str())}else{None},
                            receipt_sequence,tip.snapshot_json],
                    )?;
                }
                let pending_changed=transaction.execute(
                    "UPDATE cloud_sync_note_pending_resolutions
                     SET lifecycle='consumed',updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
                     WHERE resolution_event_id=?1 AND lifecycle='prepared'",
                    [&resolution.header.event_id],
                )?;
                let group_changed=transaction.execute(
                    "UPDATE cloud_sync_note_conflict_groups
                     SET lifecycle='resolving',updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
                     WHERE group_id=?1 AND generation=?2 AND lifecycle='open'",
                    rusqlite::params![resolution.conflict_group_id,resolution.conflict_generation],
                )?;
                if pending_changed!=1 || group_changed!=1 {
                    return Err(PrepareNoteConflictResolutionError::StaleConflict);
                }
                Ok(ApplyPreparedNoteConflictResolutionResult::Applied)
            }
        },
    )
}

#[derive(Clone)]
struct VerifiedPeerResolutionCommand {
    account_id: String,
    canonical_user_id: String,
    pulling_device_id: String,
    event_id: String,
    server_sequence: i64,
    source_device_id: String,
    crypto_version: i64,
    aad_version: i64,
    nonce: Vec<u8>,
    ciphertext: Vec<u8>,
    canonical_payload: Vec<u8>,
    resolution: crate::note_sync_plaintext::NoteSyncResolutionV2,
}

#[derive(Debug)]
struct PeerResolutionTipProof {
    version_id: String,
    event_id: String,
    parent_event_id: String,
    revision: i64,
    operation: String,
    snapshot_json: String,
    publication_source: String,
    server_sequence: i64,
    local_mutation_generation: Option<i64>,
}

#[derive(Debug)]
enum PeerResolutionApplyPlan {
    Replay,
    SelfEcho,
    Orphan {
        error_code: &'static str,
    },
    Apply {
        local_group_id: String,
        local_generation: i64,
        tips: Vec<PeerResolutionTipProof>,
        strategy: &'static str,
        result_operation: &'static str,
        result_snapshot_json: String,
        clone_entity_id: Option<String>,
        clone_snapshot_json: Option<String>,
        original: ResolutionNoteMutation,
        clone: ResolutionNoteMutation,
    },
}

fn peer_resolution_error(
    error: PrepareNoteConflictResolutionError,
) -> ApplyVerifiedReceivedResolutionV2Error {
    match error {
        PrepareNoteConflictResolutionError::Database(error) => {
            ApplyVerifiedReceivedResolutionV2Error::Database(error)
        }
        PrepareNoteConflictResolutionError::InvalidPayload => {
            ApplyVerifiedReceivedResolutionV2Error::InvalidPayload
        }
        PrepareNoteConflictResolutionError::ScopeMismatch => {
            ApplyVerifiedReceivedResolutionV2Error::ScopeMismatch
        }
        PrepareNoteConflictResolutionError::StaleConflict => {
            ApplyVerifiedReceivedResolutionV2Error::StaleConflict
        }
        PrepareNoteConflictResolutionError::ConflictingResolution => {
            ApplyVerifiedReceivedResolutionV2Error::CloneCollision
        }
        PrepareNoteConflictResolutionError::MissingCausalProof => {
            ApplyVerifiedReceivedResolutionV2Error::MissingCausalProof
        }
        PrepareNoteConflictResolutionError::InjectedFailure => {
            ApplyVerifiedReceivedResolutionV2Error::InjectedFailure
        }
        PrepareNoteConflictResolutionError::ProtectedStorageFailure => {
            ApplyVerifiedReceivedResolutionV2Error::ProtectedStorageFailure
        }
    }
}

fn peer_result_evidence(
    resolution: &crate::note_sync_plaintext::NoteSyncResolutionV2,
    tips: &[PeerResolutionTipProof],
) -> Result<
    (&'static str, String, Option<String>, Option<String>),
    ApplyVerifiedReceivedResolutionV2Error,
> {
    peer_result_evidence_with_content(resolution, tips, false)
}
fn peer_result_evidence_with_content(
    resolution: &crate::note_sync_plaintext::NoteSyncResolutionV2,
    tips: &[PeerResolutionTipProof],
    content: bool,
) -> Result<
    (&'static str, String, Option<String>, Option<String>),
    ApplyVerifiedReceivedResolutionV2Error,
> {
    let (result_operation, result_value) =
        result_parts(&resolution.result).map_err(peer_resolution_error)?;
    let find_tip = |event_id: &str| {
        tips.iter()
            .find(|tip| tip.event_id == event_id)
            .ok_or(ApplyVerifiedReceivedResolutionV2Error::MissingCausalProof)
    };
    let (clone_entity_id, clone_snapshot_json) = match &resolution.strategy {
        NoteSyncResolutionV2Strategy::ChooseVersion { selected_event_id } => {
            let tip = find_tip(selected_event_id)?;
            if tip.operation != result_operation
                || wire_snapshot(&tip.snapshot_json, &tip.operation)
                    .map_err(peer_resolution_error)?
                    != result_value
            {
                return Err(ApplyVerifiedReceivedResolutionV2Error::MissingCausalProof);
            }
            (None, None)
        }
        NoteSyncResolutionV2Strategy::ManualMerge => {
            let NoteSyncResolutionV2Result::Upsert(note) = &resolution.result else {
                return Err(ApplyVerifiedReceivedResolutionV2Error::InvalidPayload);
            };
            if !content && eligibility(&note.route) != Eligibility::Eligible {
                return Err(ApplyVerifiedReceivedResolutionV2Error::InvalidPayload);
            }
            (None, None)
        }
        NoteSyncResolutionV2Strategy::Delete => {
            if result_operation != "delete" {
                return Err(ApplyVerifiedReceivedResolutionV2Error::InvalidPayload);
            }
            (None, None)
        }
        NoteSyncResolutionV2Strategy::KeepBoth {
            selected_event_id,
            retained_event_id,
            retained_note,
        } => {
            if tips.len() != 2 || tips.iter().any(|tip| tip.operation != "upsert") {
                return Err(ApplyVerifiedReceivedResolutionV2Error::MissingCausalProof);
            }
            let selected = find_tip(selected_event_id)?;
            if wire_snapshot(&selected.snapshot_json, "upsert").map_err(peer_resolution_error)?
                != result_value
            {
                return Err(ApplyVerifiedReceivedResolutionV2Error::MissingCausalProof);
            }
            let retained = find_tip(retained_event_id)?;
            let mut expected =
                wire_snapshot(&retained.snapshot_json, "upsert").map_err(peer_resolution_error)?;
            let retained_value = serde_json::to_value(retained_note)
                .map_err(|_| ApplyVerifiedReceivedResolutionV2Error::InvalidPayload)?;
            let object = expected
                .as_object_mut()
                .ok_or(ApplyVerifiedReceivedResolutionV2Error::MissingCausalProof)?;
            object.insert(
                "id".into(),
                serde_json::Value::String(retained_note.route.id.clone()),
            );
            object.insert(
                "created_at".into(),
                serde_json::Value::String(retained_note.created_at.clone()),
            );
            object.insert(
                "updated_at".into(),
                serde_json::Value::String(retained_note.updated_at.clone()),
            );
            if expected != retained_value {
                return Err(ApplyVerifiedReceivedResolutionV2Error::MissingCausalProof);
            }
            (
                Some(retained_note.route.id.clone()),
                Some(canonical_json(&retained_value).map_err(peer_resolution_error)?),
            )
        }
    };
    Ok((
        result_operation,
        canonical_json(&result_value).map_err(peer_resolution_error)?,
        clone_entity_id,
        clone_snapshot_json,
    ))
}

fn peer_resolution_replay_matches(
    transaction: &Transaction<'_>,
    command: &VerifiedPeerResolutionCommand,
) -> Result<bool, ApplyVerifiedReceivedResolutionV2Error> {
    let parents_json = serde_json::to_string(&command.resolution.resolved_event_ids)
        .map_err(|_| ApplyVerifiedReceivedResolutionV2Error::InvalidPayload)?;
    let strategy = match &command.resolution.strategy {
        NoteSyncResolutionV2Strategy::ChooseVersion { .. } => "choose_version",
        NoteSyncResolutionV2Strategy::ManualMerge => "manual_merge",
        NoteSyncResolutionV2Strategy::KeepBoth { .. } => "keep_both",
        NoteSyncResolutionV2Strategy::Delete => "delete",
    };
    let (result_operation, result_value) =
        result_parts(&command.resolution.result).map_err(peer_resolution_error)?;
    let result_snapshot = canonical_json(&result_value).map_err(peer_resolution_error)?;
    let (clone_entity_id, clone_snapshot) = match &command.resolution.strategy {
        NoteSyncResolutionV2Strategy::KeepBoth { retained_note, .. } => {
            let value = serde_json::to_value(retained_note)
                .map_err(|_| ApplyVerifiedReceivedResolutionV2Error::InvalidPayload)?;
            (
                Some(retained_note.route.id.clone()),
                Some(canonical_json(&value).map_err(peer_resolution_error)?),
            )
        }
        _ => (None, None),
    };
    Ok(transaction.query_row(
        "SELECT source_device_id,server_sequence,project_id,entity_id,revision,event_updated_at,
                remote_conflict_group_id,remote_conflict_generation,parent_event_ids_json,
                parent_count,strategy,result_operation,canonical_payload,result_snapshot_json,
                clone_entity_id,clone_snapshot_json,lifecycle,
                (SELECT count(*) FROM cloud_sync_note_applied_resolution_parents AS parent
                 WHERE parent.account_id=resolution.account_id
                   AND parent.resolution_event_id=resolution.resolution_event_id),
                COALESCE((SELECT inbox.applied_at IS resolution.applied_at
                 FROM cloud_sync_inbox AS inbox
                 WHERE inbox.account_id=resolution.account_id
                   AND inbox.event_id=resolution.resolution_event_id
                   AND inbox.state='applied'),0)
         FROM cloud_sync_note_applied_resolutions AS resolution
         WHERE account_id=?1 AND resolution_event_id=?2",
        rusqlite::params![command.account_id, command.event_id],
        |row| {
            let mut stored_payload=row.get::<_,Vec<u8>>(12)?;
            let payload_matches=stored_payload==command.canonical_payload;
            stored_payload.fill(0);
            Ok(row.get::<_, String>(0)? == command.source_device_id
                && row.get::<_, i64>(1)? == command.server_sequence
                && row.get::<_, String>(2)? == command.resolution.header.project_id
                && row.get::<_, String>(3)? == command.resolution.header.entity_id
                && row.get::<_, i64>(4)? == command.resolution.header.revision
                && note_sync_timestamps_equal(
                    &row.get::<_, String>(5)?,&command.resolution.header.updated_at)
                && row.get::<_, String>(6)? == command.resolution.conflict_group_id
                && row.get::<_, i64>(7)? == command.resolution.conflict_generation
                && row.get::<_, String>(8)? == parents_json
                && row.get::<_, i64>(9)? == command.resolution.resolved_event_ids.len() as i64
                && row.get::<_, String>(10)? == strategy
                && row.get::<_, String>(11)? == result_operation
                && payload_matches
                && row.get::<_, String>(13)? == result_snapshot
                && row.get::<_, Option<String>>(14)? == clone_entity_id
                && row.get::<_, Option<String>>(15)? == clone_snapshot
                && row.get::<_, String>(16)? == "applied"
                && row.get::<_, i64>(17)? == command.resolution.resolved_event_ids.len() as i64
                && row.get::<_, i64>(18)? == 1)
        },
    ).optional()? == Some(true))
}

fn peer_resolution_tip_proofs(
    transaction: &Transaction<'_>,
    account_id: &str,
    project_id: &str,
    entity_id: &str,
    group_id: &str,
) -> Result<Vec<PeerResolutionTipProof>, ApplyVerifiedReceivedResolutionV2Error> {
    let mut statement = transaction.prepare(
        "SELECT version.version_id,version.event_id,version.parent_event_id,
                version.revision,version.operation,version.snapshot_json,version.source,
                version.local_mutation_generation,version.server_sequence
         FROM cloud_sync_note_conflict_tips AS tip
         JOIN cloud_sync_note_conflict_versions AS version
           ON version.version_id=tip.version_id AND version.group_id=tip.group_id
         WHERE tip.group_id=?1 AND version.account_id=?2 AND version.project_id=?3
           AND version.entity_id=?4 AND version.entity_type='note'
         ORDER BY version.event_id",
    )?;
    let versions = statement
        .query_map(
            rusqlite::params![group_id, account_id, project_id, entity_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, Option<i64>>(7)?,
                    row.get::<_, Option<i64>>(8)?,
                ))
            },
        )?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    let tip_count: i64 = transaction.query_row(
        "SELECT count(*) FROM cloud_sync_note_conflict_tips WHERE group_id=?1",
        [group_id],
        |row| row.get(0),
    )?;
    if tip_count != versions.len() as i64 {
        return Err(ApplyVerifiedReceivedResolutionV2Error::StaleConflict);
    }
    let mut proofs = Vec::with_capacity(versions.len());
    for version in versions {
        let sequence = match version.6.as_str() {
            "remote" => {
                let expected = version
                    .8
                    .ok_or(ApplyVerifiedReceivedResolutionV2Error::MissingCausalProof)?;
                transaction.query_row(
                    "SELECT server_sequence FROM cloud_sync_inbox
                     WHERE account_id=?1 AND event_id=?2 AND server_sequence=?3
                       AND project_id=?4 AND entity_id=?5 AND entity_type='note'
                       AND (operation=?6 OR (operation='event' AND EXISTS(SELECT 1 FROM cloud_content_note_receipts r WHERE r.account_id=cloud_sync_inbox.account_id AND r.event_id=cloud_sync_inbox.event_id AND r.outcome='conflict_preserved'))) AND sync_revision=?7
                       AND state='conflict_preserved' AND conflict_group_id=?8",
                    rusqlite::params![account_id, version.1, expected, project_id, entity_id,
                        version.4, version.3, group_id], |row| row.get(0),
                ).optional()?.ok_or(ApplyVerifiedReceivedResolutionV2Error::MissingCausalProof)?
            }
            "remote_applied" => {
                let expected = version
                    .8
                    .ok_or(ApplyVerifiedReceivedResolutionV2Error::MissingCausalProof)?;
                transaction
                    .query_row(
                        "SELECT server_sequence FROM cloud_sync_note_causal_history
                     WHERE account_id=?1 AND event_id=?2 AND server_sequence=?3
                       AND project_id=?4 AND entity_id=?5 AND entity_type='note'
                       AND revision=?6 AND operation=?7 AND snapshot_json=?8",
                        rusqlite::params![
                            account_id, version.1, expected, project_id, entity_id, version.3,
                            version.4, version.5
                        ],
                        |row| row.get(0),
                    )
                    .optional()?
                    .ok_or(ApplyVerifiedReceivedResolutionV2Error::MissingCausalProof)?
            }
            "local_unsealed" => transaction
                .query_row(
                    "SELECT receipt.server_sequence
                 FROM cloud_sync_outbox AS outbox
                 JOIN cloud_sync_upload_receipts AS receipt
                   ON receipt.account_id=outbox.account_id AND receipt.event_id=outbox.event_id
                  AND receipt.device_id=outbox.device_id
                 WHERE outbox.account_id=?1 AND outbox.event_id=?2
                   AND outbox.project_id=?3 AND outbox.entity_id=?4 AND outbox.entity_type='note'
                   AND outbox.operation=?5 AND outbox.revision=?6 AND outbox.lifecycle='accepted'",
                    rusqlite::params![
                        account_id, version.1, project_id, entity_id, version.4, version.3
                    ],
                    |row| row.get(0),
                )
                .optional()?
                .ok_or(ApplyVerifiedReceivedResolutionV2Error::MissingCausalProof)?,
            _ => return Err(ApplyVerifiedReceivedResolutionV2Error::MissingCausalProof),
        };
        proofs.push(PeerResolutionTipProof {
            version_id: version.0,
            event_id: version.1,
            parent_event_id: version.2,
            revision: version.3,
            operation: version.4,
            snapshot_json: version.5,
            publication_source: match version.6.as_str() {
                "local_unsealed" => "local_accepted".into(),
                value => value.into(),
            },
            server_sequence: sequence,
            local_mutation_generation: version.7,
        });
    }
    Ok(proofs)
}

fn advance_resolution_entity_head(
    transaction: &Transaction<'_>,
    command: &VerifiedPeerResolutionCommand,
    entity_id: &str,
) -> rusqlite::Result<()> {
    transaction.execute(
        "INSERT INTO cloud_sync_entities(
            account_id,project_id,entity_id,entity_type,head_event_id,
            head_sync_revision,conflict_event_id,conflict_state,updated_at
         ) VALUES(?1,?2,?3,'note',?4,?5,NULL,NULL,?6)
         ON CONFLICT(account_id,project_id,entity_id,entity_type) DO UPDATE SET
            head_event_id=excluded.head_event_id,
            head_sync_revision=excluded.head_sync_revision,
            conflict_event_id=NULL,conflict_state=NULL,updated_at=excluded.updated_at",
        rusqlite::params![
            command.account_id,
            command.resolution.header.project_id,
            entity_id,
            command.event_id,
            command.resolution.header.revision,
            command.resolution.header.updated_at
        ],
    )?;
    Ok(())
}

pub(crate) fn apply_verified_received_resolution_v2_ipc(
    connection: &mut PrivilegedRemoteApplyConnection,
    mut command: ApplyVerifiedReceivedResolutionV2IpcCommand,
) -> Result<ApplyVerifiedReceivedResolutionV2Result, ApplyVerifiedReceivedResolutionV2Error> {
    let mut canonical_payload = std::mem::take(&mut command.plaintext);
    let resolution = match decode_canonical_resolution_bytes(&canonical_payload) {
        Ok(value) => value,
        Err(error) => {
            canonical_payload.fill(0);
            return Err(peer_resolution_error(error));
        }
    };
    let mut verified = VerifiedPeerResolutionCommand {
        account_id: command.account_id,
        canonical_user_id: command.canonical_user_id,
        pulling_device_id: command.pulling_device_id,
        event_id: command.event_id,
        server_sequence: command.server_sequence,
        source_device_id: command.source_device_id,
        crypto_version: command.crypto_version,
        aad_version: command.aad_version,
        nonce: command.nonce,
        ciphertext: command.ciphertext,
        canonical_payload,
        resolution,
    };
    let result = apply_verified_received_resolution_v2_inner(connection, &verified, false, false);
    verified.canonical_payload.fill(0);
    result
}

fn apply_verified_received_resolution_v2_inner(
    connection: &mut PrivilegedRemoteApplyConnection,
    command: &VerifiedPeerResolutionCommand,
    inject_after_original: bool,
    inject_before_completion: bool,
) -> Result<ApplyVerifiedReceivedResolutionV2Result, ApplyVerifiedReceivedResolutionV2Error> {
    apply_resolution_with_content(
        connection,
        command,
        inject_after_original,
        inject_before_completion,
        None,
    )
}
fn apply_resolution_with_content(
    connection: &mut PrivilegedRemoteApplyConnection,
    command: &VerifiedPeerResolutionCommand,
    inject_after_original: bool,
    inject_before_completion: bool,
    content: Option<(
        &crate::content_note_sync::ContentNote,
        &ApplyVerifiedReceivedNoteIpcCommand,
    )>,
) -> Result<ApplyVerifiedReceivedResolutionV2Result, ApplyVerifiedReceivedResolutionV2Error> {
    if command.event_id != command.resolution.header.event_id
        || command.crypto_version != SUPPORTED_CRYPTO_VERSION
        || command.aad_version != SUPPORTED_AAD_VERSION
    {
        return Err(ApplyVerifiedReceivedResolutionV2Error::ImmutableMismatch);
    }
    connection.execute_planned_many_once(
        |transaction| {
            validate_pull_scope(transaction, &command.account_id, &command.pulling_device_id,
                &command.canonical_user_id)
                .map_err(|_| ApplyVerifiedReceivedResolutionV2Error::ScopeMismatch)?;
            let inbox: Option<(i64,String,String,String,String,i64,String,Option<String>,String)> =
                transaction.query_row(
                    "SELECT server_sequence,device_id,project_id,entity_id,operation,
                            sync_revision,updated_at,deleted_at,state
                     FROM cloud_sync_inbox
                     WHERE account_id=?1 AND event_id=?2 AND entity_type='note'",
                    rusqlite::params![command.account_id, command.event_id],
                    |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,
                        row.get(5)?,row.get(6)?,row.get(7)?,row.get(8)?)),
                ).optional()?;
            let Some((sequence,source,project,entity,operation,revision,updated_at,deleted_at,state)) = inbox else {
                return Err(ApplyVerifiedReceivedResolutionV2Error::ImmutableMismatch);
            };
            if sequence != command.server_sequence || source != command.source_device_id
                || project != command.resolution.header.project_id
                || entity != command.resolution.header.entity_id || operation != if content.is_some() { "event" } else { "resolution" }
                || revision != command.resolution.header.revision
                || !note_sync_timestamps_equal(&updated_at, &command.resolution.header.updated_at)
                || deleted_at.is_some()
            {
                return Err(ApplyVerifiedReceivedResolutionV2Error::ImmutableMismatch);
            }
            let object: Option<(i64,i64,Vec<u8>,Vec<u8>)> = transaction.query_row(
                "SELECT crypto_version,aad_version,nonce,ciphertext
                 FROM cloud_sync_event_objects WHERE account_id=?1 AND event_id=?2",
                rusqlite::params![command.account_id, command.event_id],
                |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
            ).optional()?;
            if object != Some((command.crypto_version, command.aad_version,
                command.nonce.clone(), command.ciphertext.clone()))
            {
                return Err(ApplyVerifiedReceivedResolutionV2Error::ImmutableMismatch);
            }
            if let Some((event, raw)) = content {
                crate::content_note_sync::receipt(transaction, raw, Some(&event.frame))?;
                if state != "applied" {
                    if let Err(code) = crate::content_note_sync::ready(transaction, &command.account_id, &command.canonical_user_id, &command.source_device_id, event) {
                        transaction.execute("UPDATE cloud_sync_inbox SET state='orphan',error_code=?3 WHERE account_id=?1 AND event_id=?2", rusqlite::params![command.account_id,command.event_id,code])?;
                        transaction.execute("UPDATE cloud_content_note_receipts SET blocker=?3 WHERE account_id=?1 AND event_id=?2", rusqlite::params![command.account_id,command.event_id,code])?;
                        return Ok((Vec::new(), PeerResolutionApplyPlan::Orphan { error_code: "dependency_not_synced" }));
                    }
                }
            }
            let project_bound: i64 = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM cloud_sync_project_bindings
                 WHERE project_id=?1 AND account_id=?2)",
                rusqlite::params![project, command.account_id], |row| row.get(0),
            )?;
            if project_bound != 1 {
                return Err(ApplyVerifiedReceivedResolutionV2Error::ScopeMismatch);
            }
            if state == "applied" {
                if !peer_resolution_replay_matches(transaction, command)? {
                    return Err(ApplyVerifiedReceivedResolutionV2Error::ImmutableMismatch);
                }
                return Ok((Vec::new(), PeerResolutionApplyPlan::Replay));
            }
            if state != "received" && state != "orphan" {
                return Err(ApplyVerifiedReceivedResolutionV2Error::StaleConflict);
            }
            if source == command.pulling_device_id {
                let local: Option<i64> = transaction.query_row(
                    "SELECT 1 FROM cloud_sync_note_resolution_outbox
                     WHERE resolution_event_id=?1 AND account_id=?2 AND device_id=?3
                       AND project_id=?4 AND entity_id=?5 AND revision=?6
                       AND canonical_payload=?7",
                    rusqlite::params![command.event_id, command.account_id, source, project,
                        entity, revision, command.canonical_payload], |_| Ok(1),
                ).optional()?;
                if local.is_none() {
                    return Err(ApplyVerifiedReceivedResolutionV2Error::SelfEchoMismatch);
                }
                return Ok((Vec::new(), PeerResolutionApplyPlan::SelfEcho));
            }
            let group: Option<(String,i64,String)> = transaction.query_row(
                "SELECT group_id,generation,common_parent_event_id
                 FROM cloud_sync_note_conflict_groups
                 WHERE account_id=?1 AND project_id=?2 AND entity_id=?3
                   AND entity_type='note' AND lifecycle='open'",
                rusqlite::params![command.account_id, project, entity],
                |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
            ).optional()?;
            let Some((local_group_id,local_generation,common_parent)) = group else {
                return Ok((Vec::new(), PeerResolutionApplyPlan::Orphan { error_code: "missing_conflict_group" }));
            };
            let mut tips = match peer_resolution_tip_proofs(transaction, &command.account_id,
                &project, &entity, &local_group_id)
            {
                Ok(value) => value,
                Err(ApplyVerifiedReceivedResolutionV2Error::MissingCausalProof) => {
                    return Ok((Vec::new(), PeerResolutionApplyPlan::Orphan { error_code: "incomplete_parent_publication" }));
                }
                Err(error) => return Err(error),
            };
            let actual_ids: Vec<String> = tips.iter().map(|tip| tip.event_id.clone()).collect();
            if actual_ids != command.resolution.resolved_event_ids {
                let subset = actual_ids.len() < command.resolution.resolved_event_ids.len()
                    && actual_ids.iter().all(|event_id| command.resolution.resolved_event_ids.contains(event_id));
                if subset {
                    return Ok((Vec::new(), PeerResolutionApplyPlan::Orphan { error_code: "missing_conflict_parent" }));
                }
                return Err(ApplyVerifiedReceivedResolutionV2Error::StaleConflict);
            }
            if tips.iter().any(|tip| tip.parent_event_id != common_parent) {
                return Err(ApplyVerifiedReceivedResolutionV2Error::StaleConflict);
            }
            let maximum = tips.iter().map(|tip| tip.revision).max()
                .ok_or(ApplyVerifiedReceivedResolutionV2Error::MissingCausalProof)?;
            if maximum.checked_add(1) != Some(revision) {
                return Err(ApplyVerifiedReceivedResolutionV2Error::MissingCausalProof);
            }
            let parent_known: i64 = transaction.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM cloud_sync_note_causal_history
                     WHERE account_id=?1 AND project_id=?2 AND entity_id=?3 AND event_id=?4
                    UNION ALL
                    SELECT 1 FROM cloud_sync_outbox
                     WHERE account_id=?1 AND project_id=?2 AND entity_id=?3 AND event_id=?4
                       AND lifecycle IN ('sealed','accepted'))",
                rusqlite::params![command.account_id, project, entity, common_parent],
                |row| row.get(0),
            )?;
            let resolution_parent = is_resolution_event(transaction, &common_parent)
                .map_err(|_| ApplyVerifiedReceivedResolutionV2Error::MissingCausalProof)?;
            let resolution_proven = if resolution_parent {
                applied_resolution_parent_revision(transaction, &command.account_id,
                    &project, &entity, &common_parent)
                    .map_err(|_| ApplyVerifiedReceivedResolutionV2Error::MissingCausalProof)?
                    .and_then(|value| value.checked_add(1)) == Some(revision - 1)
            } else { false };
            if parent_known != 1 && !resolution_proven {
                return Ok((Vec::new(), PeerResolutionApplyPlan::Orphan { error_code: "missing_common_parent" }));
            }
            let unexpected_unsealed: i64 = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM cloud_sync_outbox
                 WHERE account_id=?1 AND project_id=?2 AND entity_id=?3 AND entity_type='note'
                   AND lifecycle='unsealed' AND event_id NOT IN (
                       SELECT event_id FROM cloud_sync_note_conflict_tips WHERE group_id=?4))",
                rusqlite::params![command.account_id, project, entity, local_group_id],
                |row| row.get(0),
            )?;
            if unexpected_unsealed != 0 {
                return Err(ApplyVerifiedReceivedResolutionV2Error::StaleConflict);
            }
            let (result_operation,result_snapshot_json,clone_entity_id,clone_snapshot_json) =
                peer_result_evidence_with_content(&command.resolution, &tips, content.is_some())?;
            if let Some(clone_id) = clone_entity_id.as_deref() {
                let collision: i64 = transaction.query_row(
                    "SELECT EXISTS(
                        SELECT 1 FROM notes WHERE id=?1
                        UNION ALL SELECT 1 FROM cloud_sync_entities
                         WHERE account_id=?2 AND project_id=?3 AND entity_id=?1
                        UNION ALL SELECT 1 FROM cloud_sync_outbox
                         WHERE account_id=?2 AND project_id=?3 AND entity_id=?1
                        UNION ALL SELECT 1 FROM cloud_sync_note_resolution_outbox
                         WHERE account_id=?2 AND project_id=?3
                           AND (entity_id=?1 OR clone_entity_id=?1)
                        UNION ALL SELECT 1 FROM cloud_sync_note_applied_resolutions
                         WHERE account_id=?2 AND project_id=?3
                           AND (entity_id=?1 OR clone_entity_id=?1))",
                    rusqlite::params![clone_id, command.account_id, project], |row| row.get(0),
                )?;
                if collision != 0 {
                    return Err(ApplyVerifiedReceivedResolutionV2Error::CloneCollision);
                }
            }
            let original = resolution_mutation_for_result(transaction, &command.resolution.result)
                .map_err(peer_resolution_error)?;
            let clone = match &command.resolution.strategy {
                NoteSyncResolutionV2Strategy::KeepBoth { retained_note, .. } =>
                    resolution_mutation_for_result(transaction,
                        &NoteSyncResolutionV2Result::Upsert(retained_note.clone()))
                        .map_err(peer_resolution_error)?,
                _ => ResolutionNoteMutation::None,
            };
            let mut authorizations = Vec::new();
            if let Some(value) = resolution_authorization(&original, command.event_id.clone(), &command.account_id) {
                authorizations.push(value);
            }
            if let Some(value) = resolution_authorization(&clone, format!("{}:clone", command.event_id), &command.account_id) {
                authorizations.push(value);
            }
            Ok((authorizations, PeerResolutionApplyPlan::Apply {
                local_group_id, local_generation, tips: std::mem::take(&mut tips),
                strategy: match &command.resolution.strategy {
                    NoteSyncResolutionV2Strategy::ChooseVersion { .. } => "choose_version",
                    NoteSyncResolutionV2Strategy::ManualMerge => "manual_merge",
                    NoteSyncResolutionV2Strategy::KeepBoth { .. } => "keep_both",
                    NoteSyncResolutionV2Strategy::Delete => "delete",
                },
                result_operation, result_snapshot_json, clone_entity_id, clone_snapshot_json,
                original, clone,
            }))
        },
        |transaction, plan| match plan {
            PeerResolutionApplyPlan::Replay => Ok(ApplyVerifiedReceivedResolutionV2Result::AlreadyApplied),
            PeerResolutionApplyPlan::SelfEcho => Ok(ApplyVerifiedReceivedResolutionV2Result::SelfEchoPending),
            PeerResolutionApplyPlan::Orphan { error_code } => {
                transaction.execute(
                    "UPDATE cloud_sync_inbox SET state='orphan',error_code=?1,applied_at=NULL
                     WHERE account_id=?2 AND event_id=?3 AND state='received'",
                    rusqlite::params![error_code, command.account_id, command.event_id],
                )?;
                if content.is_some() { crate::content_note_sync::complete(transaction, &command.account_id, &command.event_id)?; }
                Ok(ApplyVerifiedReceivedResolutionV2Result::Orphan)
            }
            PeerResolutionApplyPlan::Apply {
                local_group_id,local_generation,tips,strategy,result_operation,
                result_snapshot_json,clone_entity_id,clone_snapshot_json,original,clone,
            } => {
                apply_resolution_note_mutation(transaction, &original)?;
                if inject_after_original {
                    return Err(ApplyVerifiedReceivedResolutionV2Error::InjectedFailure);
                }
                apply_resolution_note_mutation(transaction, &clone)?;
                let parents_json = serde_json::to_string(&command.resolution.resolved_event_ids)
                    .map_err(|_| ApplyVerifiedReceivedResolutionV2Error::InvalidPayload)?;
                let now: String = transaction.query_row(
                    "SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')", [], |row| row.get(0),
                )?;
                let event_updated_at:String=transaction.query_row(
                    "SELECT updated_at FROM cloud_sync_inbox WHERE account_id=?1 AND event_id=?2",
                    rusqlite::params![command.account_id,command.event_id],|row|row.get(0),
                )?;
                transaction.execute(
                    "INSERT INTO cloud_sync_note_applied_resolutions(
                        account_id,resolution_event_id,source_device_id,server_sequence,
                        project_id,entity_id,revision,event_updated_at,remote_conflict_group_id,
                        local_conflict_group_id,remote_conflict_generation,local_conflict_generation,
                        parent_event_ids_json,parent_count,strategy,result_operation,canonical_payload,
                        result_snapshot_json,clone_entity_id,clone_snapshot_json,lifecycle,applied_at,created_at
                     ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,
                              ?15,?16,?17,?18,?19,?20,'applying',NULL,?21)",
                    rusqlite::params![command.account_id,command.event_id,command.source_device_id,
                        command.server_sequence,command.resolution.header.project_id,
                        command.resolution.header.entity_id,command.resolution.header.revision,
                        event_updated_at,command.resolution.conflict_group_id,
                        local_group_id,command.resolution.conflict_generation,local_generation,
                        parents_json,tips.len() as i64,strategy,result_operation,
                        command.canonical_payload,result_snapshot_json,clone_entity_id,
                        clone_snapshot_json,now],
                )?;
                for (ordinal, tip) in tips.iter().enumerate() {
                    transaction.execute(
                        "INSERT INTO cloud_sync_note_applied_resolution_parents(
                            account_id,resolution_event_id,parent_ordinal,parent_event_id,
                            conflict_version_id,parent_revision,parent_operation,parent_snapshot_json,
                            publication_source,server_sequence,local_mutation_generation,recorded_at
                         ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
                        rusqlite::params![command.account_id,command.event_id,ordinal as i64,
                            tip.event_id,tip.version_id,tip.revision,tip.operation,tip.snapshot_json,
                            tip.publication_source,tip.server_sequence,tip.local_mutation_generation,now],
                    )?;
                }
                advance_resolution_entity_head(transaction, command, &command.resolution.header.entity_id)?;
                if let Some(clone_id) = clone_entity_id.as_deref() {
                    advance_resolution_entity_head(transaction, command, clone_id)?;
                }
                if inject_before_completion {
                    return Err(ApplyVerifiedReceivedResolutionV2Error::InjectedFailure);
                }
                let completed = transaction.execute(
                    "UPDATE cloud_sync_note_applied_resolutions
                     SET lifecycle='applied',applied_at=?1
                     WHERE account_id=?2 AND resolution_event_id=?3 AND lifecycle='applying'",
                    rusqlite::params![now,command.account_id,command.event_id],
                )?;
                let resolved = transaction.execute(
                    "UPDATE cloud_sync_note_conflict_groups
                     SET lifecycle='resolved',updated_at=?1
                     WHERE group_id=?2 AND account_id=?3 AND generation=?4 AND lifecycle='open'",
                    rusqlite::params![now,local_group_id,command.account_id,local_generation],
                )?;
                let applied = transaction.execute(
                    "UPDATE cloud_sync_inbox SET state='applied',error_code=NULL,applied_at=?1
                     WHERE account_id=?2 AND event_id=?3 AND state IN ('received','orphan')",
                    rusqlite::params![now,command.account_id,command.event_id],
                )?;
                if completed != 1 || resolved != 1 || applied != 1 {
                    return Err(ApplyVerifiedReceivedResolutionV2Error::StaleConflict);
                }
                if content.is_some() { crate::content_note_sync::complete(transaction, &command.account_id, &command.event_id)?; }
                Ok(ApplyVerifiedReceivedResolutionV2Result::Applied)
            }
        },
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SelfEchoFailurePoint {
    None,
    AfterReceipt,
    AfterAcceptance,
    BeforeLedgerCompletion,
    BeforeInboxApply,
}

enum SelfEchoReconcilePlan {
    Replay,
    Reconcile {
        create_pull_receipt: bool,
        local_group_id: String,
        local_generation: i64,
        tips: Vec<PeerResolutionTipProof>,
        strategy: &'static str,
        result_operation: &'static str,
        result_snapshot_json: String,
        clone_entity_id: Option<String>,
        clone_snapshot_json: Option<String>,
    },
}

fn self_echo_result_matches_notes(
    transaction: &Transaction<'_>,
    resolution: &crate::note_sync_plaintext::NoteSyncResolutionV2,
) -> Result<bool, ApplyVerifiedReceivedResolutionV2Error> {
    let result_matches = match &resolution.result {
        NoteSyncResolutionV2Result::Upsert(note) => {
            let expected = note_payload_from_record(note)
                .map_err(|_| ApplyVerifiedReceivedResolutionV2Error::InvalidPayload)?;
            transaction
                .query_row(
                    "SELECT project_id=?2 AND payload_json=?3 FROM notes WHERE id=?1",
                    rusqlite::params![note.route.id, note.route.project_id, expected],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?
                == Some(1)
        }
        NoteSyncResolutionV2Result::Delete(note) => {
            transaction.query_row(
                "SELECT NOT EXISTS(SELECT 1 FROM notes WHERE id=?1)",
                [&note.route.id],
                |row| row.get::<_, i64>(0),
            )? == 1
        }
    };
    if !result_matches {
        return Ok(false);
    }
    if let NoteSyncResolutionV2Strategy::KeepBoth { retained_note, .. } = &resolution.strategy {
        let expected = note_payload_from_record(retained_note)
            .map_err(|_| ApplyVerifiedReceivedResolutionV2Error::InvalidPayload)?;
        return Ok(transaction
            .query_row(
                "SELECT project_id=?2 AND payload_json=?3 FROM notes WHERE id=?1",
                rusqlite::params![
                    retained_note.route.id,
                    retained_note.route.project_id,
                    expected
                ],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            == Some(1));
    }
    Ok(true)
}

fn self_echo_heads_match(
    transaction: &Transaction<'_>,
    command: &VerifiedPeerResolutionCommand,
    clone_entity_id: Option<&str>,
) -> Result<bool, ApplyVerifiedReceivedResolutionV2Error> {
    let matches = |entity_id: &str| -> rusqlite::Result<bool> {
        Ok(transaction
            .query_row(
                "SELECT head_event_id=?4 AND head_sync_revision=?5
             FROM cloud_sync_entities
             WHERE account_id=?1 AND project_id=?2 AND entity_id=?3 AND entity_type='note'",
                rusqlite::params![
                    command.account_id,
                    command.resolution.header.project_id,
                    entity_id,
                    command.event_id,
                    command.resolution.header.revision
                ],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            == Some(1))
    };
    if !matches(&command.resolution.header.entity_id)? {
        return Ok(false);
    }
    match clone_entity_id {
        Some(entity_id) => Ok(matches(entity_id)?),
        None => Ok(true),
    }
}

fn self_echo_dependencies_match(
    transaction: &Transaction<'_>,
    command: &VerifiedPeerResolutionCommand,
    tips: &[PeerResolutionTipProof],
) -> Result<bool, ApplyVerifiedReceivedResolutionV2Error> {
    let count: i64 = transaction.query_row(
        "SELECT count(*) FROM cloud_sync_note_resolution_dependencies
         WHERE resolution_event_id=?1",
        [&command.event_id],
        |row| row.get(0),
    )?;
    if count != tips.len() as i64 {
        return Ok(false);
    }
    for tip in tips {
        let (source, server_sequence) = if tip.publication_source == "local_accepted" {
            ("local_unsealed", None)
        } else {
            (tip.publication_source.as_str(), Some(tip.server_sequence))
        };
        let exact: i64 = transaction.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM cloud_sync_note_resolution_dependencies
                WHERE resolution_event_id=?1 AND parent_event_id=?2 AND source=?3
                  AND server_sequence IS ?4 AND local_mutation_generation IS ?5
                  AND snapshot_json=?6
                  AND (upload_receipt_sequence IS NULL OR upload_receipt_sequence=?7))",
            rusqlite::params![
                command.event_id,
                tip.event_id,
                source,
                server_sequence,
                tip.local_mutation_generation,
                tip.snapshot_json,
                tip.server_sequence
            ],
            |row| row.get(0),
        )?;
        if exact != 1 {
            return Ok(false);
        }
    }
    Ok(true)
}

fn self_echo_replay_matches(
    transaction: &Transaction<'_>,
    command: &VerifiedPeerResolutionCommand,
    local_group_id: &str,
    local_generation: i64,
    clone_entity_id: Option<&str>,
) -> Result<bool, ApplyVerifiedReceivedResolutionV2Error> {
    if !peer_resolution_replay_matches(transaction, command)? {
        return Ok(false);
    }
    let local_matches = transaction
        .query_row(
            "SELECT local_conflict_group_id=?3 AND local_conflict_generation=?4
           AND remote_conflict_group_id=?3 AND remote_conflict_generation=?4
         FROM cloud_sync_note_applied_resolutions
         WHERE account_id=?1 AND resolution_event_id=?2 AND lifecycle='applied'",
            rusqlite::params![
                command.account_id,
                command.event_id,
                local_group_id,
                local_generation
            ],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        == Some(1);
    if !local_matches || !self_echo_heads_match(transaction, command, clone_entity_id)? {
        return Ok(false);
    }
    let tips = peer_resolution_tip_proofs(
        transaction,
        &command.account_id,
        &command.resolution.header.project_id,
        &command.resolution.header.entity_id,
        local_group_id,
    )?;
    if tips
        .iter()
        .map(|tip| tip.event_id.as_str())
        .collect::<Vec<_>>()
        != command
            .resolution
            .resolved_event_ids
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
        || !self_echo_dependencies_match(transaction, command, &tips)?
    {
        return Ok(false);
    }
    for (ordinal, tip) in tips.iter().enumerate() {
        let exact: i64 = transaction.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM cloud_sync_note_applied_resolution_parents
                WHERE account_id=?1 AND resolution_event_id=?2 AND parent_ordinal=?3
                  AND parent_event_id=?4 AND conflict_version_id=?5 AND parent_revision=?6
                  AND parent_operation=?7 AND parent_snapshot_json=?8
                  AND publication_source=?9 AND server_sequence=?10
                  AND local_mutation_generation IS ?11)",
            rusqlite::params![
                command.account_id,
                command.event_id,
                ordinal as i64,
                tip.event_id,
                tip.version_id,
                tip.revision,
                tip.operation,
                tip.snapshot_json,
                tip.publication_source,
                tip.server_sequence,
                tip.local_mutation_generation
            ],
            |row| row.get(0),
        )?;
        if exact != 1 {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(crate) fn reconcile_verified_received_resolution_self_echo_ipc(
    connection: &mut PrivilegedRemoteApplyConnection,
    mut command: ReconcileVerifiedReceivedResolutionSelfEchoIpcCommand,
) -> Result<ReconcileVerifiedReceivedResolutionSelfEchoResult, ApplyVerifiedReceivedResolutionV2Error>
{
    let mut canonical_payload = std::mem::take(&mut command.plaintext);
    let resolution = match decode_canonical_resolution_bytes(&canonical_payload) {
        Ok(value) => value,
        Err(error) => {
            canonical_payload.fill(0);
            return Err(peer_resolution_error(error));
        }
    };
    let mut verified = VerifiedPeerResolutionCommand {
        account_id: command.account_id,
        canonical_user_id: command.canonical_user_id,
        pulling_device_id: command.pulling_device_id,
        event_id: command.event_id,
        server_sequence: command.server_sequence,
        source_device_id: command.source_device_id,
        crypto_version: command.crypto_version,
        aad_version: command.aad_version,
        nonce: command.nonce,
        ciphertext: command.ciphertext,
        canonical_payload,
        resolution,
    };
    let result = reconcile_verified_received_resolution_self_echo_inner(
        connection,
        &verified,
        SelfEchoFailurePoint::None,
    );
    verified.canonical_payload.fill(0);
    result
}

fn reconcile_verified_received_resolution_self_echo_inner(
    connection: &mut PrivilegedRemoteApplyConnection,
    command: &VerifiedPeerResolutionCommand,
    failure: SelfEchoFailurePoint,
) -> Result<ReconcileVerifiedReceivedResolutionSelfEchoResult, ApplyVerifiedReceivedResolutionV2Error>
{
    reconcile_resolution_with_content(connection, command, failure, None)
}
fn reconcile_resolution_with_content(
    connection: &mut PrivilegedRemoteApplyConnection,
    command: &VerifiedPeerResolutionCommand,
    failure: SelfEchoFailurePoint,
    content: Option<(
        &crate::content_note_sync::ContentNote,
        &ApplyVerifiedReceivedNoteIpcCommand,
    )>,
) -> Result<ReconcileVerifiedReceivedResolutionSelfEchoResult, ApplyVerifiedReceivedResolutionV2Error>
{
    if command.event_id != command.resolution.header.event_id
        || command.source_device_id != command.pulling_device_id
        || command.crypto_version != SUPPORTED_CRYPTO_VERSION
        || command.aad_version != SUPPORTED_AAD_VERSION
    {
        return Err(ApplyVerifiedReceivedResolutionV2Error::SelfEchoMismatch);
    }
    connection.execute_planned_many_once(
        |transaction| {
            if let Some((event,ipc))=content {
                crate::content_note_sync::receipt(transaction,ipc,Some(&event.frame))?;
                crate::content_note_sync::ready(transaction,&command.account_id,&command.canonical_user_id,&command.source_device_id,event)
                    .map_err(|_|ApplyVerifiedReceivedResolutionV2Error::MissingCausalProof)?;
            }
            validate_pull_scope(transaction, &command.account_id, &command.pulling_device_id,
                &command.canonical_user_id)
                .map_err(|_| ApplyVerifiedReceivedResolutionV2Error::ScopeMismatch)?;
            let inbox: Option<(i64,String,String,String,String,i64,String,Option<String>,String)> =
                transaction.query_row(
                    "SELECT server_sequence,device_id,project_id,entity_id,operation,
                            sync_revision,updated_at,deleted_at,state
                     FROM cloud_sync_inbox
                     WHERE account_id=?1 AND event_id=?2 AND entity_type='note'",
                    rusqlite::params![command.account_id,command.event_id],
                    |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,
                        row.get(5)?,row.get(6)?,row.get(7)?,row.get(8)?)),
                ).optional()?;
            let Some((sequence,source,project,entity,operation,revision,updated_at,deleted_at,state))=inbox else {
                return Err(ApplyVerifiedReceivedResolutionV2Error::ImmutableMismatch);
            };
            if sequence!=command.server_sequence || source!=command.source_device_id
                || source!=command.pulling_device_id
                || project!=command.resolution.header.project_id
                || entity!=command.resolution.header.entity_id || (operation!="resolution" && !(operation=="event" && content.is_some()))
                || revision!=command.resolution.header.revision
                || !note_sync_timestamps_equal(&updated_at,&command.resolution.header.updated_at)
                || deleted_at.is_some()
            {
                return Err(ApplyVerifiedReceivedResolutionV2Error::ImmutableMismatch);
            }
            let object:Option<(i64,i64,Vec<u8>,Vec<u8>)>=transaction.query_row(
                "SELECT crypto_version,aad_version,nonce,ciphertext
                 FROM cloud_sync_event_objects WHERE account_id=?1 AND event_id=?2",
                rusqlite::params![command.account_id,command.event_id],
                |row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
            ).optional()?;
            if object!=Some((command.crypto_version,command.aad_version,
                command.nonce.clone(),command.ciphertext.clone()))
            {
                return Err(ApplyVerifiedReceivedResolutionV2Error::ImmutableMismatch);
            }
            let project_bound:i64=transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM cloud_sync_project_bindings
                 WHERE project_id=?1 AND account_id=?2)",
                rusqlite::params![project,command.account_id],|row|row.get(0),
            )?;
            if project_bound!=1 {
                return Err(ApplyVerifiedReceivedResolutionV2Error::ScopeMismatch);
            }
            let parents_json=serde_json::to_string(&command.resolution.resolved_event_ids)
                .map_err(|_|ApplyVerifiedReceivedResolutionV2Error::InvalidPayload)?;
            let strategy=match &command.resolution.strategy {
                NoteSyncResolutionV2Strategy::ChooseVersion{..}=>"choose_version",
                NoteSyncResolutionV2Strategy::ManualMerge=>"manual_merge",
                NoteSyncResolutionV2Strategy::KeepBoth{..}=>"keep_both",
                NoteSyncResolutionV2Strategy::Delete=>"delete",
            };
            let (result_operation,_)=result_parts(&command.resolution.result)
                .map_err(peer_resolution_error)?;
            let clone_entity_id=match &command.resolution.strategy {
                NoteSyncResolutionV2Strategy::KeepBoth{retained_note,..}=>Some(retained_note.route.id.clone()),
                _=>None,
            };
            let outbox:Option<(String,String,String,String,Option<String>,String,i64,i64,String,String,String,Vec<u8>,String)>=
                transaction.query_row(
                    "SELECT account_id,device_id,project_id,entity_id,clone_entity_id,
                            conflict_group_id,conflict_generation,revision,parent_event_ids_json,
                            strategy,result_operation,canonical_payload,lifecycle
                     FROM cloud_sync_note_resolution_outbox WHERE resolution_event_id=?1",
                    [&command.event_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,
                        row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?,row.get(8)?,row.get(9)?,
                        row.get(10)?,row.get(11)?,row.get(12)?)),
                ).optional()?;
            let Some((outbox_account,outbox_device,outbox_project,outbox_entity,outbox_clone,
                local_group_id,local_generation,outbox_revision,outbox_parents,outbox_strategy,
                outbox_operation,mut outbox_payload,outbox_lifecycle))=outbox else {
                return Err(ApplyVerifiedReceivedResolutionV2Error::SelfEchoMismatch);
            };
            let payload_matches=outbox_payload==command.canonical_payload;
            outbox_payload.fill(0);
            if outbox_account!=command.account_id || outbox_device!=command.pulling_device_id
                || outbox_project!=project || outbox_entity!=entity || outbox_clone!=clone_entity_id
                || local_group_id!=command.resolution.conflict_group_id
                || local_generation!=command.resolution.conflict_generation
                || outbox_revision!=revision || outbox_parents!=parents_json
                || outbox_strategy!=strategy || outbox_operation!=result_operation
                || !payload_matches
            {
                return Err(ApplyVerifiedReceivedResolutionV2Error::SelfEchoMismatch);
            }
            let receipt:Option<(String,i64,Option<i64>,String)>=transaction.query_row(
                "SELECT device_id,server_sequence,duplicate,acceptance_source
                 FROM cloud_sync_note_resolution_upload_receipts
                 WHERE account_id=?1 AND resolution_event_id=?2",
                rusqlite::params![command.account_id,command.event_id],
                |row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
            ).optional()?;
            let receipt_matches=|value:&(String,i64,Option<i64>,String)| {
                value.0==command.pulling_device_id && value.1==command.server_sequence
                    && ((value.3=="push_response" && matches!(value.2,Some(0|1)))
                        || (value.3=="pull_self_echo" && value.2.is_none()))
            };
            let push_receipt_matches=|value:&(String,i64,Option<i64>,String)| {
                value.0==command.pulling_device_id && value.1==command.server_sequence
                    && value.3=="push_response" && matches!(value.2,Some(0|1))
            };
            if state=="applied" {
                if outbox_lifecycle!="accepted" || !receipt.as_ref().is_some_and(receipt_matches)
                    || !self_echo_replay_matches(transaction,command,&local_group_id,
                        local_generation,clone_entity_id.as_deref())?
                {
                    return Err(ApplyVerifiedReceivedResolutionV2Error::ImmutableMismatch);
                }
                return Ok((Vec::new(),SelfEchoReconcilePlan::Replay));
            }
            if state!="received" && state!="orphan" {
                return Err(ApplyVerifiedReceivedResolutionV2Error::StaleConflict);
            }
            let create_pull_receipt=match outbox_lifecycle.as_str() {
                "accepted" if receipt.as_ref().is_some_and(push_receipt_matches)=>false,
                "sealed_local" if receipt.is_none()=>true,
                _=>return Err(ApplyVerifiedReceivedResolutionV2Error::SelfEchoMismatch),
            };
            let pending_matches:i64=transaction.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM cloud_sync_note_pending_resolutions AS pending
                    WHERE pending.resolution_event_id=?1 AND pending.account_id=?2
                      AND pending.device_id=?3 AND pending.project_id=?4
                      AND pending.entity_id=?5 AND pending.conflict_group_id=?6
                      AND pending.expected_conflict_generation=?7
                      AND pending.resolution_revision=?8 AND pending.tip_event_ids_json=?9
                      AND pending.strategy=?10 AND pending.result_operation=?11
                      AND pending.canonical_payload=?12 AND pending.lifecycle='consumed')",
                rusqlite::params![command.event_id,command.account_id,command.pulling_device_id,
                    project,entity,local_group_id,local_generation,revision,parents_json,
                    strategy,result_operation,command.canonical_payload],|row|row.get(0),
            )?;
            if pending_matches!=1 {
                return Err(ApplyVerifiedReceivedResolutionV2Error::SelfEchoMismatch);
            }
            let group:Option<(i64,String,String)>=transaction.query_row(
                "SELECT generation,lifecycle,common_parent_event_id
                 FROM cloud_sync_note_conflict_groups
                 WHERE group_id=?1 AND account_id=?2 AND project_id=?3
                   AND entity_id=?4 AND entity_type='note'",
                rusqlite::params![local_group_id,command.account_id,project,entity],
                |row|Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
            ).optional()?;
            let Some((group_generation,group_lifecycle,common_parent))=group else {
                return Err(ApplyVerifiedReceivedResolutionV2Error::MissingCausalProof);
            };
            if group_generation!=local_generation || group_lifecycle!="resolving" {
                return Err(ApplyVerifiedReceivedResolutionV2Error::StaleConflict);
            }
            let tips=peer_resolution_tip_proofs(transaction,&command.account_id,
                &project,&entity,&local_group_id)?;
            let actual_ids:Vec<String>=tips.iter().map(|tip|tip.event_id.clone()).collect();
            if actual_ids!=command.resolution.resolved_event_ids
                || tips.iter().any(|tip|tip.parent_event_id!=common_parent)
                || !self_echo_dependencies_match(transaction,command,&tips)?
            {
                return Err(ApplyVerifiedReceivedResolutionV2Error::StaleConflict);
            }
            let maximum=tips.iter().map(|tip|tip.revision).max()
                .ok_or(ApplyVerifiedReceivedResolutionV2Error::MissingCausalProof)?;
            if maximum.checked_add(1)!=Some(revision) {
                return Err(ApplyVerifiedReceivedResolutionV2Error::MissingCausalProof);
            }
            let (proved_operation,result_snapshot_json,proved_clone,clone_snapshot_json)=
                peer_result_evidence(&command.resolution,&tips)?;
            if proved_operation!=result_operation || proved_clone!=clone_entity_id
                || !self_echo_result_matches_notes(transaction,&command.resolution)?
            {
                return Err(ApplyVerifiedReceivedResolutionV2Error::ImmutableMismatch);
            }
            Ok((Vec::new(),SelfEchoReconcilePlan::Reconcile{
                create_pull_receipt,local_group_id,local_generation,tips,strategy,
                result_operation,result_snapshot_json,clone_entity_id,clone_snapshot_json,
            }))
        },
        |transaction,plan| match plan {
            SelfEchoReconcilePlan::Replay=>Ok(
                ReconcileVerifiedReceivedResolutionSelfEchoResult::AlreadyReconciled),
            SelfEchoReconcilePlan::Reconcile{
                create_pull_receipt,local_group_id,local_generation,tips,strategy,
                result_operation,result_snapshot_json,clone_entity_id,clone_snapshot_json,
            }=>{
                let now:String=transaction.query_row(
                    "SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')",[],|row|row.get(0),
                )?;
                if create_pull_receipt {
                    let collision:i64=transaction.query_row(
                        "SELECT EXISTS(
                            SELECT 1 FROM cloud_sync_upload_receipts
                             WHERE account_id=?1 AND server_sequence=?2
                            UNION ALL SELECT 1 FROM cloud_sync_note_resolution_upload_receipts
                             WHERE account_id=?1 AND server_sequence=?2
                            UNION ALL SELECT 1 FROM cloud_sync_note_causal_history
                             WHERE account_id=?1 AND server_sequence=?2
                            UNION ALL SELECT 1 FROM cloud_sync_note_applied_resolutions
                             WHERE account_id=?1 AND server_sequence=?2)",
                        rusqlite::params![command.account_id,command.server_sequence],
                        |row|row.get(0),
                    )?;
                    if collision!=0 {
                        return Err(ApplyVerifiedReceivedResolutionV2Error::ImmutableMismatch);
                    }
                    transaction.execute(
                        "INSERT INTO cloud_sync_note_resolution_upload_receipts(
                            account_id,resolution_event_id,device_id,server_sequence,duplicate,
                            acceptance_source,accepted_at
                         ) VALUES(?1,?2,?3,?4,NULL,'pull_self_echo',?5)",
                        rusqlite::params![command.account_id,command.event_id,
                            command.pulling_device_id,command.server_sequence,now],
                    )?;
                    if failure==SelfEchoFailurePoint::AfterReceipt {
                        return Err(ApplyVerifiedReceivedResolutionV2Error::InjectedFailure);
                    }
                    let accepted=transaction.execute(
                        "UPDATE cloud_sync_note_resolution_outbox
                         SET lifecycle='accepted',updated_at=?1
                         WHERE resolution_event_id=?2 AND account_id=?3 AND device_id=?4
                           AND lifecycle='sealed_local'",
                        rusqlite::params![now,command.event_id,command.account_id,
                            command.pulling_device_id],
                    )?;
                    if accepted!=1 {
                        return Err(ApplyVerifiedReceivedResolutionV2Error::StaleConflict);
                    }
                    if failure==SelfEchoFailurePoint::AfterAcceptance {
                        return Err(ApplyVerifiedReceivedResolutionV2Error::InjectedFailure);
                    }
                }
                let parents_json=serde_json::to_string(&command.resolution.resolved_event_ids)
                    .map_err(|_|ApplyVerifiedReceivedResolutionV2Error::InvalidPayload)?;
                let event_updated_at:String=transaction.query_row(
                    "SELECT updated_at FROM cloud_sync_inbox WHERE account_id=?1 AND event_id=?2",
                    rusqlite::params![command.account_id,command.event_id],|row|row.get(0),
                )?;
                transaction.execute(
                    "INSERT INTO cloud_sync_note_applied_resolutions(
                        account_id,resolution_event_id,source_device_id,server_sequence,
                        project_id,entity_id,revision,event_updated_at,remote_conflict_group_id,
                        local_conflict_group_id,remote_conflict_generation,local_conflict_generation,
                        parent_event_ids_json,parent_count,strategy,result_operation,canonical_payload,
                        result_snapshot_json,clone_entity_id,clone_snapshot_json,lifecycle,applied_at,created_at
                     ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?9,?10,?10,?11,?12,
                              ?13,?14,?15,?16,?17,?18,'applying',NULL,?19)",
                    rusqlite::params![command.account_id,command.event_id,command.source_device_id,
                        command.server_sequence,command.resolution.header.project_id,
                        command.resolution.header.entity_id,command.resolution.header.revision,
                        event_updated_at,local_group_id,local_generation,
                        parents_json,tips.len() as i64,strategy,result_operation,
                        command.canonical_payload,result_snapshot_json,clone_entity_id,
                        clone_snapshot_json,now],
                )?;
                for (ordinal,tip) in tips.iter().enumerate() {
                    transaction.execute(
                        "INSERT INTO cloud_sync_note_applied_resolution_parents(
                            account_id,resolution_event_id,parent_ordinal,parent_event_id,
                            conflict_version_id,parent_revision,parent_operation,parent_snapshot_json,
                            publication_source,server_sequence,local_mutation_generation,recorded_at
                         ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
                        rusqlite::params![command.account_id,command.event_id,ordinal as i64,
                            tip.event_id,tip.version_id,tip.revision,tip.operation,tip.snapshot_json,
                            tip.publication_source,tip.server_sequence,tip.local_mutation_generation,now],
                    )?;
                }
                advance_resolution_entity_head(transaction,command,
                    &command.resolution.header.entity_id)?;
                if let Some(clone_id)=clone_entity_id.as_deref() {
                    advance_resolution_entity_head(transaction,command,clone_id)?;
                }
                if failure==SelfEchoFailurePoint::BeforeLedgerCompletion {
                    return Err(ApplyVerifiedReceivedResolutionV2Error::InjectedFailure);
                }
                let completed=transaction.execute(
                    "UPDATE cloud_sync_note_applied_resolutions
                     SET lifecycle='applied',applied_at=?1
                     WHERE account_id=?2 AND resolution_event_id=?3 AND lifecycle='applying'",
                    rusqlite::params![now,command.account_id,command.event_id],
                )?;
                let resolved=transaction.execute(
                    "UPDATE cloud_sync_note_conflict_groups
                     SET lifecycle='resolved',updated_at=?1
                     WHERE group_id=?2 AND account_id=?3 AND generation=?4
                       AND lifecycle='resolving'",
                    rusqlite::params![now,local_group_id,command.account_id,local_generation],
                )?;
                if failure==SelfEchoFailurePoint::BeforeInboxApply {
                    return Err(ApplyVerifiedReceivedResolutionV2Error::InjectedFailure);
                }
                let applied=transaction.execute(
                    "UPDATE cloud_sync_inbox SET state='applied',error_code=NULL,applied_at=?1
                     WHERE account_id=?2 AND event_id=?3 AND state IN ('received','orphan')",
                    rusqlite::params![now,command.account_id,command.event_id],
                )?;
                if completed!=1 || resolved!=1 || applied!=1 {
                    return Err(ApplyVerifiedReceivedResolutionV2Error::StaleConflict);
                }
                if content.is_some(){crate::content_note_sync::complete(transaction,&command.account_id,&command.event_id)?;}
                Ok(ReconcileVerifiedReceivedResolutionSelfEchoResult::Reconciled)
            }
        },
    )
}

fn remote_apply_storage_error(error: StorageError) -> NoteSyncError {
    match error {
        StorageError::Database(error) => NoteSyncError::Database(error),
        StorageError::RemoteApplyAuthorization(_) => {
            NoteSyncError::InvalidEnvelope("remote apply authorization failed")
        }
        StorageError::UnsupportedSchema(_) | StorageError::CorruptSchema(_) => {
            NoteSyncError::InvalidEnvelope("remote apply storage unavailable")
        }
    }
}

/// Applies one TypeScript-authenticated Note plaintext.  The caller never
/// supplies a capability, and every mutable SQLite fact is re-checked inside
/// the immediate transaction owned by the privileged connection.
pub(crate) fn record_content_note_blocker(
    connection: &mut PrivilegedRemoteApplyConnection,
    command: &ApplyVerifiedReceivedNoteIpcCommand,
    code: &str,
) -> Result<(), NoteSyncError> {
    if !crate::frame_compression::CODES.contains(&code)
        && !matches!(code, "decrypt_failed" | "invalid_note_payload")
    {
        return Err(NoteSyncError::InvalidEnvelope("invalid content blocker"));
    }
    connection.execute_planned_once(|tx| {
        crate::content_note_sync::receipt(tx,command,None)?;
        tx.execute("UPDATE cloud_content_note_receipts SET blocker=?3 WHERE account_id=?1 AND event_id=?2 AND outcome='waiting'",rusqlite::params![command.account_id,command.event_id,code])?;
        tx.execute("UPDATE cloud_sync_inbox SET state='orphan',error_code=?3 WHERE account_id=?1 AND event_id=?2 AND state IN ('received','orphan')",rusqlite::params![command.account_id,command.event_id,code])?;
        Ok((None,()))
    },|_,_|Ok(())).map_err(remote_apply_storage_error)
}

/// Explicit mode-3 framed Note route; no legacy decoder fallback.
pub(crate) fn apply_verified_received_content_note_ipc(
    connection: &mut PrivilegedRemoteApplyConnection,
    mut command: ApplyVerifiedReceivedNoteIpcCommand,
) -> Result<String, NoteSyncError> {
    let decoded = crate::content_note_sync::decode(&command.plaintext);
    let result = match decoded {
        Err(code) => connection.execute_planned_once(
            |tx| {
                let frame = if command.plaintext.len() <= 8388608 { Some(command.plaintext.as_slice()) } else { None };
                crate::content_note_sync::receipt(tx, &command, frame)?;
                tx.execute("UPDATE cloud_content_note_receipts SET blocker=?3 WHERE account_id=?1 AND event_id=?2 AND outcome='waiting'",rusqlite::params![command.account_id,command.event_id,code])?;
                tx.execute("UPDATE cloud_sync_inbox SET state='orphan',error_code=?3 WHERE account_id=?1 AND event_id=?2 AND state IN ('received','orphan')",rusqlite::params![command.account_id,command.event_id,code])?;
                Ok((None,code.to_string()))
            }, |_,code| Ok(code),
        ).map_err(remote_apply_storage_error),
        Ok(mut event) => {
            if let Some(plaintext) = event.ordinary.take() {
                let verified = ApplyVerifiedReceivedNoteCommand {
                    account_id:command.account_id.clone(),canonical_user_id:command.canonical_user_id.clone(),pulling_device_id:command.pulling_device_id.clone(),event_id:command.event_id.clone(),server_sequence:command.server_sequence,source_device_id:command.source_device_id.clone(),crypto_version:command.crypto_version,aad_version:command.aad_version,nonce:command.nonce.clone(),ciphertext:command.ciphertext.clone(),plaintext,
                };
                // Keep the parsed route available to the common transaction hook.
                event.ordinary=Some(verified.plaintext.clone());
                apply_received_note_with_content(connection,&verified,Some((&event,&command)))
                    .map(|r|serde_json::to_value(r).unwrap().as_str().unwrap().to_string())
            } else {
                let canonical_payload=serde_json::to_vec(&event.root["event"]).unwrap();
                let resolution=crate::note_sync_plaintext::decode_note_sync_resolution_v2(&canonical_payload).unwrap();
                let verified=VerifiedPeerResolutionCommand {
                    account_id:command.account_id.clone(),canonical_user_id:command.canonical_user_id.clone(),pulling_device_id:command.pulling_device_id.clone(),event_id:command.event_id.clone(),server_sequence:command.server_sequence,source_device_id:command.source_device_id.clone(),crypto_version:command.crypto_version,aad_version:command.aad_version,nonce:command.nonce.clone(),ciphertext:command.ciphertext.clone(),canonical_payload,resolution,
                };
                if command.source_device_id==command.pulling_device_id {
                    reconcile_resolution_with_content(connection,&verified,SelfEchoFailurePoint::None,Some((&event,&command)))
                        .map(|_|"applied".to_string()).map_err(|_|NoteSyncError::InvalidEnvelope("content Note self echo rejected"))
                }else{
                    apply_resolution_with_content(connection,&verified,false,false,Some((&event,&command)))
                        .map(|r|serde_json::to_value(r).unwrap().as_str().unwrap().to_string())
                        .map_err(|_|NoteSyncError::InvalidEnvelope("content Note resolution rejected"))
                }
            }
        }
    };
    command.plaintext.fill(0);
    result
}

pub(crate) fn apply_verified_received_note_ipc(
    connection: &mut PrivilegedRemoteApplyConnection,
    mut command: ApplyVerifiedReceivedNoteIpcCommand,
) -> Result<ApplyVerifiedReceivedNoteResult, NoteSyncError> {
    // This clears Rust's command-owned mutable byte copy on both success and
    // decode failure. Tauri/serde and JavaScript may retain independent copies.
    let plaintext = decode_note_sync_plaintext(&command.plaintext)
        .map_err(|_| NoteSyncError::InvalidEnvelope("invalid verified Note plaintext"));
    command.plaintext.fill(0);
    let plaintext = plaintext?;
    apply_verified_received_note(
        connection,
        &ApplyVerifiedReceivedNoteCommand {
            account_id: command.account_id,
            canonical_user_id: command.canonical_user_id,
            pulling_device_id: command.pulling_device_id,
            event_id: command.event_id,
            server_sequence: command.server_sequence,
            source_device_id: command.source_device_id,
            crypto_version: command.crypto_version,
            aad_version: command.aad_version,
            nonce: command.nonce,
            ciphertext: command.ciphertext,
            plaintext,
        },
    )
}

pub(crate) fn apply_verified_received_note(
    connection: &mut PrivilegedRemoteApplyConnection,
    command: &ApplyVerifiedReceivedNoteCommand,
) -> Result<ApplyVerifiedReceivedNoteResult, NoteSyncError> {
    apply_received_note_with_content(connection, command, None)
}
fn apply_received_note_with_content(
    connection: &mut PrivilegedRemoteApplyConnection,
    command: &ApplyVerifiedReceivedNoteCommand,
    content: Option<(
        &crate::content_note_sync::ContentNote,
        &ApplyVerifiedReceivedNoteIpcCommand,
    )>,
) -> Result<ApplyVerifiedReceivedNoteResult, NoteSyncError> {
    let incoming = verified_incoming_note(&command.plaintext)?;
    if command.event_id != incoming.event_id {
        return Err(NoteSyncError::InvalidEnvelope("plaintext event mismatch"));
    }

    connection
        .execute_planned_once(
            |transaction| {
                validate_pull_scope(
                    transaction,
                    &command.account_id,
                    &command.pulling_device_id,
                    &command.canonical_user_id,
                )
                .map_err(|error| StorageError::RemoteApplyAuthorization(error.to_string()))?;

                let inbox: Option<(i64, String, String, String, String, i64, String, Option<String>, String)> = transaction
                    .query_row(
                        "SELECT server_sequence,device_id,project_id,entity_id,operation,
                                sync_revision,updated_at,deleted_at,state
                         FROM cloud_sync_inbox
                         WHERE account_id=?1 AND event_id=?2 AND entity_type='note'",
                        rusqlite::params![command.account_id, command.event_id],
                        |row| Ok((
                            row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?,
                            row.get(5)?, row.get(6)?, row.get(7)?, row.get(8)?,
                        )),
                    )
                    .optional()?;
                let Some((server_sequence, source_device_id, project_id, entity_id, operation,
                    revision, updated_at, deleted_at, inbox_state)) = inbox else {
                    return Err(StorageError::RemoteApplyAuthorization(
                        "received inbox event is missing".to_string(),
                    ));
                };
                let immutable_matches = server_sequence == command.server_sequence
                    && source_device_id == command.source_device_id
                    && project_id == incoming.project_id
                    && entity_id == incoming.entity_id
                    && operation == if content.is_some() { "event" } else { &incoming.operation }
                    && revision == incoming.revision
                    && note_sync_timestamps_equal(&updated_at, &incoming.updated_at)
                    && if content.is_some() { deleted_at.is_none() } else { match (deleted_at.as_deref(), incoming.deleted_at.as_deref()) {
                        (None, None) => true,
                        (Some(stored), Some(opened)) => note_sync_timestamps_equal(stored, opened),
                        _ => false,
                    }};
                if !immutable_matches {
                    return classify_remote_apply(
                        transaction,
                        command,
                        ApplyVerifiedReceivedNoteResult::Rejected,
                        "rejected",
                        "metadata_mismatch",
                    )
                    .map_err(Into::into);
                }
                let object: Option<(i64, i64, Vec<u8>, Vec<u8>)> = transaction
                    .query_row(
                        "SELECT crypto_version,aad_version,nonce,ciphertext
                         FROM cloud_sync_event_objects WHERE account_id=?1 AND event_id=?2",
                        rusqlite::params![command.account_id, command.event_id],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                    )
                    .optional()?;
                let expected_object = (
                    command.crypto_version,
                    command.aad_version,
                    command.nonce.clone(),
                    command.ciphertext.clone(),
                );
                let object_error = match object.as_ref() {
                    None => Some("missing_encrypted_object"),
                    Some(actual) if actual != &expected_object => Some("metadata_mismatch"),
                    Some(_) => None,
                };
                if let Some(error_code) = object_error {
                    return classify_remote_apply(
                        transaction,
                        command,
                        ApplyVerifiedReceivedNoteResult::Rejected,
                        "rejected",
                        error_code,
                    )
                    .map_err(Into::into);
                }
                if let Some((event, raw)) = content {
                    crate::content_note_sync::receipt(transaction, raw, Some(&event.frame))?;
                    // Completed immutable replay does not re-open dependencies.
                    if !matches!(inbox_state.as_str(), "applied" | "conflict_preserved") {
                        if let Err(code) = crate::content_note_sync::ready(transaction, &command.account_id, &command.canonical_user_id, &command.source_device_id, event) {
                            return classify_remote_apply(transaction, command, ApplyVerifiedReceivedNoteResult::Orphan, "orphan", &code).map_err(Into::into);
                        }
                    }
                }
                if inbox_state == "applied" {
                    if !applied_remote_history_matches(transaction, command, &incoming)? {
                        return Err(StorageError::RemoteApplyAuthorization(
                            "conflicting applied event replay".to_string(),
                        ));
                    }
                    return Ok((
                        None,
                        RemoteApplyPlan {
                            result: ApplyVerifiedReceivedNoteResult::AlreadyApplied,
                            mutation: RemoteNoteMutation::None,
                            account_id: command.account_id.clone(),
                            project_id,
                            entity_id,
                            event_id: command.event_id.clone(),
                            revision,
                            updated_at,
                        },
                    ));
                }
                if inbox_state == "conflict_preserved" {
                    if !preserved_remote_version_matches(transaction, command, &incoming)? {
                        return Err(StorageError::RemoteApplyAuthorization(
                            "conflicting preserved event replay".to_string(),
                        ));
                    }
                    return Ok((
                        None,
                        RemoteApplyPlan {
                            result: ApplyVerifiedReceivedNoteResult::Conflict,
                            mutation: RemoteNoteMutation::None,
                            account_id: command.account_id.clone(),
                            project_id,
                            entity_id,
                            event_id: command.event_id.clone(),
                            revision,
                            updated_at,
                        },
                    ));
                }
                if inbox_state != "received" && inbox_state != "orphan" {
                    return Ok((
                        None,
                        RemoteApplyPlan {
                            result: match inbox_state.as_str() {
                                "orphan" => ApplyVerifiedReceivedNoteResult::Orphan,
                                "conflict" => ApplyVerifiedReceivedNoteResult::Conflict,
                                _ => ApplyVerifiedReceivedNoteResult::Rejected,
                            },
                            mutation: RemoteNoteMutation::None,
                            account_id: command.account_id.clone(),
                            project_id,
                            entity_id,
                            event_id: command.event_id.clone(),
                            revision,
                            updated_at,
                        },
                    ));
                }
                if transaction
                    .query_row(
                        "SELECT 1 FROM projects AS project
                         JOIN cloud_sync_project_bindings AS binding
                           ON binding.project_id=project.id
                         WHERE project.id=?1 AND binding.account_id=?2",
                        rusqlite::params![incoming.project_id, command.account_id],
                        |_| Ok(()),
                    )
                    .optional()?
                    .is_none()
                {
                    return classify_remote_apply(
                        transaction,
                        command,
                        ApplyVerifiedReceivedNoteResult::Orphan,
                        "orphan",
                        "missing_project",
                    )
                    .map_err(Into::into);
                }
                match if content.is_some() { Eligibility::Eligible } else { eligibility(&incoming.route) } {
                    Eligibility::DependencyNotSynced => {
                        return classify_remote_apply(transaction, command,
                            ApplyVerifiedReceivedNoteResult::Orphan, "orphan", "dependency_not_synced")
                            .map_err(Into::into);
                    }
                    Eligibility::UnsupportedContentFormat => {
                        return classify_remote_apply(transaction, command,
                            ApplyVerifiedReceivedNoteResult::Rejected, "rejected", "unsupported_content_format")
                            .map_err(Into::into);
                    }
                    Eligibility::Eligible => {}
                }

                let self_echo_timestamps = if source_device_id == command.pulling_device_id {
                    transaction.query_row(
                        "SELECT outbox.updated_at,outbox.deleted_at FROM cloud_sync_outbox AS outbox
                         JOIN cloud_sync_upload_receipts AS receipt
                           ON receipt.event_id=outbox.event_id
                         WHERE outbox.event_id=?1 AND outbox.account_id=?2
                           AND outbox.device_id=?3 AND receipt.account_id=?2
                           AND receipt.device_id=?3 AND receipt.server_sequence=?4
                           AND outbox.project_id=?5 AND outbox.entity_id=?6
                           AND outbox.entity_type='note' AND outbox.operation=?7
                           AND outbox.revision=?8",
                        rusqlite::params![
                            command.event_id, command.account_id, command.pulling_device_id,
                            command.server_sequence, incoming.project_id, incoming.entity_id,
                            incoming.operation, incoming.revision,
                        ],
                        |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
                    ).optional()?
                } else {
                    None
                };
                let self_echo = self_echo_timestamps.is_some_and(|(updated_at, deleted_at)| {
                    note_sync_timestamps_equal(&updated_at, &incoming.updated_at)
                        && match (deleted_at.as_deref(), incoming.deleted_at.as_deref()) {
                            (None, None) => true,
                            (Some(stored), Some(opened)) => note_sync_timestamps_equal(stored, opened),
                            _ => false,
                        }
                });
                if self_echo {
                    let head: Option<(String, i64)> = transaction.query_row(
                        "SELECT head_event_id,head_sync_revision FROM cloud_sync_entities
                         WHERE account_id=?1 AND project_id=?2 AND entity_id=?3 AND entity_type='note'",
                        rusqlite::params![command.account_id, incoming.project_id, incoming.entity_id],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    ).optional()?;
                    if head.as_ref().is_some_and(|(event_id, value)| {
                        *value == incoming.revision && event_id != &incoming.event_id
                    }) {
                        return classify_remote_apply(transaction, command,
                            ApplyVerifiedReceivedNoteResult::Conflict, "conflict", "conflicting_head")
                            .map_err(Into::into);
                    }
                    let plan = RemoteApplyPlan {
                        result: ApplyVerifiedReceivedNoteResult::SelfEchoApplied,
                        mutation: RemoteNoteMutation::None,
                        account_id: command.account_id.clone(),
                        project_id: incoming.project_id.clone(), entity_id: incoming.entity_id.clone(),
                        event_id: incoming.event_id.clone(), revision: incoming.revision,
                        updated_at: incoming.updated_at.clone(),
                    };
                    if head.as_ref().is_none_or(|(_, value)| *value < incoming.revision) {
                        advance_remote_entity_head(transaction, &plan)?;
                    }
                    record_remote_note_causal_history(transaction, command, &incoming)?;
                    update_received_inbox_state(transaction, &command.account_id, &command.event_id,
                        "applied", None, Some(&incoming.updated_at))?;
                    return Ok((None, plan));
                }

                let import_choice=if content.is_some() && incoming.revision==1 && source_device_id!=command.pulling_device_id {
                    let existing:Option<String>=transaction.query_row("SELECT payload_json FROM notes WHERE id=?1 AND project_id=?2",rusqlite::params![incoming.entity_id,incoming.project_id],|r|r.get(0)).optional()?;
                    let authenticated:bool=transaction.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_note_causal_history WHERE account_id=?1 AND project_id=?2 AND entity_id=?3)",rusqlite::params![command.account_id,incoming.project_id,incoming.entity_id],|r|r.get(0))?;
                    if let Some(local)=existing.filter(|_|!authenticated){
                        transaction.execute("INSERT INTO cloud_content_note_local_candidates VALUES(?1,?2,?3,?4,?5) ON CONFLICT(account_id,event_id) DO NOTHING",rusqlite::params![command.account_id,command.event_id,incoming.project_id,incoming.entity_id,local])?;
                        let expected:Option<String>=transaction.query_row("SELECT expected_local FROM cloud_content_note_import_decisions WHERE account_id=?1 AND event_id=?2 ORDER BY rowid DESC LIMIT 1",rusqlite::params![command.account_id,command.event_id],|r|r.get(0)).optional()?;
                        if expected.as_deref()!=Some(local.as_str()) {
                            return classify_remote_apply(transaction,command,ApplyVerifiedReceivedNoteResult::Conflict,"conflict","content_note_local_candidate").map_err(Into::into);
                        }
                        true
                    }else{false}
                }else{false};
                let local_unsealed = transaction.query_row(
                    "SELECT 1 FROM cloud_sync_outbox
                     WHERE account_id=?1 AND project_id=?2 AND entity_id=?3
                       AND entity_type='note' AND lifecycle='unsealed' AND local_ordinal>0",
                    rusqlite::params![command.account_id, incoming.project_id, incoming.entity_id],
                    |_| Ok(()),
                ).optional()?.is_some();
                if local_unsealed && !import_choice {
                    if let Some(plan) = preserve_causal_note_conflict(transaction, command, &incoming)? {
                        return Ok((None, plan));
                    }
                    return classify_remote_apply(transaction, command,
                        ApplyVerifiedReceivedNoteResult::Conflict, "conflict", "local_unsealed_change")
                        .map_err(Into::into);
                }
                if let Some(plan) = preserve_causal_note_conflict(transaction, command, &incoming)? {
                    return Ok((None, plan));
                }
                let head = match read_entity_sync_head(
                    transaction, &command.account_id, &incoming.project_id, &incoming.entity_id,
                ) {
                    Ok(head) => head,
                    Err(NoteSyncError::ConflictingHeads) => {
                        return classify_remote_apply(transaction, command,
                            ApplyVerifiedReceivedNoteResult::Conflict, "conflict", "conflicting_head")
                            .map_err(Into::into);
                    }
                    Err(NoteSyncError::Database(error)) => return Err(StorageError::Database(error)),
                    Err(error) => return Err(StorageError::RemoteApplyAuthorization(error.to_string())),
                };
                let resolution_head_proven = match &head {
                    Some(head) if is_resolution_event(transaction, &head.event_id)
                        .map_err(|error| StorageError::RemoteApplyAuthorization(error.to_string()))? =>
                            applied_resolution_parent_revision(transaction,
                                &command.account_id, &incoming.project_id,
                                &incoming.entity_id, &head.event_id)
                                .map_err(|error| StorageError::RemoteApplyAuthorization(error.to_string()))? == Some(head.revision),
                    _ => true,
                };
                let chain_valid = if incoming.revision == 1 {
                    incoming.parent_event_id.is_none() && head.is_none()
                } else {
                    resolution_head_proven && head.as_ref().is_some_and(|head| {
                        head.revision == incoming.revision - 1
                            && incoming.parent_event_id.as_deref() == Some(&head.event_id)
                    })
                };
                if !chain_valid {
                    let is_missing_parent = incoming.revision > 1
                        && head.as_ref().is_none_or(|head| head.revision < incoming.revision - 1);
                    return classify_remote_apply(transaction, command,
                        if is_missing_parent { ApplyVerifiedReceivedNoteResult::Orphan } else { ApplyVerifiedReceivedNoteResult::Conflict },
                        if is_missing_parent { "orphan" } else { "conflict" },
                        if is_missing_parent { "missing_parent" } else { "conflicting_head" })
                        .map_err(Into::into);
                }
                let existing: Option<(String, String)> = transaction.query_row(
                    "SELECT project_id,payload_json FROM notes WHERE id=?1", [&incoming.entity_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                ).optional()?;
                if existing.as_ref().is_some_and(|(project_id, _)| project_id != &incoming.project_id) {
                    return classify_remote_apply(transaction, command,
                        ApplyVerifiedReceivedNoteResult::Conflict, "conflict", "conflicting_head")
                        .map_err(Into::into);
                }
                if incoming.revision == 1 && existing.is_some() && !import_choice {
                    return classify_remote_apply(transaction, command,
                        ApplyVerifiedReceivedNoteResult::Conflict, "conflict", "conflicting_head")
                        .map_err(Into::into);
                }
                let (mutation, authorization) = match (&incoming.payload_json, existing) {
                    (Some(payload_json), None) => (
                        RemoteNoteMutation::Insert { payload_json: payload_json.clone(), updated_at: incoming.updated_at.clone() },
                        Some(OwnedRemoteApplyAuthorization {
                            event_id: incoming.event_id.clone(), account_id: command.account_id.clone(),
                            project_id: incoming.project_id.clone(), entity_id: incoming.entity_id.clone(),
                            operation: "upsert".to_string(), payload_json: Some(payload_json.clone()),
                            prior_payload_json: None,
                        }),
                    ),
                    (Some(payload_json), Some((_, prior_payload_json))) => (
                        RemoteNoteMutation::Update { payload_json: payload_json.clone(), updated_at: incoming.updated_at.clone() },
                        Some(OwnedRemoteApplyAuthorization {
                            event_id: incoming.event_id.clone(), account_id: command.account_id.clone(),
                            project_id: incoming.project_id.clone(), entity_id: incoming.entity_id.clone(),
                            operation: "upsert".to_string(), payload_json: Some(payload_json.clone()),
                            prior_payload_json: Some(prior_payload_json),
                        }),
                    ),
                    (None, Some((_, prior_payload_json))) => (
                        RemoteNoteMutation::Delete,
                        Some(OwnedRemoteApplyAuthorization {
                            event_id: incoming.event_id.clone(), account_id: command.account_id.clone(),
                            project_id: incoming.project_id.clone(), entity_id: incoming.entity_id.clone(),
                            operation: "delete".to_string(), payload_json: None,
                            prior_payload_json: Some(prior_payload_json),
                        }),
                    ),
                    (None, None) => (RemoteNoteMutation::None, None),
                };
                record_remote_note_causal_history(transaction, command, &incoming)?;
                Ok((authorization, RemoteApplyPlan {
                    result: ApplyVerifiedReceivedNoteResult::Applied, mutation,
                    account_id: command.account_id.clone(), project_id: incoming.project_id.clone(),
                    entity_id: incoming.entity_id.clone(), event_id: incoming.event_id.clone(),
                    revision: incoming.revision, updated_at: incoming.updated_at.clone(),
                }))
            },
            |transaction, plan| {
                match &plan.mutation {
                    RemoteNoteMutation::Insert { payload_json, updated_at } => {
                        transaction.execute(
                            "INSERT INTO notes(id,project_id,stage_id,updated_at,payload_json)
                             VALUES(?1,?2,json_extract(?4,'$.stage_id'),?3,?4)",
                            rusqlite::params![plan.entity_id, plan.project_id, updated_at, payload_json],
                        )?;
                    }
                    RemoteNoteMutation::Update { payload_json, updated_at } => {
                        let changed = transaction.execute(
                            "UPDATE notes SET updated_at=?1,payload_json=?2,stage_id=json_extract(?2,'$.stage_id')
                             WHERE id=?3 AND project_id=?4",
                            rusqlite::params![updated_at, payload_json, plan.entity_id, plan.project_id],
                        )?;
                        if changed != 1 {
                            return Err(StorageError::RemoteApplyAuthorization(
                                "remote Note update target disappeared".to_string(),
                            ));
                        }
                    }
                    RemoteNoteMutation::Delete => {
                        transaction.execute(
                            "DELETE FROM notes WHERE id=?1 AND project_id=?2",
                            rusqlite::params![plan.entity_id, plan.project_id],
                        )?;
                    }
                    RemoteNoteMutation::None => {}
                }
                if plan.result == ApplyVerifiedReceivedNoteResult::Applied {
                    advance_remote_entity_head(transaction, &plan)?;
                    update_received_inbox_state(
                        transaction, &plan.account_id, &plan.event_id, "applied", None,
                        Some(&plan.updated_at),
                    )?;
                }
                if content.is_some() {
                    crate::content_note_sync::complete(transaction, &plan.account_id, &plan.event_id)?;
                    if plan.result==ApplyVerifiedReceivedNoteResult::Applied {
                        crate::content_note_writer::finish_local_import(transaction,&plan.account_id,&plan.event_id).map_err(StorageError::RemoteApplyAuthorization)?;
                    }
                }
                Ok(plan.result)
            },
        )
        .map_err(remote_apply_storage_error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    const DEVICE_ID: &str = "123e4567-e89b-42d3-a456-426614174000";

    fn database() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        crate::sqlite::apply_migrations(&connection).unwrap();
        configure_database(&connection);
        connection
    }

    fn inbound_scope(connection: &mut Connection) {
        connection.execute("INSERT OR IGNORE INTO cloud_sync_state(account_id,device_id,pull_cursor,ack_cursor,created_at,updated_at) VALUES('inbox-account',?1,0,0,'now','now')", [DEVICE_ID]).unwrap();
        connection.execute("INSERT OR IGNORE INTO cloud_account_bindings(local_account_id,canonical_user_id,created_at,validated_at) VALUES('inbox-account','abcdefab-0000-0000-0000-000000000101','now','now')", []).unwrap();
    }

    fn inbound_command() -> CommitNoteSyncInboundPageCommand {
        CommitNoteSyncInboundPageCommand {
            account_id: "inbox-account".into(),
            device_id: DEVICE_ID.into(),
            canonical_user_id: "abcdefab-0000-0000-0000-000000000101".into(),
            expected_cursor: 0,
            next_cursor: 1,
            has_more: false,
            items: vec![InboundNoteSyncItem {
                event_id: "123e4567-e89b-42d3-a456-426614174099".into(),
                server_sequence: 1,
                source_device_id: DEVICE_ID.into(),
                project_id: "project".into(),
                entity_id: "note".into(),
                entity_type: "note".into(),
                operation: "upsert".into(),
                revision: 1,
                updated_at: "2026-09-22T00:00:00Z".into(),
                deleted_at: None,
                envelope: Some(EncryptedNoteSyncEnvelope {
                    crypto_version: 1,
                    aad_version: 1,
                    nonce: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into(),
                    ciphertext: "AAAAAAAAAAAAAAAAAAAAAA".into(),
                }),
            }],
        }
    }

    fn received_list_command(limit: u32) -> ListReceivedNoteSyncInboxCommand {
        ListReceivedNoteSyncInboxCommand {
            account_id: "inbox-account".into(),
            device_id: DEVICE_ID.into(),
            canonical_user_id: "abcdefab-0000-0000-0000-000000000101".into(),
            limit,
        }
    }

    fn received_page_command(
        kind: ReceivedNoteSyncInboxPageKind,
        limit: u32,
        after_server_sequence: i64,
    ) -> ListReceivedNoteSyncInboxPageCommand {
        ListReceivedNoteSyncInboxPageCommand {
            account_id: "inbox-account".into(),
            device_id: DEVICE_ID.into(),
            canonical_user_id: "abcdefab-0000-0000-0000-000000000101".into(),
            kind,
            limit,
            after_server_sequence,
        }
    }

    fn insert_received_item(connection: &mut Connection, sequence: i64, operation: &str) {
        let mut command = inbound_command();
        command.expected_cursor = sequence - 1;
        command.next_cursor = sequence;
        command.items[0].event_id = format!("123e4567-e89b-42d3-a456-426614174{sequence:03}");
        command.items[0].server_sequence = sequence;
        command.items[0].operation = operation.into();
        if operation == "resolution" {
            command.items[0].revision = 2;
        }
        if operation == "delete" {
            command.items[0].deleted_at = Some("2026-09-22T00:00:01Z".into());
        }
        commit_note_sync_inbound_page(connection, &command).unwrap();
    }

    fn orphan_resolution_list_command(
        limit: u32,
        after_server_sequence: i64,
    ) -> ListOrphanNoteResolutionInboxCommand {
        ListOrphanNoteResolutionInboxCommand {
            account_id: "inbox-account".into(),
            device_id: DEVICE_ID.into(),
            canonical_user_id: "abcdefab-0000-0000-0000-000000000101".into(),
            limit,
            after_server_sequence,
        }
    }

    fn insert_orphan_resolution(connection: &mut Connection, sequence: i64) {
        insert_received_item(connection, sequence, "resolution");
        connection.execute(
            "UPDATE cloud_sync_inbox SET state='orphan' WHERE account_id='inbox-account' AND server_sequence=?1",
            [sequence],
        ).unwrap();
    }

    fn ack_prepare_command() -> PrepareNoteSyncAckCommand {
        PrepareNoteSyncAckCommand {
            account_id: "inbox-account".into(),
            device_id: DEVICE_ID.into(),
            canonical_user_id: "abcdefab-0000-0000-0000-000000000101".into(),
        }
    }

    fn ack_commit_command(
        expected_old_ack_cursor: i64,
        acknowledged_cursor: i64,
    ) -> CommitNoteSyncAckCommand {
        CommitNoteSyncAckCommand {
            account_id: "inbox-account".into(),
            device_id: DEVICE_ID.into(),
            canonical_user_id: "abcdefab-0000-0000-0000-000000000101".into(),
            expected_old_ack_cursor,
            acknowledged_cursor,
        }
    }

    fn set_ack_pull_cursor(connection: &Connection, pull_cursor: i64) {
        connection
            .execute(
                "UPDATE cloud_sync_state SET pull_cursor=?1 WHERE account_id='inbox-account'",
                [pull_cursor],
            )
            .unwrap();
    }

    fn insert_ack_inbox(connection: &Connection, sequence: i64, state: &str) {
        connection.execute(
            "INSERT INTO cloud_sync_inbox(account_id,event_id,server_sequence,device_id,project_id,entity_id,entity_type,operation,sync_revision,updated_at,deleted_at,state,received_at) VALUES('inbox-account',?1,?2,?3,'project',?4,'note','upsert',1,'2026-09-22T00:00:00Z',NULL,?5,'now')",
            rusqlite::params![format!("123e4567-e89b-42d3-a456-426614174{sequence:03}"), sequence, DEVICE_ID, format!("note-{sequence}"), state],
        ).unwrap();
    }

    #[test]
    fn inbound_page_is_atomic_idempotent_and_advances_only_pull_cursor() {
        let mut connection = database();
        inbound_scope(&mut connection);
        let command = inbound_command();
        assert_eq!(
            commit_note_sync_inbound_page(&mut connection, &command)
                .unwrap()
                .new_events,
            1
        );
        assert_eq!(connection.query_row("SELECT pull_cursor,ack_cursor FROM cloud_sync_state WHERE account_id='inbox-account'", [], |row| Ok((row.get::<_, i64>(0)?,row.get::<_, i64>(1)?))).unwrap(), (1, 0));
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM cloud_sync_inbox", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            commit_note_sync_inbound_page(&mut connection, &command)
                .unwrap()
                .replayed_events,
            1
        );
    }

    #[test]
    fn inbound_resolution_shares_stream_is_immutable_and_blocks_ack() {
        let mut connection = database();
        inbound_scope(&mut connection);
        let mut page = inbound_command();
        page.next_cursor = 3;
        page.has_more = false;
        let mut resolution = page.items[0].clone();
        resolution.event_id = "123e4567-e89b-42d3-a456-426614174098".into();
        resolution.server_sequence = 2;
        resolution.operation = "resolution".into();
        resolution.revision = 2;
        let mut deleted = page.items[0].clone();
        deleted.event_id = "123e4567-e89b-42d3-a456-426614174097".into();
        deleted.server_sequence = 3;
        deleted.operation = "delete".into();
        deleted.deleted_at = Some("2026-09-22T00:00:01Z".into());
        page.items = vec![page.items[0].clone(), resolution.clone(), deleted];
        assert_eq!(
            commit_note_sync_inbound_page(&mut connection, &page)
                .unwrap()
                .new_events,
            3
        );
        assert_eq!(
            commit_note_sync_inbound_page(&mut connection, &page)
                .unwrap()
                .replayed_events,
            3
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT pull_cursor FROM cloud_sync_state WHERE account_id='inbox-account'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            3
        );
        assert_eq!(
            list_received_note_sync_inbox(&connection, &received_list_command(8)).unwrap()[1]
                .operation,
            "resolution"
        );
        assert_eq!(
            prepare_note_sync_ack(&mut connection, &ack_prepare_command())
                .unwrap()
                .candidate_cursor,
            0
        );
        assert!(connection.execute("UPDATE cloud_sync_inbox SET project_id='other' WHERE account_id='inbox-account' AND event_id=?1", [&resolution.event_id]).is_err());
        assert!(connection.execute("DELETE FROM cloud_sync_event_objects WHERE account_id='inbox-account' AND event_id=?1", [&resolution.event_id]).is_err());
        assert!(connection.execute("UPDATE cloud_sync_event_objects SET ciphertext=X'01' WHERE account_id='inbox-account' AND event_id=?1", [&resolution.event_id]).is_err());
        let mut changed = page.clone();
        changed.items[1].updated_at = "2026-09-22T00:00:02Z".into();
        assert!(commit_note_sync_inbound_page(&mut connection, &changed).is_err());
        let mut changed_object = page.clone();
        changed_object.items[1]
            .envelope
            .as_mut()
            .unwrap()
            .ciphertext = "AQAAAAAAAAAAAAAAAAAAAA".into();
        assert!(commit_note_sync_inbound_page(&mut connection, &changed_object).is_err());
        let mut partial = page.clone();
        partial.items.remove(0);
        assert!(commit_note_sync_inbound_page(&mut connection, &partial).is_err());
        assert_eq!(
            connection
                .query_row(
                    "SELECT pull_cursor FROM cloud_sync_state WHERE account_id='inbox-account'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            3
        );
        assert!(connection.execute("UPDATE cloud_sync_inbox SET operation='resolution',sync_revision=2 WHERE account_id='inbox-account' AND event_id=?1", [&page.items[0].event_id]).is_err());
        connection.execute("UPDATE cloud_sync_inbox SET state='orphan' WHERE account_id='inbox-account' AND event_id=?1", [&resolution.event_id]).unwrap();
        assert_eq!(
            connection
                .query_row(
                    "SELECT state FROM cloud_sync_inbox WHERE event_id=?1",
                    [&resolution.event_id],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            "orphan"
        );
        assert!(connection.execute("UPDATE cloud_sync_inbox SET state='applied',applied_at='now' WHERE account_id='inbox-account' AND event_id=?1", [&resolution.event_id]).is_err());
        assert_eq!(
            connection
                .query_row(
                    "SELECT state FROM cloud_sync_inbox WHERE event_id=?1",
                    [&resolution.event_id],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            "orphan"
        );
    }

    fn seed_applied_resolution_proof(connection: &Connection) {
        connection
            .execute_batch(
                r#"
            UPDATE cloud_sync_state SET pull_cursor=3 WHERE account_id='inbox-account';
            INSERT INTO cloud_sync_note_conflict_groups(
                group_id,account_id,project_id,entity_id,entity_type,common_parent_event_id,
                tip_revision,generation,lifecycle,created_at,updated_at
            ) VALUES(
                'aaaaaaaa-0000-0000-0000-000000000001','inbox-account','project','note',
                'note','aaaaaaaa-0000-0000-0000-000000000002',2,7,'open','now','now'
            );
            INSERT INTO cloud_sync_note_conflict_versions VALUES(
                'left-version','aaaaaaaa-0000-0000-0000-000000000001','inbox-account',
                'project','note','note','aaaaaaaa-0000-0000-0000-000000000003',
                'aaaaaaaa-0000-0000-0000-000000000002',2,1,'upsert','{"body":"left"}',
                'remote',NULL,NULL,7,'now'
            );
            INSERT INTO cloud_sync_note_conflict_versions VALUES(
                'right-version','aaaaaaaa-0000-0000-0000-000000000001','inbox-account',
                'project','note','note','aaaaaaaa-0000-0000-0000-000000000004',
                'aaaaaaaa-0000-0000-0000-000000000002',2,2,'upsert','{"body":"right"}',
                'remote',NULL,NULL,7,'now'
            );
            INSERT INTO cloud_sync_note_conflict_tips VALUES(
                'aaaaaaaa-0000-0000-0000-000000000001','left-version',
                'aaaaaaaa-0000-0000-0000-000000000003',7
            );
            INSERT INTO cloud_sync_note_conflict_tips VALUES(
                'aaaaaaaa-0000-0000-0000-000000000001','right-version',
                'aaaaaaaa-0000-0000-0000-000000000004',7
            );
            INSERT INTO cloud_sync_inbox VALUES(
                'inbox-account','aaaaaaaa-0000-0000-0000-000000000003',1,
                'aaaaaaaa-0000-0000-0000-000000000010','project','note','note','upsert',2,
                'left-time',NULL,'conflict_preserved','now',NULL,NULL,
                'aaaaaaaa-0000-0000-0000-000000000001','now'
            );
            INSERT INTO cloud_sync_inbox VALUES(
                'inbox-account','aaaaaaaa-0000-0000-0000-000000000004',2,
                'aaaaaaaa-0000-0000-0000-000000000010','project','note','note','upsert',2,
                'right-time',NULL,'conflict_preserved','now',NULL,NULL,
                'aaaaaaaa-0000-0000-0000-000000000001','now'
            );
            INSERT INTO cloud_sync_inbox VALUES(
                'inbox-account','aaaaaaaa-0000-0000-0000-000000000005',3,
                'aaaaaaaa-0000-0000-0000-000000000010','project','note','note','resolution',3,
                'resolution-time',NULL,'received','now',NULL,NULL,NULL,NULL
            );
            INSERT INTO cloud_sync_event_objects VALUES(
                'inbox-account','aaaaaaaa-0000-0000-0000-000000000005',1,1,
                X'000000000000000000000000000000000000000000000000',
                X'00000000000000000000000000000000','now'
            );
        "#,
            )
            .unwrap();
    }

    fn insert_applying_resolution_proof(transaction: &Transaction<'_>) {
        transaction
            .execute_batch(
                r#"
            INSERT INTO cloud_sync_note_applied_resolutions VALUES(
                'inbox-account','aaaaaaaa-0000-0000-0000-000000000005',
                'aaaaaaaa-0000-0000-0000-000000000010',3,'project','note',3,
                'resolution-time','aaaaaaaa-0000-0000-0000-000000000006',
                'aaaaaaaa-0000-0000-0000-000000000001',19,7,
                '["aaaaaaaa-0000-0000-0000-000000000003","aaaaaaaa-0000-0000-0000-000000000004"]',
                2,'choose_version','upsert',X'63616e6f6e6963616c2d7632','{"body":"left"}',
                NULL,NULL,'applying',NULL,'now'
            );
            INSERT INTO cloud_sync_note_applied_resolution_parents VALUES(
                'inbox-account','aaaaaaaa-0000-0000-0000-000000000005',0,
                'aaaaaaaa-0000-0000-0000-000000000003','left-version',2,'upsert',
                '{"body":"left"}','remote',1,NULL,'now'
            );
        "#,
            )
            .unwrap();
    }

    #[test]
    fn applied_resolution_proof_is_atomic_and_controls_ack_eligibility() {
        let (root, path) = temporary_database_path("applied-resolution-proof");
        let mut connection = crate::sqlite::open_database(&path).unwrap();
        configure_database(&connection);
        inbound_scope(&mut connection);
        seed_applied_resolution_proof(&connection);
        assert_eq!(
            prepare_note_sync_ack(&mut connection, &ack_prepare_command())
                .unwrap()
                .candidate_cursor,
            2
        );

        {
            let transaction = connection.transaction().unwrap();
            insert_applying_resolution_proof(&transaction);
            transaction.rollback().unwrap();
        }
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM cloud_sync_note_applied_resolutions",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT state FROM cloud_sync_inbox WHERE server_sequence=3",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "received"
        );

        let transaction = connection.transaction().unwrap();
        insert_applying_resolution_proof(&transaction);
        transaction
            .execute_batch(
                r#"
            INSERT INTO cloud_sync_note_applied_resolution_parents VALUES(
                'inbox-account','aaaaaaaa-0000-0000-0000-000000000005',1,
                'aaaaaaaa-0000-0000-0000-000000000004','right-version',2,'upsert',
                '{"body":"right"}','remote',2,NULL,'now'
            );
            UPDATE cloud_sync_note_applied_resolutions
               SET lifecycle='applied',applied_at='now'
             WHERE account_id='inbox-account'
               AND resolution_event_id='aaaaaaaa-0000-0000-0000-000000000005';
            UPDATE cloud_sync_inbox SET state='applied',applied_at='now'
             WHERE account_id='inbox-account'
               AND event_id='aaaaaaaa-0000-0000-0000-000000000005';
        "#,
            )
            .unwrap();
        transaction.commit().unwrap();

        assert_eq!(
            prepare_note_sync_ack(&mut connection, &ack_prepare_command())
                .unwrap()
                .candidate_cursor,
            3
        );
        drop(connection);
        let mut connection = crate::sqlite::open_database(&path).unwrap();
        assert_eq!(
            prepare_note_sync_ack(&mut connection, &ack_prepare_command())
                .unwrap()
                .candidate_cursor,
            3
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT lifecycle FROM cloud_sync_note_applied_resolutions",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "applied"
        );
        assert_eq!(
            connection
                .query_row("PRAGMA foreign_key_check", [], |_| Ok(1_i64),)
                .optional()
                .unwrap(),
            None
        );
        drop(connection);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_resolution_rolls_back_the_complete_mixed_page() {
        let mut connection = database();
        inbound_scope(&mut connection);
        let mut page = inbound_command();
        page.next_cursor = 2;
        let mut resolution = page.items[0].clone();
        resolution.event_id = "123e4567-e89b-42d3-a456-426614174098".into();
        resolution.server_sequence = 2;
        resolution.operation = "resolution".into();
        resolution.revision = 1;
        page.items.push(resolution);
        assert!(commit_note_sync_inbound_page(&mut connection, &page).is_err());
        assert_eq!(
            connection
                .query_row(
                    "SELECT pull_cursor FROM cloud_sync_state WHERE account_id='inbox-account'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM cloud_sync_inbox WHERE account_id='inbox-account'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        assert_eq!(connection.query_row("SELECT COUNT(*) FROM cloud_sync_event_objects WHERE account_id='inbox-account'", [], |row| row.get::<_, i64>(0)).unwrap(), 0);

        page.items[1].revision = 2;
        page.items[1].envelope = None;
        assert!(commit_note_sync_inbound_page(&mut connection, &page).is_err());
        page.items[1].envelope = page.items[0].envelope.clone();
        page.items[1].entity_type = "document".into();
        assert!(commit_note_sync_inbound_page(&mut connection, &page).is_err());
        page.items[1].entity_type = "note".into();
        page.items[1].server_sequence = 1;
        assert!(commit_note_sync_inbound_page(&mut connection, &page).is_err());
        assert_eq!(
            connection
                .query_row(
                    "SELECT pull_cursor FROM cloud_sync_state WHERE account_id='inbox-account'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM cloud_sync_inbox WHERE account_id='inbox-account'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
    }

    #[test]
    fn received_inbox_reader_is_bounded_scoped_ordered_and_read_only() {
        let mut connection = database();
        inbound_scope(&mut connection);
        let mut first = inbound_command();
        first.items[0].event_id = "123e4567-e89b-42d3-a456-426614174098".into();
        first.items[0].server_sequence = 1;
        first.next_cursor = 1;
        commit_note_sync_inbound_page(&mut connection, &first).unwrap();
        let mut second = inbound_command();
        second.expected_cursor = 1;
        second.next_cursor = 2;
        second.items[0].server_sequence = 2;
        commit_note_sync_inbound_page(&mut connection, &second).unwrap();
        let before: (i64, i64, String) = connection.query_row(
            "SELECT pull_cursor,ack_cursor,state FROM cloud_sync_state JOIN cloud_sync_inbox ON cloud_sync_inbox.account_id=cloud_sync_state.account_id WHERE cloud_sync_state.account_id='inbox-account' ORDER BY server_sequence LIMIT 1",
            [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).unwrap();
        let listed = list_received_note_sync_inbox(&connection, &received_list_command(1)).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].server_sequence, 1);
        assert_eq!(listed[0].event_id, "123e4567-e89b-42d3-a456-426614174098");
        let after: (i64, i64, String) = connection.query_row(
            "SELECT pull_cursor,ack_cursor,state FROM cloud_sync_state JOIN cloud_sync_inbox ON cloud_sync_inbox.account_id=cloud_sync_state.account_id WHERE cloud_sync_state.account_id='inbox-account' ORDER BY server_sequence LIMIT 1",
            [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).unwrap();
        assert_eq!(after, before);
        assert!(list_received_note_sync_inbox(&connection, &received_list_command(33)).is_err());
        let mut wrong_device = received_list_command(1);
        wrong_device.device_id = "123e4567-e89b-42d3-a456-426614174111".into();
        assert!(list_received_note_sync_inbox(&connection, &wrong_device).is_err());
    }

    #[test]
    fn received_inbox_reader_requires_a_valid_durable_object() {
        let mut connection = database();
        inbound_scope(&mut connection);
        let command = inbound_command();
        commit_note_sync_inbound_page(&mut connection, &command).unwrap();
        connection
            .execute(
                "DELETE FROM cloud_sync_event_objects WHERE account_id='inbox-account'",
                [],
            )
            .unwrap();
        assert!(matches!(
            list_received_note_sync_inbox(&connection, &received_list_command(8)),
            Err(NoteSyncError::SealedObjectMissing)
        ));
    }

    #[test]
    fn received_inbox_pages_are_operation_fair_keyset_scoped_and_read_only() {
        let mut connection = database();
        inbound_scope(&mut connection);
        for sequence in 1..=32 {
            insert_received_item(&mut connection, sequence, "upsert");
        }
        insert_received_item(&mut connection, 33, "resolution");
        let before: (i64, i64, i64) = connection.query_row(
            "SELECT pull_cursor,ack_cursor,(SELECT count(*) FROM cloud_sync_inbox WHERE account_id='inbox-account' AND state='received') FROM cloud_sync_state WHERE account_id='inbox-account'",
            [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).unwrap();
        let v1 = list_received_note_sync_inbox_page(
            &connection,
            &received_page_command(ReceivedNoteSyncInboxPageKind::V1, 32, 0),
        )
        .unwrap();
        assert_eq!(v1.len(), 32);
        assert_eq!(v1[0].server_sequence, 1);
        assert_eq!(v1[31].server_sequence, 32);
        assert!(v1
            .iter()
            .all(|item| matches!(item.operation.as_str(), "upsert" | "delete")));
        let resolutions = list_received_note_sync_inbox_page(
            &connection,
            &received_page_command(ReceivedNoteSyncInboxPageKind::Resolution, 32, 0),
        )
        .unwrap();
        assert_eq!(resolutions.len(), 1);
        assert_eq!(resolutions[0].server_sequence, 33);
        assert_eq!(resolutions[0].operation, "resolution");
        let first_page = list_received_note_sync_inbox_page(
            &connection,
            &received_page_command(ReceivedNoteSyncInboxPageKind::V1, 1, 0),
        )
        .unwrap();
        let repeated = list_received_note_sync_inbox_page(
            &connection,
            &received_page_command(ReceivedNoteSyncInboxPageKind::V1, 1, 0),
        )
        .unwrap();
        assert_eq!(first_page[0].event_id, repeated[0].event_id);
        assert_eq!(
            list_received_note_sync_inbox_page(
                &connection,
                &received_page_command(ReceivedNoteSyncInboxPageKind::V1, 1, 1)
            )
            .unwrap()[0]
                .server_sequence,
            2
        );
        let after: (i64, i64, i64) = connection.query_row(
            "SELECT pull_cursor,ack_cursor,(SELECT count(*) FROM cloud_sync_inbox WHERE account_id='inbox-account' AND state='received') FROM cloud_sync_state WHERE account_id='inbox-account'",
            [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).unwrap();
        assert_eq!(after, before);
        assert!(
            list_received_note_sync_inbox(&connection, &received_list_command(32))
                .unwrap()
                .iter()
                .all(|item| item.operation != "resolution")
        );
        assert!(list_orphan_note_resolution_inbox(
            &connection,
            &orphan_resolution_list_command(32, 0)
        )
        .unwrap()
        .is_empty());
        assert!(list_received_note_sync_inbox_page(
            &connection,
            &received_page_command(ReceivedNoteSyncInboxPageKind::V1, 33, 0)
        )
        .is_err());
        assert!(list_received_note_sync_inbox_page(
            &connection,
            &received_page_command(ReceivedNoteSyncInboxPageKind::V1, 1, -1)
        )
        .is_err());
        let mut wrong_device = received_page_command(ReceivedNoteSyncInboxPageKind::V1, 1, 0);
        wrong_device.device_id = "123e4567-e89b-42d3-a456-426614174111".into();
        assert!(list_received_note_sync_inbox_page(&connection, &wrong_device).is_err());
        let mut wrong_account = received_page_command(ReceivedNoteSyncInboxPageKind::V1, 1, 0);
        wrong_account.account_id = "other-account".into();
        assert!(list_received_note_sync_inbox_page(&connection, &wrong_account).is_err());
    }

    #[test]
    fn received_inbox_resolution_pages_do_not_hide_v1_and_reject_bad_objects() {
        let mut connection = database();
        inbound_scope(&mut connection);
        for sequence in 1..=32 {
            insert_received_item(&mut connection, sequence, "resolution");
        }
        insert_received_item(&mut connection, 33, "delete");
        let resolutions = list_received_note_sync_inbox_page(
            &connection,
            &received_page_command(ReceivedNoteSyncInboxPageKind::Resolution, 32, 0),
        )
        .unwrap();
        assert_eq!(resolutions.len(), 32);
        assert!(resolutions
            .iter()
            .all(|item| item.operation == "resolution"));
        let v1 = list_received_note_sync_inbox_page(
            &connection,
            &received_page_command(ReceivedNoteSyncInboxPageKind::V1, 32, 0),
        )
        .unwrap();
        assert_eq!(v1.len(), 1);
        assert_eq!(v1[0].server_sequence, 33);
        assert_eq!(v1[0].operation, "delete");
        connection.execute("UPDATE cloud_sync_inbox SET state='orphan' WHERE account_id='inbox-account' AND server_sequence=1", []).unwrap();
        assert_eq!(
            list_received_note_sync_inbox_page(
                &connection,
                &received_page_command(ReceivedNoteSyncInboxPageKind::Resolution, 1, 0)
            )
            .unwrap()[0]
                .server_sequence,
            2
        );

        let mut missing = database();
        inbound_scope(&mut missing);
        insert_received_item(&mut missing, 1, "resolution");
        missing
            .execute_batch("DROP TRIGGER cloud_sync_resolution_inbox_object_immutable_delete;")
            .unwrap();
        missing
            .execute(
                "DELETE FROM cloud_sync_event_objects WHERE account_id='inbox-account'",
                [],
            )
            .unwrap();
        assert!(matches!(
            list_received_note_sync_inbox_page(
                &missing,
                &received_page_command(ReceivedNoteSyncInboxPageKind::Resolution, 1, 0)
            ),
            Err(NoteSyncError::SealedObjectMissing)
        ));

        let mut malformed = database();
        inbound_scope(&mut malformed);
        insert_received_item(&mut malformed, 1, "resolution");
        malformed
            .execute_batch("DROP TRIGGER cloud_sync_resolution_inbox_object_immutable_update;")
            .unwrap();
        malformed.execute("UPDATE cloud_sync_event_objects SET crypto_version=2 WHERE account_id='inbox-account'", []).unwrap();
        assert!(matches!(
            list_received_note_sync_inbox_page(
                &malformed,
                &received_page_command(ReceivedNoteSyncInboxPageKind::Resolution, 1, 0)
            ),
            Err(NoteSyncError::InvalidEnvelope(_))
        ));
    }

    #[test]
    fn orphan_resolution_reader_is_scoped_keyset_bounded_and_read_only() {
        let mut connection = database();
        inbound_scope(&mut connection);
        for sequence in 1..=3 {
            insert_orphan_resolution(&mut connection, sequence);
        }
        connection.execute("UPDATE cloud_sync_inbox SET state='received' WHERE account_id='inbox-account' AND server_sequence=1", []).unwrap();
        connection.execute("UPDATE cloud_sync_inbox SET state='conflict' WHERE account_id='inbox-account' AND server_sequence=2", []).unwrap();
        connection.execute("INSERT INTO cloud_sync_state(account_id,device_id,pull_cursor,ack_cursor,created_at,updated_at) VALUES('other-account',?1,0,0,'now','now')", [DEVICE_ID]).unwrap();
        connection.execute("INSERT INTO cloud_account_bindings(local_account_id,canonical_user_id,created_at,validated_at) VALUES('other-account','abcdefab-0000-0000-0000-000000000102','now','now')", []).unwrap();
        let mut foreign = inbound_command();
        foreign.account_id = "other-account".into();
        foreign.canonical_user_id = "abcdefab-0000-0000-0000-000000000102".into();
        foreign.items[0].event_id = "123e4567-e89b-42d3-a456-426614174097".into();
        foreign.items[0].operation = "resolution".into();
        foreign.items[0].revision = 2;
        commit_note_sync_inbound_page(&mut connection, &foreign).unwrap();
        connection
            .execute(
                "UPDATE cloud_sync_inbox SET state='orphan' WHERE account_id='other-account'",
                [],
            )
            .unwrap();
        let before: (i64, i64, i64) = connection.query_row(
            "SELECT pull_cursor,ack_cursor,(SELECT count(*) FROM cloud_sync_inbox WHERE account_id='inbox-account' AND state='orphan') FROM cloud_sync_state WHERE account_id='inbox-account'",
            [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).unwrap();
        let listed =
            list_orphan_note_resolution_inbox(&connection, &orphan_resolution_list_command(1, 0))
                .unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].server_sequence, 3);
        assert_eq!(listed[0].operation, "resolution");
        assert!(list_orphan_note_resolution_inbox(
            &connection,
            &orphan_resolution_list_command(1, 3)
        )
        .unwrap()
        .is_empty());
        let after: (i64, i64, i64) = connection.query_row(
            "SELECT pull_cursor,ack_cursor,(SELECT count(*) FROM cloud_sync_inbox WHERE account_id='inbox-account' AND state='orphan') FROM cloud_sync_state WHERE account_id='inbox-account'",
            [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).unwrap();
        assert_eq!(after, before);
        assert!(list_orphan_note_resolution_inbox(
            &connection,
            &orphan_resolution_list_command(33, 0)
        )
        .is_err());
        assert!(list_orphan_note_resolution_inbox(
            &connection,
            &orphan_resolution_list_command(1, -1)
        )
        .is_err());
        let mut wrong_device = orphan_resolution_list_command(1, 0);
        wrong_device.device_id = "123e4567-e89b-42d3-a456-426614174111".into();
        assert!(list_orphan_note_resolution_inbox(&connection, &wrong_device).is_err());
        let mut wrong_user = orphan_resolution_list_command(1, 0);
        wrong_user.canonical_user_id = "abcdefab-0000-0000-0000-000000000102".into();
        assert!(list_orphan_note_resolution_inbox(&connection, &wrong_user).is_err());
        let mut applied = database();
        inbound_scope(&mut applied);
        seed_applied_resolution_proof(&applied);
        assert!(list_orphan_note_resolution_inbox(
            &applied,
            &orphan_resolution_list_command(32, 0)
        )
        .unwrap()
        .is_empty());
    }

    #[test]
    fn orphan_resolution_reader_excludes_v1_orphans_and_validates_durable_objects() {
        let mut connection = database();
        inbound_scope(&mut connection);
        insert_orphan_resolution(&mut connection, 1);
        let mut v1 = inbound_command();
        v1.expected_cursor = 1;
        v1.next_cursor = 2;
        v1.items[0].event_id = "123e4567-e89b-42d3-a456-426614174002".into();
        v1.items[0].server_sequence = 2;
        commit_note_sync_inbound_page(&mut connection, &v1).unwrap();
        connection.execute("UPDATE cloud_sync_inbox SET state='orphan' WHERE account_id='inbox-account' AND server_sequence=2", []).unwrap();
        assert_eq!(
            list_orphan_note_resolution_inbox(&connection, &orphan_resolution_list_command(32, 0))
                .unwrap()
                .len(),
            1
        );
        connection
            .execute_batch("DROP TRIGGER cloud_sync_resolution_inbox_object_immutable_delete;")
            .unwrap();
        connection.execute("DELETE FROM cloud_sync_event_objects WHERE account_id='inbox-account' AND event_id=(SELECT event_id FROM cloud_sync_inbox WHERE account_id='inbox-account' AND server_sequence=1)", []).unwrap();
        assert!(matches!(
            list_orphan_note_resolution_inbox(&connection, &orphan_resolution_list_command(32, 0)),
            Err(NoteSyncError::SealedObjectMissing)
        ));
        let mut malformed = database();
        inbound_scope(&mut malformed);
        insert_orphan_resolution(&mut malformed, 1);
        malformed
            .execute_batch("DROP TRIGGER cloud_sync_resolution_inbox_object_immutable_update;")
            .unwrap();
        malformed.execute("UPDATE cloud_sync_event_objects SET crypto_version=2 WHERE account_id='inbox-account'", []).unwrap();
        assert!(matches!(
            list_orphan_note_resolution_inbox(&malformed, &orphan_resolution_list_command(32, 0)),
            Err(NoteSyncError::InvalidEnvelope(_))
        ));
    }

    #[test]
    fn inbound_conflict_rolls_back_page_and_cursor() {
        let mut connection = database();
        inbound_scope(&mut connection);
        let command = inbound_command();
        commit_note_sync_inbound_page(&mut connection, &command).unwrap();
        let mut conflict = inbound_command();
        conflict.items[0].envelope.as_mut().unwrap().ciphertext = "AQAAAAAAAAAAAAAAAAAAAA".into();
        assert!(commit_note_sync_inbound_page(&mut connection, &conflict).is_err());
        assert_eq!(
            connection
                .query_row(
                    "SELECT pull_cursor FROM cloud_sync_state WHERE account_id='inbox-account'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
    }

    #[test]
    fn inbound_replay_survives_reopen_and_stale_or_partial_pages_rollback() {
        let (root, path) = temporary_database_path("inbound-replay");
        let mut connection = crate::sqlite::open_database(&path).unwrap();
        configure_database(&connection);
        inbound_scope(&mut connection);
        let command = inbound_command();
        commit_note_sync_inbound_page(&mut connection, &command).unwrap();
        drop(connection);
        let mut reopened = crate::sqlite::open_database(&path).unwrap();
        assert_eq!(
            commit_note_sync_inbound_page(&mut reopened, &command)
                .unwrap()
                .replayed_events,
            1
        );
        let mut stale = inbound_command();
        stale.expected_cursor = 2;
        stale.next_cursor = 3;
        assert!(commit_note_sync_inbound_page(&mut reopened, &stale).is_err());
        let mut partial = inbound_command();
        partial.items.clear();
        assert!(commit_note_sync_inbound_page(&mut reopened, &partial).is_err());
        assert_eq!(reopened.query_row("SELECT pull_cursor,ack_cursor FROM cloud_sync_state WHERE account_id='inbox-account'", [], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))).unwrap(), (1, 0));
        assert_eq!(
            reopened
                .query_row("SELECT count(*) FROM notes", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            reopened
                .query_row("SELECT count(*) FROM cloud_sync_outbox", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        drop(reopened);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn durable_ack_candidate_requires_a_contiguous_applied_prefix() {
        let mut connection = database();
        inbound_scope(&mut connection);
        assert_eq!(
            prepare_note_sync_ack(&mut connection, &ack_prepare_command())
                .unwrap()
                .candidate_cursor,
            0
        );
        set_ack_pull_cursor(&connection, 3);
        for sequence in 1..=3 {
            insert_ack_inbox(&connection, sequence, "applied");
        }
        assert_eq!(
            prepare_note_sync_ack(&mut connection, &ack_prepare_command()).unwrap(),
            NoteSyncAckCandidate {
                current_ack_cursor: 0,
                candidate_cursor: 3
            }
        );
        let mut gap = database();
        inbound_scope(&mut gap);
        set_ack_pull_cursor(&gap, 3);
        insert_ack_inbox(&gap, 1, "applied");
        insert_ack_inbox(&gap, 3, "applied");
        assert_eq!(
            prepare_note_sync_ack(&mut gap, &ack_prepare_command())
                .unwrap()
                .candidate_cursor,
            1
        );
    }

    #[test]
    fn durable_ack_candidate_stops_at_unresolved_states_and_pull_bound() {
        for state in ["conflict", "orphan", "rejected", "unknown_entity"] {
            let mut connection = database();
            inbound_scope(&mut connection);
            set_ack_pull_cursor(&connection, 3);
            insert_ack_inbox(&connection, 1, "applied");
            insert_ack_inbox(&connection, 2, state);
            insert_ack_inbox(&connection, 3, "applied");
            assert_eq!(
                prepare_note_sync_ack(&mut connection, &ack_prepare_command())
                    .unwrap()
                    .candidate_cursor,
                1,
                "{state}"
            );
        }
        let mut bounded = database();
        inbound_scope(&mut bounded);
        set_ack_pull_cursor(&bounded, 1);
        insert_ack_inbox(&bounded, 1, "applied");
        insert_ack_inbox(&bounded, 2, "applied");
        assert_eq!(
            prepare_note_sync_ack(&mut bounded, &ack_prepare_command())
                .unwrap()
                .candidate_cursor,
            1
        );
    }

    #[test]
    fn durable_ack_advance_is_conditional_idempotent_and_replay_safe() {
        let mut connection = database();
        inbound_scope(&mut connection);
        set_ack_pull_cursor(&connection, 2);
        insert_ack_inbox(&connection, 1, "applied");
        insert_ack_inbox(&connection, 2, "applied");
        assert_eq!(
            commit_note_sync_ack(&mut connection, &ack_commit_command(0, 2)).unwrap(),
            CommitNoteSyncAckResult::Advanced
        );
        assert_eq!(
            commit_note_sync_ack(&mut connection, &ack_commit_command(0, 2)).unwrap(),
            CommitNoteSyncAckResult::AlreadyAcknowledged
        );
        assert_eq!(
            commit_note_sync_ack(&mut connection, &ack_commit_command(0, 1)).unwrap(),
            CommitNoteSyncAckResult::AlreadyAdvanced
        );
        let mut stale = database();
        inbound_scope(&mut stale);
        set_ack_pull_cursor(&stale, 2);
        insert_ack_inbox(&stale, 1, "applied");
        insert_ack_inbox(&stale, 2, "applied");
        stale
            .execute(
                "UPDATE cloud_sync_state SET ack_cursor=1 WHERE account_id='inbox-account'",
                [],
            )
            .unwrap();
        assert_eq!(
            commit_note_sync_ack(&mut stale, &ack_commit_command(0, 2)).unwrap(),
            CommitNoteSyncAckResult::Stale
        );
    }

    #[test]
    fn durable_ack_advance_revalidates_scope_prefix_restart_and_rollback() {
        let mut connection = database();
        inbound_scope(&mut connection);
        set_ack_pull_cursor(&connection, 2);
        insert_ack_inbox(&connection, 1, "applied");
        insert_ack_inbox(&connection, 2, "conflict");
        assert!(commit_note_sync_ack(&mut connection, &ack_commit_command(0, 2)).is_err());
        let mut wrong = ack_prepare_command();
        wrong.device_id = "123e4567-e89b-42d3-a456-426614174111".into();
        assert!(prepare_note_sync_ack(&mut connection, &wrong).is_err());
        wrong = ack_prepare_command();
        wrong.account_id = "other-account".into();
        assert!(prepare_note_sync_ack(&mut connection, &wrong).is_err());
        connection.execute("UPDATE cloud_sync_inbox SET state='applied' WHERE account_id='inbox-account' AND server_sequence=2", []).unwrap();
        connection.execute_batch("CREATE TRIGGER note_sync_ack_test_fail BEFORE UPDATE OF ack_cursor ON cloud_sync_state WHEN NEW.account_id='inbox-account' BEGIN SELECT RAISE(ABORT,'injected_ack_failure'); END;").unwrap();
        assert!(commit_note_sync_ack(&mut connection, &ack_commit_command(0, 2)).is_err());
        assert_eq!(
            connection
                .query_row(
                    "SELECT ack_cursor FROM cloud_sync_state WHERE account_id='inbox-account'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        connection
            .execute_batch("DROP TRIGGER note_sync_ack_test_fail;")
            .unwrap();
        assert_eq!(
            commit_note_sync_ack(&mut connection, &ack_commit_command(0, 2)).unwrap(),
            CommitNoteSyncAckResult::Advanced
        );
        let (root, path) = temporary_database_path("ack-restart");
        let mut persistent = crate::sqlite::open_database(&path).unwrap();
        configure_database(&persistent);
        inbound_scope(&mut persistent);
        set_ack_pull_cursor(&persistent, 1);
        insert_ack_inbox(&persistent, 1, "applied");
        assert_eq!(
            commit_note_sync_ack(&mut persistent, &ack_commit_command(0, 1)).unwrap(),
            CommitNoteSyncAckResult::Advanced
        );
        drop(persistent);
        let mut reopened = crate::sqlite::open_database(&path).unwrap();
        assert_eq!(
            prepare_note_sync_ack(&mut reopened, &ack_prepare_command()).unwrap(),
            NoteSyncAckCandidate {
                current_ack_cursor: 1,
                candidate_cursor: 1
            }
        );
        drop(reopened);
        std::fs::remove_dir_all(root).unwrap();
    }

    fn configure_database(connection: &Connection) {
        connection.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        connection.execute("INSERT INTO projects(id,name,infinite,unit,status,payload_json) VALUES('p','P',0,'symbols','active','{}')", []).unwrap();
        connection
            .execute(
                "INSERT INTO project_order(project_id,position) VALUES('p',0)",
                [],
            )
            .unwrap();
        connection.execute("INSERT INTO cloud_sync_state(account_id,device_id,pull_cursor,ack_cursor,created_at,updated_at) VALUES('account',?1,0,0,'now','now')", [DEVICE_ID]).unwrap();
        connection.execute("INSERT INTO cloud_sync_project_bindings(project_id,account_id,created_at,updated_at) VALUES('p','account','now','now')", []).unwrap();
    }

    fn note(content: &str, revision: i64) -> String {
        note_for("n", content, revision)
    }

    fn note_for(note_id: &str, content: &str, revision: i64) -> String {
        serde_json::json!({
            "id":note_id, "project_id":"p", "stage_id":null, "title":"",
            "content":content, "content_format":"html", "checklist":[],
            "color":"default", "pinned":false, "archived":false,
            "sort_order":0, "tags":[], "source_type":"project",
            "source_map_id":null, "source_node_id":null,
            "created_at":"2026-09-22T00:00:00Z",
            "updated_at":"2026-09-22T00:00:00Z", "revision":revision,
            "metadata":{}
        })
        .to_string()
    }

    fn prepare<'a>(snapshot: &'a str) -> PrepareNoteIntent<'a> {
        prepare_for("n", snapshot)
    }

    fn prepare_for<'a>(note_id: &'a str, snapshot: &'a str) -> PrepareNoteIntent<'a> {
        PrepareNoteIntent {
            project_id: "p",
            entity_id: note_id,
            operation: NoteSyncOperation::Upsert,
            updated_at: "2026-09-22T00:00:00Z",
            deleted_at: None,
            snapshot_json: snapshot,
            state_updated_at: "2026-09-22T00:00:00Z",
        }
    }

    fn persist_note_intent(
        connection: &mut Connection,
        note_id: &str,
        content: &str,
    ) -> (PreparedNoteIntent, String) {
        let snapshot = note_for(note_id, content, 0);
        let transaction = connection.transaction().unwrap();
        let intent = prepare_unsealed_note_intent(&transaction, prepare_for(note_id, &snapshot))
            .unwrap()
            .unwrap();
        let exists = transaction
            .query_row("SELECT 1 FROM notes WHERE id=?1", [note_id], |_| Ok(()))
            .optional()
            .unwrap()
            .is_some();
        if exists {
            transaction
                .execute(
                    "UPDATE notes SET updated_at='2026-09-22T00:00:00Z',payload_json=?1
                     WHERE id=?2",
                    rusqlite::params![snapshot, note_id],
                )
                .unwrap();
        } else {
            transaction
                .execute(
                    "INSERT INTO notes(id,project_id,stage_id,updated_at,payload_json)
                     VALUES(?1,'p',NULL,'2026-09-22T00:00:00Z',?2)",
                    rusqlite::params![note_id, snapshot],
                )
                .unwrap();
        }
        transaction.commit().unwrap();
        (intent, snapshot)
    }

    fn encode_base64url(bytes: &[u8]) -> String {
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut encoded = String::with_capacity(base64url_encoded_length(bytes.len()).unwrap());
        let complete_length = bytes.len() / 3 * 3;
        for chunk in bytes[..complete_length].chunks_exact(3) {
            encoded.push(ALPHABET[(chunk[0] >> 2) as usize] as char);
            encoded.push(ALPHABET[(((chunk[0] & 0x03) << 4) | (chunk[1] >> 4)) as usize] as char);
            encoded.push(ALPHABET[(((chunk[1] & 0x0f) << 2) | (chunk[2] >> 6)) as usize] as char);
            encoded.push(ALPHABET[(chunk[2] & 0x3f) as usize] as char);
        }
        match bytes.len() - complete_length {
            1 => {
                encoded.push(ALPHABET[(bytes[complete_length] >> 2) as usize] as char);
                encoded.push(ALPHABET[((bytes[complete_length] & 0x03) << 4) as usize] as char);
            }
            2 => {
                encoded.push(ALPHABET[(bytes[complete_length] >> 2) as usize] as char);
                encoded.push(
                    ALPHABET[(((bytes[complete_length] & 0x03) << 4)
                        | (bytes[complete_length + 1] >> 4)) as usize] as char,
                );
                encoded.push(ALPHABET[((bytes[complete_length + 1] & 0x0f) << 2) as usize] as char);
            }
            _ => {}
        }
        encoded
    }

    fn envelope(fill: u8) -> EncryptedNoteSyncEnvelope {
        EncryptedNoteSyncEnvelope {
            crypto_version: 1,
            aad_version: 1,
            nonce: encode_base64url(&vec![fill; 24]),
            ciphertext: encode_base64url(&vec![fill.wrapping_add(1); 16]),
        }
    }

    fn seal_command(
        event_id: &str,
        mutation_generation: i64,
        envelope: EncryptedNoteSyncEnvelope,
    ) -> CommitSealedNoteSyncEventCommand {
        CommitSealedNoteSyncEventCommand {
            event_id: event_id.to_string(),
            expected_mutation_generation: mutation_generation,
            envelope,
        }
    }

    fn failure_command(
        event_id: &str,
        mutation_generation: i64,
        error_code: NoteSyncSealErrorCode,
    ) -> RecordNoteSyncSealFailureCommand {
        RecordNoteSyncSealFailureCommand {
            event_id: event_id.to_string(),
            expected_mutation_generation: mutation_generation,
            error_code,
        }
    }

    fn seal_persisted_intent(
        connection: &mut Connection,
        note_id: &str,
        content: &str,
        fill: u8,
    ) -> PreparedNoteIntent {
        let (intent, _) = persist_note_intent(connection, note_id, content);
        assert_eq!(
            commit_sealed_note_sync_event(
                connection,
                &seal_command(&intent.event_id, intent.mutation_generation, envelope(fill)),
            )
            .unwrap(),
            CommitSealedNoteSyncEventResult::Sealed
        );
        intent
    }

    fn temporary_database_path(label: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "nfprogress-note-sync-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("nfprogress.db");
        (root, path)
    }

    #[test]
    fn note_sync_unbound_project_needs_no_intent() {
        let mut connection = database();
        connection
            .execute("DELETE FROM cloud_sync_project_bindings", [])
            .unwrap();
        let snapshot = note("local", 0);
        let transaction = connection.transaction().unwrap();
        assert!(
            prepare_unsealed_note_intent(&transaction, prepare(&snapshot))
                .unwrap()
                .is_none()
        );
        transaction.commit().unwrap();
    }

    #[test]
    fn note_sync_coalesces_unsealed_identity_and_advances_generation() {
        let mut connection = database();
        let first_snapshot = note("first", 0);
        let transaction = connection.transaction().unwrap();
        let first = prepare_unsealed_note_intent(&transaction, prepare(&first_snapshot))
            .unwrap()
            .unwrap();
        transaction.execute("INSERT INTO notes(id,project_id,stage_id,updated_at,payload_json) VALUES('n','p',NULL,'2026-09-22T00:00:00Z',?1)", [&first_snapshot]).unwrap();
        transaction.commit().unwrap();

        let second_snapshot = note("second", 1);
        let transaction = connection.transaction().unwrap();
        let second = prepare_unsealed_note_intent(&transaction, prepare(&second_snapshot))
            .unwrap()
            .unwrap();
        transaction
            .execute(
                "UPDATE notes SET updated_at='2026-09-22T00:00:00Z',payload_json=?1 \
                 WHERE id='n'",
                [&second_snapshot],
            )
            .unwrap();
        transaction.commit().unwrap();

        assert_eq!(second.event_id, first.event_id);
        assert_eq!(second.revision, 1);
        assert_eq!(second.parent_event_id, None);
        assert_eq!(second.local_ordinal, first.local_ordinal);
        assert_eq!(second.mutation_generation, 2);
        assert!(second.coalesced);
        assert_eq!(second.event_id.len(), 36);
        assert_eq!(&second.event_id[14..15], "4");
        assert!(matches!(&second.event_id[19..20], "8" | "9" | "a" | "b"));
        assert_eq!(
            connection
                .query_row(
                    "SELECT snapshot_json FROM cloud_sync_note_intents",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            second_snapshot
        );
    }

    #[test]
    fn note_sync_generation_reaches_wire_max_and_refuses_the_next_increment() {
        let mut connection = database();
        let (first, _) = persist_note_intent(&mut connection, "n", "first");
        connection
            .execute(
                "UPDATE cloud_sync_note_intents SET mutation_generation=?1 WHERE event_id=?2",
                rusqlite::params![MAX_SYNC_INTEGER - 1, first.event_id],
            )
            .unwrap();

        let maximum_snapshot = note("maximum", 1);
        let transaction = connection.transaction().unwrap();
        let maximum = prepare_unsealed_note_intent(&transaction, prepare(&maximum_snapshot))
            .unwrap()
            .unwrap();
        transaction
            .execute(
                "UPDATE notes SET payload_json=?1 WHERE id='n'",
                [&maximum_snapshot],
            )
            .unwrap();
        transaction.commit().unwrap();

        assert_eq!(maximum.event_id, first.event_id);
        assert_eq!(maximum.mutation_generation, MAX_SYNC_INTEGER);
        let listed = list_unsealed_note_sync_intents(&mut connection, 1, false).unwrap();
        assert_eq!(listed[0].mutation_generation, MAX_SYNC_INTEGER);
        assert_eq!(
            serde_json::to_value(&listed[0]).unwrap()["mutation_generation"].as_u64(),
            Some(MAX_SYNC_INTEGER as u64)
        );

        let overflow_snapshot = note("overflow", 2);
        let transaction = connection.transaction().unwrap();
        assert!(matches!(
            prepare_unsealed_note_intent(&transaction, prepare(&overflow_snapshot)),
            Err(NoteSyncError::MutationGenerationOverflow)
        ));
        drop(transaction);
        let stored: (i64, String) = connection
            .query_row(
                "SELECT mutation_generation,snapshot_json FROM cloud_sync_note_intents",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(stored, (MAX_SYNC_INTEGER, maximum_snapshot));
    }

    #[test]
    fn note_sync_generation_cas_and_listing_enforce_the_wire_limit() {
        let mut connection = database();
        let (failure_intent, _) = persist_note_intent(&mut connection, "failure", "failure");
        connection
            .execute(
                "UPDATE cloud_sync_note_intents SET mutation_generation=?1 WHERE event_id=?2",
                rusqlite::params![MAX_SYNC_INTEGER, failure_intent.event_id],
            )
            .unwrap();
        assert_eq!(
            record_note_sync_seal_failure(
                &mut connection,
                &failure_command(
                    &failure_intent.event_id,
                    MAX_SYNC_INTEGER,
                    NoteSyncSealErrorCode::KeyUnavailable,
                ),
            )
            .unwrap(),
            RecordNoteSyncSealFailureResult::Recorded
        );
        assert!(matches!(
            record_note_sync_seal_failure(
                &mut connection,
                &failure_command(
                    &failure_intent.event_id,
                    MAX_SYNC_INTEGER + 1,
                    NoteSyncSealErrorCode::KeyUnavailable,
                ),
            ),
            Err(NoteSyncError::InvalidSealState(_))
        ));

        let (seal_intent, _) = persist_note_intent(&mut connection, "seal", "seal");
        connection
            .execute(
                "UPDATE cloud_sync_note_intents SET mutation_generation=?1 WHERE event_id=?2",
                rusqlite::params![MAX_SYNC_INTEGER, seal_intent.event_id],
            )
            .unwrap();
        assert_eq!(
            commit_sealed_note_sync_event(
                &mut connection,
                &seal_command(&seal_intent.event_id, MAX_SYNC_INTEGER, envelope(31)),
            )
            .unwrap(),
            CommitSealedNoteSyncEventResult::Sealed
        );

        connection
            .execute(
                "UPDATE cloud_sync_note_intents SET mutation_generation=?1 WHERE event_id=?2",
                rusqlite::params![MAX_SYNC_INTEGER + 1, failure_intent.event_id],
            )
            .unwrap();
        assert!(matches!(
            list_unsealed_note_sync_intents(&mut connection, 10, true),
            Err(NoteSyncError::InvalidSealState(_))
        ));
        assert!(matches!(
            commit_sealed_note_sync_event(
                &mut connection,
                &seal_command(&failure_intent.event_id, MAX_SYNC_INTEGER + 1, envelope(32)),
            ),
            Err(NoteSyncError::InvalidSealState(_))
        ));
    }

    #[test]
    fn note_sync_legacy_timestamp_preflight_is_project_scoped_and_read_only() {
        let connection = database();
        connection
            .execute("DELETE FROM cloud_sync_project_bindings", [])
            .unwrap();
        connection.execute("INSERT INTO projects(id,name,infinite,unit,status,payload_json) VALUES('other','Other',0,'symbols','active','{}')", []).unwrap();
        connection
            .execute(
                "INSERT INTO project_order(project_id,position) VALUES('other',1)",
                [],
            )
            .unwrap();

        let mut valid: serde_json::Value = serde_json::from_str(&note_for("valid", "", 0)).unwrap();
        valid["created_at"] = serde_json::json!("2024-02-29T23:59:59.123456+03:30");
        valid["updated_at"] = serde_json::json!("2026-09-22T00:00:00Z");
        let mut missing: serde_json::Value =
            serde_json::from_str(&note_for("missing", "", 0)).unwrap();
        missing["created_at"] = serde_json::json!("");
        let mut invalid: serde_json::Value =
            serde_json::from_str(&note_for("invalid", "", 0)).unwrap();
        invalid["created_at"] = serde_json::json!("2025-02-29T00:00:00Z");
        invalid["updated_at"] = serde_json::json!("not-a-timestamp");
        let other = note_for("other-note", "", 0);

        for (note_id, project_id, payload) in [
            ("valid", "p", valid.to_string()),
            ("missing", "p", missing.to_string()),
            ("invalid", "p", invalid.to_string()),
            ("other-note", "other", other),
        ] {
            connection
                .execute(
                    "INSERT INTO notes(id,project_id,stage_id,updated_at,payload_json)
                     VALUES(?1,?2,NULL,'',?3)",
                    rusqlite::params![note_id, project_id, payload],
                )
                .unwrap();
        }

        assert_eq!(
            preflight_note_sync_project(&connection, "p").unwrap(),
            vec![
                NoteSyncPreflightIssue {
                    note_id: "invalid".to_string(),
                    code: NoteSyncPreflightIssueCode::InvalidCreatedAt,
                },
                NoteSyncPreflightIssue {
                    note_id: "invalid".to_string(),
                    code: NoteSyncPreflightIssueCode::InvalidUpdatedAt,
                },
                NoteSyncPreflightIssue {
                    note_id: "missing".to_string(),
                    code: NoteSyncPreflightIssueCode::MissingCreatedAt,
                },
            ]
        );
        assert!(preflight_note_sync_project(&connection, "other")
            .unwrap()
            .is_empty());
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM cloud_sync_outbox", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM notes", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            4
        );
    }

    #[test]
    fn note_sync_timestamp_preflight_matches_frozen_timestamp_boundaries() {
        for valid in [
            "0001-01-01T00:00:00Z",
            "2024-02-29T23:59:59.1Z",
            "9999-12-31T23:59:59.999999Z",
            "2026-09-22T00:00:00+23:59",
        ] {
            assert!(valid_note_sync_timestamp(valid), "expected valid: {valid}");
        }
        for invalid in [
            "",
            "20x6-09-22T00:00:00Z",
            "0000-01-01T00:00:00Z",
            "2025-02-29T00:00:00Z",
            "2026-09-22T00:00:00.1234567Z",
            "2026-09-22T00:00:00-00:00",
            "0001-01-01T00:00:00+00:01",
            "9999-12-31T23:59:59-00:01",
        ] {
            assert!(
                !valid_note_sync_timestamp(invalid),
                "expected invalid: {invalid}"
            );
        }
    }

    #[test]
    fn note_sync_ipc_fixture_matches_real_rust_serde_contract() {
        let expected: serde_json::Value = serde_json::from_str(include_str!(
            "../../src/infrastructure/sqlite/noteSyncIpcContract.v1.json"
        ))
        .unwrap();
        let event_id = "123e4567-e89b-42d3-a456-426614174000";
        let actual = serde_json::json!({
            "list_unsealed_note_sync_intents": [
                UnsealedNoteSyncIntent {
                    event_id: event_id.to_string(),
                    account_id: "account-1".to_string(),
                    device_id: "123e4567-e89b-42d3-a456-426614174001".to_string(),
                    project_id: "project-1".to_string(),
                    entity_id: "note-1".to_string(),
                    entity_type: "note".to_string(),
                    operation: NoteSyncOperation::Upsert,
                    revision: 1,
                    parent_event_id: None,
                    updated_at: "2026-09-22T00:00:00.000000Z".to_string(),
                    deleted_at: None,
                    local_ordinal: 1,
                    mutation_generation: MAX_SYNC_INTEGER,
                    snapshot_json: "{\"id\":\"note-1\"}".to_string(),
                    seal_state: NoteSyncSealState::Pending,
                    seal_attempt_count: 0,
                    last_error_code: None,
                    next_attempt_at: None,
                },
                UnsealedNoteSyncIntent {
                    event_id: "123e4567-e89b-42d3-a456-426614174002".to_string(),
                    account_id: "account-1".to_string(),
                    device_id: "123e4567-e89b-42d3-a456-426614174001".to_string(),
                    project_id: "project-1".to_string(),
                    entity_id: "note-2".to_string(),
                    entity_type: "note".to_string(),
                    operation: NoteSyncOperation::Delete,
                    revision: 2,
                    parent_event_id: Some(
                        "123e4567-e89b-42d3-a456-426614174003".to_string()
                    ),
                    updated_at: "2026-09-22T00:00:01.000000Z".to_string(),
                    deleted_at: Some("2026-09-22T00:00:01.000000Z".to_string()),
                    local_ordinal: 2,
                    mutation_generation: 7,
                    snapshot_json: concat!(
                        "{\"id\":\"note-2\",",
                        "\"deleted_at\":\"2026-09-22T00:00:01.000000Z\"}"
                    )
                    .to_string(),
                    seal_state: NoteSyncSealState::RetryableError,
                    seal_attempt_count: 2,
                    last_error_code: Some("runtime_unavailable".to_string()),
                    next_attempt_at: Some("2026-09-22T00:00:31.000000Z".to_string()),
                },
            ],
            "record_note_sync_seal_failure": {
                "command": RecordNoteSyncSealFailureCommand {
                    event_id: event_id.to_string(),
                    expected_mutation_generation: MAX_SYNC_INTEGER,
                    error_code: NoteSyncSealErrorCode::RuntimeUnavailable,
                },
            },
            "commit_sealed_note_sync_event": {
                "command": CommitSealedNoteSyncEventCommand {
                    event_id: event_id.to_string(),
                    expected_mutation_generation: MAX_SYNC_INTEGER,
                    envelope: EncryptedNoteSyncEnvelope {
                        crypto_version: SUPPORTED_CRYPTO_VERSION,
                        aad_version: SUPPORTED_AAD_VERSION,
                        nonce: encode_base64url(&[0; 24]),
                        ciphertext: encode_base64url(&[0; 16]),
                    },
                },
            },
            "commit_note_sync_upload_acceptance": {
                "command": CommitNoteSyncUploadAcceptanceCommand {
                    account_id: "account-1".to_string(),
                    device_id: "123e4567-e89b-42d3-a456-426614174001".to_string(),
                    receipts: vec![NoteSyncUploadReceipt {
                        event_id: "123e4567-e89b-42d3-a456-426614174004".to_string(),
                        server_sequence: 9,
                        duplicate: true,
                    }],
                },
            },
            "record_note_sync_upload_failure": {
                "account_id": "account-1",
                "device_id": "123e4567-e89b-42d3-a456-426614174001",
                "event_ids": ["123e4567-e89b-42d3-a456-426614174004"],
                "error_code": NoteSyncUploadErrorCode::RequestTimeout,
            },
            "list_sealed_note_sync_outbox": [
                SealedNoteSyncOutboxItem {
                    event_id: "123e4567-e89b-42d3-a456-426614174004".to_string(),
                    account_id: "account-1".to_string(),
                    device_id: "123e4567-e89b-42d3-a456-426614174001".to_string(),
                    project_id: "deleted-project".to_string(),
                    entity_id: "note-3".to_string(),
                    entity_type: "note".to_string(),
                    operation: NoteSyncOperation::Delete,
                    revision: 2,
                    parent_event_id: Some("123e4567-e89b-42d3-a456-426614174003".to_string()),
                    updated_at: "2026-09-22T00:00:02.000000Z".to_string(),
                    deleted_at: Some("2026-09-22T00:00:02.000000Z".to_string()),
                    local_ordinal: 3,
                    envelope: EncryptedNoteSyncEnvelope {
                        crypto_version: SUPPORTED_CRYPTO_VERSION,
                        aad_version: SUPPORTED_AAD_VERSION,
                        nonce: encode_base64url(&[0; 24]),
                        ciphertext: encode_base64url(&[0; 16]),
                    },
                }
            ],
            "operation_values": [NoteSyncOperation::Upsert, NoteSyncOperation::Delete],
            "seal_state_values": [
                NoteSyncSealState::Pending,
                NoteSyncSealState::RetryableError,
                NoteSyncSealState::Blocked,
                NoteSyncSealState::InvariantError,
            ],
            "error_code_values": [
                NoteSyncSealErrorCode::KeyUnavailable,
                NoteSyncSealErrorCode::PayloadTooLarge,
                NoteSyncSealErrorCode::EncryptedSyncObjectTooLarge,
                NoteSyncSealErrorCode::DependencyNotSynced,
                NoteSyncSealErrorCode::UnsupportedContentFormat,
                NoteSyncSealErrorCode::InvalidNotePayload,
                NoteSyncSealErrorCode::CryptoContextInvalid,
                NoteSyncSealErrorCode::InvalidSyncMetadata,
                NoteSyncSealErrorCode::InvalidEnvelope,
                NoteSyncSealErrorCode::MetadataMismatch,
                NoteSyncSealErrorCode::RuntimeUnavailable,
            ],
            "record_failure_result_values": [
                RecordNoteSyncSealFailureResult::Recorded,
                RecordNoteSyncSealFailureResult::StaleGeneration,
                RecordNoteSyncSealFailureResult::AlreadySealed,
            ],
            "commit_result_values": [
                CommitSealedNoteSyncEventResult::Sealed,
                CommitSealedNoteSyncEventResult::StaleGeneration,
                CommitSealedNoteSyncEventResult::AlreadySealed,
            ],
        });

        assert_eq!(actual, expected);
    }

    #[test]
    fn sealed_outbox_is_bounded_deterministic_and_read_only() {
        let mut connection = database();
        assert!(list_sealed_note_sync_outbox(&mut connection, "account", 2)
            .unwrap()
            .is_empty());
        let first = seal_persisted_intent(&mut connection, "z-note", "first", 1);
        let second = seal_persisted_intent(&mut connection, "a-note", "second", 2);
        let lifecycle_before: Vec<(String, i64, i64)> = connection.prepare(
            "SELECT lifecycle,attempt_count,local_ordinal FROM cloud_sync_outbox ORDER BY event_id"
        ).unwrap().query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?)))
            .unwrap().collect::<Result<_, _>>().unwrap();
        let once = list_sealed_note_sync_outbox(&mut connection, "account", 1).unwrap();
        assert_eq!(once.len(), 1);
        assert_eq!(once[0].event_id, first.event_id);
        assert_eq!(once[0].envelope.nonce, encode_canonical_base64url(&[1; 24]));
        let all = list_sealed_note_sync_outbox(&mut connection, "account", 2).unwrap();
        assert_eq!(
            all.iter().map(|item| &item.event_id).collect::<Vec<_>>(),
            vec![&first.event_id, &second.event_id]
        );
        let repeated = list_sealed_note_sync_outbox(&mut connection, "account", 2).unwrap();
        assert_eq!(all, repeated);
        assert!(
            list_sealed_note_sync_outbox(&mut connection, "other-account", 2)
                .unwrap()
                .is_empty()
        );
        assert!(list_sealed_note_sync_outbox(&mut connection, "account", 0).is_err());
        let lifecycle_after: Vec<(String, i64, i64)> = connection.prepare(
            "SELECT lifecycle,attempt_count,local_ordinal FROM cloud_sync_outbox ORDER BY event_id"
        ).unwrap().query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?)))
            .unwrap().collect::<Result<_, _>>().unwrap();
        assert_eq!(lifecycle_before, lifecycle_after);
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM cloud_sync_note_intents", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            0
        );
    }

    #[test]
    fn upload_acceptance_is_atomic_idempotent_and_keeps_the_encrypted_object() {
        let mut connection = database();
        let event_id = seal_persisted_intent(&mut connection, "n", "sealed upload", 0).event_id;
        let ciphertext: Vec<u8> = connection
            .query_row(
                "SELECT ciphertext FROM cloud_sync_event_objects WHERE event_id=?1",
                [&event_id],
                |row| row.get(0),
            )
            .unwrap();
        let command = CommitNoteSyncUploadAcceptanceCommand {
            account_id: "account".to_string(),
            device_id: DEVICE_ID.to_string(),
            receipts: vec![NoteSyncUploadReceipt {
                event_id: event_id.clone(),
                server_sequence: 7,
                duplicate: false,
            }],
        };
        assert_eq!(
            commit_note_sync_upload_acceptance(&mut connection, &command).unwrap(),
            vec![CommitNoteSyncUploadAcceptanceResult::Accepted]
        );
        assert!(list_sealed_note_sync_outbox(&mut connection, "account", 10)
            .unwrap()
            .is_empty());
        assert_eq!(
            connection
                .query_row(
                    "SELECT ciphertext FROM cloud_sync_event_objects WHERE event_id=?1",
                    [&event_id],
                    |row| row.get::<_, Vec<u8>>(0)
                )
                .unwrap(),
            ciphertext
        );
        assert_eq!(
            commit_note_sync_upload_acceptance(&mut connection, &command).unwrap(),
            vec![CommitNoteSyncUploadAcceptanceResult::AlreadyAccepted]
        );
        let duplicate_response = CommitNoteSyncUploadAcceptanceCommand {
            receipts: vec![NoteSyncUploadReceipt {
                duplicate: true,
                ..command.receipts[0].clone()
            }],
            ..command.clone()
        };
        assert_eq!(
            commit_note_sync_upload_acceptance(&mut connection, &duplicate_response).unwrap(),
            vec![CommitNoteSyncUploadAcceptanceResult::AlreadyAccepted]
        );
        let conflicting = CommitNoteSyncUploadAcceptanceCommand {
            receipts: vec![NoteSyncUploadReceipt {
                server_sequence: 8,
                ..command.receipts[0].clone()
            }],
            ..command
        };
        assert!(matches!(
            commit_note_sync_upload_acceptance(&mut connection, &conflicting),
            Err(NoteSyncError::ConflictingUploadReceipt)
        ));
        assert_eq!(
            connection
                .query_row(
                    "SELECT lifecycle FROM cloud_sync_outbox WHERE event_id=?1",
                    [&event_id],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            "accepted"
        );
    }

    #[test]
    fn upload_failure_backoff_is_durable_and_permanent_failure_does_not_starve_later_events() {
        let mut connection = database();
        let delayed = seal_persisted_intent(&mut connection, "delayed", "delayed", 0);
        let later = seal_persisted_intent(&mut connection, "later", "later", 1);
        record_note_sync_upload_failure(
            &mut connection,
            &RecordNoteSyncUploadFailureCommand {
                account_id: "account".to_string(),
                device_id: DEVICE_ID.to_string(),
                event_ids: vec![delayed.event_id.clone()],
                error_code: NoteSyncUploadErrorCode::RequestTimeout,
            },
        )
        .unwrap();
        let row: (i64, String, Option<String>) = connection.query_row(
            "SELECT attempt_count,last_error,next_attempt_at FROM cloud_sync_outbox WHERE event_id=?1", [&delayed.event_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).unwrap();
        assert_eq!((row.0, row.1), (1, "request_timeout".to_string()));
        assert!(row.2.is_some());
        assert_eq!(
            list_sealed_note_sync_outbox(&mut connection, "account", 10)
                .unwrap()
                .iter()
                .map(|item| &item.event_id)
                .collect::<Vec<_>>(),
            vec![&later.event_id]
        );
        record_note_sync_upload_failure(
            &mut connection,
            &RecordNoteSyncUploadFailureCommand {
                account_id: "account".to_string(),
                device_id: DEVICE_ID.to_string(),
                event_ids: vec![delayed.event_id.clone()],
                error_code: NoteSyncUploadErrorCode::ConflictingEvent,
            },
        )
        .unwrap();
        assert_eq!(
            connection
                .query_row(
                    "SELECT lifecycle FROM cloud_sync_outbox WHERE event_id=?1",
                    [&delayed.event_id],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            "rejected"
        );
        assert_eq!(
            list_sealed_note_sync_outbox(&mut connection, "account", 10)
                .unwrap()
                .iter()
                .map(|item| &item.event_id)
                .collect::<Vec<_>>(),
            vec![&later.event_id]
        );
    }

    #[test]
    fn sealed_outbox_enforces_dependencies_objects_and_batch_limits_after_restart() {
        let (root, path) = temporary_database_path("sealed-outbox");
        let mut connection = crate::sqlite::open_database(&path).unwrap();
        configure_database(&connection);
        let parent = seal_persisted_intent(&mut connection, "n", "parent", 3);
        let child = seal_persisted_intent(&mut connection, "n", "child", 4);
        let listed = list_sealed_note_sync_outbox(&mut connection, "account", 2).unwrap();
        assert_eq!(
            listed.iter().map(|item| &item.event_id).collect::<Vec<_>>(),
            vec![&parent.event_id, &child.event_id]
        );
        assert_eq!(
            listed[1].parent_event_id.as_deref(),
            Some(parent.event_id.as_str())
        );
        connection
            .execute(
                "UPDATE cloud_sync_event_objects SET ciphertext=?1 WHERE event_id=?2",
                rusqlite::params![
                    vec![9_u8; MAX_ENCRYPTED_SYNC_CIPHERTEXT_BYTES],
                    parent.event_id
                ],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE cloud_sync_event_objects SET ciphertext=?1 WHERE event_id=?2",
                rusqlite::params![
                    vec![8_u8; MAX_ENCRYPTED_SYNC_CIPHERTEXT_BYTES],
                    child.event_id
                ],
            )
            .unwrap();
        assert_eq!(
            list_sealed_note_sync_outbox(&mut connection, "account", 2)
                .unwrap()
                .len(),
            1
        );
        drop(connection);
        let mut reopened = crate::sqlite::open_database(&path).unwrap();
        assert_eq!(
            list_sealed_note_sync_outbox(&mut reopened, "account", 2)
                .unwrap()
                .len(),
            1
        );
        reopened
            .execute(
                "DELETE FROM cloud_sync_event_objects WHERE event_id=?1",
                [&parent.event_id],
            )
            .unwrap();
        assert!(matches!(
            list_sealed_note_sync_outbox(&mut reopened, "account", 2),
            Err(NoteSyncError::InvalidOutboxRead(_))
        ));
        drop(reopened);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn sealed_outbox_never_mixes_accounts_or_devices() {
        let mut connection = database();
        let first = seal_persisted_intent(&mut connection, "account-one", "one", 6);
        let second = seal_persisted_intent(&mut connection, "account-two", "two", 7);
        connection.execute(
            "UPDATE cloud_sync_outbox SET account_id='other-account',device_id='123e4567-e89b-42d3-a456-426614174099' WHERE event_id=?1",
            [&second.event_id],
        ).unwrap();
        connection
            .execute(
                "UPDATE cloud_sync_event_objects SET account_id='other-account' WHERE event_id=?1",
                [&second.event_id],
            )
            .unwrap();
        let own = list_sealed_note_sync_outbox(&mut connection, "account", 2).unwrap();
        let other = list_sealed_note_sync_outbox(&mut connection, "other-account", 2).unwrap();
        assert_eq!(
            own.iter().map(|item| &item.event_id).collect::<Vec<_>>(),
            vec![&first.event_id]
        );
        assert_eq!(
            other.iter().map(|item| &item.event_id).collect::<Vec<_>>(),
            vec![&second.event_id]
        );
        assert_eq!(other[0].device_id, "123e4567-e89b-42d3-a456-426614174099");
    }

    #[test]
    fn sealed_outbox_round_robins_devices_durably() {
        let mut connection = database();
        let first = seal_persisted_intent(&mut connection, "first", "first", 6);
        let second = seal_persisted_intent(&mut connection, "second", "second", 7);
        connection.execute(
            "UPDATE cloud_sync_outbox SET device_id='123e4567-e89b-42d3-a456-426614174099' WHERE event_id=?1",
            [&second.event_id],
        ).unwrap();
        let first_pass = list_sealed_note_sync_outbox(&mut connection, "account", 1).unwrap();
        let second_pass = list_sealed_note_sync_outbox(&mut connection, "account", 1).unwrap();
        assert_eq!(first_pass.len(), 1);
        assert_eq!(second_pass.len(), 1);
        assert_ne!(first_pass[0].device_id, second_pass[0].device_id);
        assert_eq!(
            [
                first_pass[0].event_id.as_str(),
                second_pass[0].event_id.as_str()
            ]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>(),
            [first.event_id.as_str(), second.event_id.as_str()]
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>(),
        );
    }

    #[test]
    fn sealed_outbox_keeps_tombstones_after_project_deletion_and_rejects_corruption() {
        let mut connection = database();
        let (_, snapshot) = persist_note_intent(&mut connection, "n", "tombstone");
        let note_value: serde_json::Value = serde_json::from_str(&snapshot).unwrap();
        let transaction = connection.transaction().unwrap();
        let event =
            prepare_note_delete_intent(&transaction, "p", "n", &note_value, "2026-09-22T00:01:00Z")
                .unwrap()
                .unwrap();
        transaction
            .execute("DELETE FROM notes WHERE id='n'", [])
            .unwrap();
        transaction
            .execute("DELETE FROM project_order WHERE project_id='p'", [])
            .unwrap();
        transaction
            .execute("DELETE FROM cloud_sync_project_bindings", [])
            .unwrap();
        transaction
            .execute("DELETE FROM projects WHERE id='p'", [])
            .unwrap();
        transaction.commit().unwrap();
        assert_eq!(
            commit_sealed_note_sync_event(
                &mut connection,
                &seal_command(&event.event_id, event.mutation_generation, envelope(5)),
            )
            .unwrap(),
            CommitSealedNoteSyncEventResult::Sealed
        );
        let listed = list_sealed_note_sync_outbox(&mut connection, "account", 1).unwrap();
        assert_eq!(listed[0].event_id, event.event_id);
        assert_eq!(listed[0].operation, NoteSyncOperation::Delete);
        connection
            .execute(
                "UPDATE cloud_sync_event_objects SET crypto_version=2 WHERE event_id=?1",
                [&event.event_id],
            )
            .unwrap();
        assert!(matches!(
            list_sealed_note_sync_outbox(&mut connection, "account", 1),
            Err(NoteSyncError::InvalidOutboxRead(_))
        ));
    }

    #[test]
    fn note_sync_lists_pending_intents_bounded_in_durable_round_robin_order() {
        let mut connection = database();
        let (first, first_snapshot) = persist_note_intent(&mut connection, "later-name", "first");
        let (second, _) = persist_note_intent(&mut connection, "earlier-name", "second");
        connection
            .execute("DELETE FROM cloud_sync_project_bindings", [])
            .unwrap();

        let limited = list_unsealed_note_sync_intents(&mut connection, 1, false).unwrap();
        assert_eq!(limited.len(), 1);
        assert_eq!(limited[0].event_id, first.event_id);
        assert_eq!(limited[0].account_id, "account");
        assert_eq!(limited[0].device_id, DEVICE_ID);
        assert_eq!(limited[0].project_id, "p");
        assert_eq!(limited[0].entity_id, "later-name");
        assert_eq!(limited[0].entity_type, "note");
        assert_eq!(limited[0].operation, NoteSyncOperation::Upsert);
        assert_eq!(limited[0].revision, 1);
        assert_eq!(limited[0].parent_event_id, None);
        assert_eq!(limited[0].local_ordinal, 1);
        assert_eq!(limited[0].mutation_generation, 1);
        assert_eq!(limited[0].snapshot_json, first_snapshot);
        assert_eq!(limited[0].seal_state, NoteSyncSealState::Pending);
        assert_eq!(limited[0].seal_attempt_count, 0);
        assert_eq!(limited[0].last_error_code, None);
        assert_eq!(limited[0].next_attempt_at, None);

        let all = list_unsealed_note_sync_intents(&mut connection, 10, false).unwrap();
        assert_eq!(
            all.iter()
                .map(|intent| intent.event_id.as_str())
                .collect::<Vec<_>>(),
            vec![second.event_id.as_str(), first.event_id.as_str()]
        );
        assert!(list_unsealed_note_sync_intents(&mut connection, 0, false).is_err());
        assert!(list_unsealed_note_sync_intents(&mut connection, 201, false).is_err());
    }

    #[test]
    fn note_sync_fairness_cursor_reaches_later_pending_intents_after_restart() {
        let (root, path) = temporary_database_path("pending-fairness-reopen");
        let mut connection = crate::sqlite::open_database(&path).unwrap();
        configure_database(&connection);
        let mut event_ids = Vec::new();
        for index in 0..10 {
            let note_id = format!("note-{index:02}");
            event_ids.push(
                persist_note_intent(&mut connection, &note_id, "pending")
                    .0
                    .event_id,
            );
        }

        let first = list_unsealed_note_sync_intents(&mut connection, 8, false).unwrap();
        assert_eq!(first.len(), 8);
        assert_eq!(first[0].event_id, event_ids[0]);
        assert_eq!(first[7].event_id, event_ids[7]);
        drop(connection);

        let mut connection = crate::sqlite::open_database(&path).unwrap();
        let second = list_unsealed_note_sync_intents(&mut connection, 8, false).unwrap();
        assert!(second.iter().any(|intent| intent.event_id == event_ids[8]));
        assert!(second.iter().any(|intent| intent.event_id == event_ids[9]));
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM cloud_sync_note_intents", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            10
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM cloud_sync_outbox WHERE lifecycle='unsealed'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            10
        );
        drop(connection);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn note_sync_retry_blocked_cursor_reaches_later_eligible_intent_after_restart() {
        let (root, path) = temporary_database_path("blocked-fairness-reopen");
        let mut connection = crate::sqlite::open_database(&path).unwrap();
        configure_database(&connection);
        let mut event_ids = Vec::new();
        for index in 0..34 {
            let note_id = format!("note-{index:02}");
            let prepared = persist_note_intent(&mut connection, &note_id, "blocked").0;
            if index < 33 {
                assert_eq!(
                    record_note_sync_seal_failure(
                        &mut connection,
                        &failure_command(
                            &prepared.event_id,
                            prepared.mutation_generation,
                            NoteSyncSealErrorCode::DependencyNotSynced,
                        ),
                    )
                    .unwrap(),
                    RecordNoteSyncSealFailureResult::Recorded
                );
            }
            event_ids.push(prepared.event_id);
        }

        let regular = list_unsealed_note_sync_intents(&mut connection, 8, false).unwrap();
        assert_eq!(regular.len(), 1);
        assert_eq!(regular[0].event_id, event_ids[33]);

        let first_retry = list_unsealed_note_sync_intents(&mut connection, 32, true).unwrap();
        assert_eq!(first_retry.len(), 32);
        assert!(!first_retry
            .iter()
            .any(|intent| intent.event_id == event_ids[33]));
        drop(connection);

        let mut connection = crate::sqlite::open_database(&path).unwrap();
        let second_retry = list_unsealed_note_sync_intents(&mut connection, 32, true).unwrap();
        assert!(second_retry
            .iter()
            .any(|intent| intent.event_id == event_ids[33]));
        assert_eq!(
            connection
                .query_row(
                    "SELECT max(seal_attempt_count) FROM cloud_sync_note_intents",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM cloud_sync_note_intents", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            34
        );
        drop(connection);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn note_sync_blocked_failure_is_durable_after_database_reopen() {
        let (root, path) = temporary_database_path("blocked-reopen");
        let mut connection = crate::sqlite::open_database(&path).unwrap();
        configure_database(&connection);
        let (intent, snapshot) = persist_note_intent(&mut connection, "n", "blocked");
        assert_eq!(
            record_note_sync_seal_failure(
                &mut connection,
                &failure_command(
                    &intent.event_id,
                    intent.mutation_generation,
                    NoteSyncSealErrorCode::KeyUnavailable,
                ),
            )
            .unwrap(),
            RecordNoteSyncSealFailureResult::Recorded
        );
        drop(connection);

        let mut connection = crate::sqlite::open_database(&path).unwrap();
        let intents = list_unsealed_note_sync_intents(&mut connection, 10, true).unwrap();
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].seal_state, NoteSyncSealState::Blocked);
        assert_eq!(intents[0].seal_attempt_count, 1);
        assert_eq!(
            intents[0].last_error_code.as_deref(),
            Some("key_unavailable")
        );
        assert_eq!(intents[0].snapshot_json, snapshot);
        assert_eq!(intents[0].next_attempt_at, None);
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM cloud_sync_outbox", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        drop(connection);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn note_sync_retryable_failure_has_durable_bounded_backoff() {
        let mut connection = database();
        let (intent, _) = persist_note_intent(&mut connection, "n", "retry");
        record_note_sync_seal_failure(
            &mut connection,
            &failure_command(
                &intent.event_id,
                intent.mutation_generation,
                NoteSyncSealErrorCode::RuntimeUnavailable,
            ),
        )
        .unwrap();

        let stored = connection
            .query_row(
                "SELECT seal_state,seal_attempt_count,last_error_code,next_attempt_at,
                        state_updated_at,next_attempt_at > state_updated_at
                 FROM cloud_sync_note_intents WHERE event_id=?1",
                [&intent.event_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, i64>(5)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(stored.0, "retryable_error");
        assert_eq!(stored.1, 1);
        assert_eq!(stored.2, "runtime_unavailable");
        assert!(stored.3.is_some());
        assert_eq!(stored.5, 1);
        assert!(list_unsealed_note_sync_intents(&mut connection, 10, false)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn note_sync_new_mutation_resets_sealing_failure() {
        let mut connection = database();
        let (first, _) = persist_note_intent(&mut connection, "n", "first");
        record_note_sync_seal_failure(
            &mut connection,
            &failure_command(
                &first.event_id,
                first.mutation_generation,
                NoteSyncSealErrorCode::PayloadTooLarge,
            ),
        )
        .unwrap();
        let (second, second_snapshot) = persist_note_intent(&mut connection, "n", "second");

        assert_eq!(second.event_id, first.event_id);
        assert_eq!(second.mutation_generation, 2);
        let stored = connection
            .query_row(
                "SELECT seal_state,seal_attempt_count,last_error_code,next_attempt_at,snapshot_json
                 FROM cloud_sync_note_intents WHERE event_id=?1",
                [&first.event_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(stored.0, "pending");
        assert_eq!(stored.1, 0);
        assert_eq!(stored.2, None);
        assert_eq!(stored.3, None);
        assert_eq!(stored.4, second_snapshot);
    }

    #[test]
    fn note_sync_stale_failure_cannot_overwrite_new_generation() {
        let mut connection = database();
        let (first, _) = persist_note_intent(&mut connection, "n", "first");
        let (second, second_snapshot) = persist_note_intent(&mut connection, "n", "second");
        assert_eq!(
            record_note_sync_seal_failure(
                &mut connection,
                &failure_command(
                    &first.event_id,
                    first.mutation_generation,
                    NoteSyncSealErrorCode::CryptoContextInvalid,
                ),
            )
            .unwrap(),
            RecordNoteSyncSealFailureResult::StaleGeneration
        );
        let intent = list_unsealed_note_sync_intents(&mut connection, 10, false)
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(intent.mutation_generation, second.mutation_generation);
        assert_eq!(intent.snapshot_json, second_snapshot);
        assert_eq!(intent.seal_state, NoteSyncSealState::Pending);
        assert_eq!(intent.seal_attempt_count, 0);
    }

    #[test]
    fn note_sync_failure_codes_have_bounded_lifecycle_classification() {
        for code in [
            NoteSyncSealErrorCode::KeyUnavailable,
            NoteSyncSealErrorCode::PayloadTooLarge,
            NoteSyncSealErrorCode::EncryptedSyncObjectTooLarge,
            NoteSyncSealErrorCode::DependencyNotSynced,
            NoteSyncSealErrorCode::UnsupportedContentFormat,
        ] {
            assert_eq!(code.seal_state(), NoteSyncSealState::Blocked);
        }
        assert_eq!(
            NoteSyncSealErrorCode::RuntimeUnavailable.seal_state(),
            NoteSyncSealState::RetryableError
        );
        for code in [
            NoteSyncSealErrorCode::InvalidNotePayload,
            NoteSyncSealErrorCode::CryptoContextInvalid,
            NoteSyncSealErrorCode::InvalidSyncMetadata,
            NoteSyncSealErrorCode::InvalidEnvelope,
            NoteSyncSealErrorCode::MetadataMismatch,
        ] {
            assert_eq!(code.seal_state(), NoteSyncSealState::InvariantError);
        }
    }

    #[test]
    fn note_sync_envelope_requires_canonical_unpadded_base64url() {
        assert_eq!(decode_canonical_base64url("_w", 1, 1).unwrap(), vec![0xff]);
        assert!(decode_canonical_base64url("_x", 1, 1).is_err());
        assert!(decode_canonical_base64url("_w=", 1, 1).is_err());
        assert!(decode_canonical_base64url("+w", 1, 1).is_err());
    }

    #[test]
    fn note_sync_sealing_command_rejects_plaintext_and_unknown_fields() {
        let value = serde_json::json!({
            "event_id": "123e4567-e89b-42d3-a456-426614174001",
            "expected_mutation_generation": 1,
            "envelope": {
                "crypto_version": 1,
                "aad_version": 1,
                "nonce": encode_base64url(&[0_u8; 24]),
                "ciphertext": encode_base64url(&[0_u8; 16]),
            },
            "plaintext": {"content": "must not be accepted"},
        });
        assert!(serde_json::from_value::<CommitSealedNoteSyncEventCommand>(value).is_err());
    }

    #[test]
    fn note_sync_successful_sealing_stores_object_and_removes_plaintext() {
        let mut connection = database();
        let (intent, _) = persist_note_intent(&mut connection, "n", "sealed");
        let command = seal_command(&intent.event_id, intent.mutation_generation, envelope(7));
        assert_eq!(
            commit_sealed_note_sync_event(&mut connection, &command).unwrap(),
            CommitSealedNoteSyncEventResult::Sealed
        );

        let stored = connection
            .query_row(
                "SELECT event.lifecycle,object.account_id,object.crypto_version,
                        object.aad_version,length(object.nonce),length(object.ciphertext)
                 FROM cloud_sync_outbox AS event
                 JOIN cloud_sync_event_objects AS object ON object.event_id=event.event_id
                    AND object.account_id=event.account_id
                 WHERE event.event_id=?1",
                [&intent.event_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            stored,
            ("sealed".to_string(), "account".to_string(), 1, 1, 24, 16)
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM cloud_sync_note_intents WHERE event_id=?1",
                    [&intent.event_id],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
        assert!(list_unsealed_note_sync_intents(&mut connection, 10, false)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn note_sync_ciphertext_survives_database_reopen() {
        let (root, path) = temporary_database_path("ciphertext-reopen");
        let mut connection = crate::sqlite::open_database(&path).unwrap();
        configure_database(&connection);
        let (intent, _) = persist_note_intent(&mut connection, "n", "reopen");
        let expected = vec![10_u8; 16];
        commit_sealed_note_sync_event(
            &mut connection,
            &seal_command(&intent.event_id, intent.mutation_generation, envelope(9)),
        )
        .unwrap();
        drop(connection);

        let mut connection = crate::sqlite::open_database(&path).unwrap();
        let ciphertext = connection
            .query_row(
                "SELECT ciphertext FROM cloud_sync_event_objects WHERE event_id=?1",
                [&intent.event_id],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .unwrap();
        assert_eq!(ciphertext, expected);
        assert!(list_unsealed_note_sync_intents(&mut connection, 10, false)
            .unwrap()
            .is_empty());
        drop(connection);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn note_sync_generation_race_rejects_stale_ciphertext() {
        let mut connection = database();
        let (first, _) = persist_note_intent(&mut connection, "n", "snapshot-a");
        let listed = list_unsealed_note_sync_intents(&mut connection, 10, false)
            .unwrap()
            .pop()
            .unwrap();
        let (second, second_snapshot) = persist_note_intent(&mut connection, "n", "snapshot-b");
        assert_eq!(
            commit_sealed_note_sync_event(
                &mut connection,
                &seal_command(&first.event_id, listed.mutation_generation, envelope(1)),
            )
            .unwrap(),
            CommitSealedNoteSyncEventResult::StaleGeneration
        );
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM cloud_sync_event_objects", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            0
        );
        let remaining = list_unsealed_note_sync_intents(&mut connection, 10, false)
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(remaining.event_id, first.event_id);
        assert_eq!(remaining.mutation_generation, second.mutation_generation);
        assert_eq!(remaining.snapshot_json, second_snapshot);
    }

    #[test]
    fn note_sync_sealing_rolls_back_when_object_insert_is_followed_by_failure() {
        let mut connection = database();
        let (intent, snapshot) = persist_note_intent(&mut connection, "n", "rollback-object");
        connection
            .execute_batch(
                "CREATE TRIGGER note_sync_test_fail_after_object
             BEFORE DELETE ON cloud_sync_note_intents
             BEGIN SELECT RAISE(ABORT,'injected_after_object_insert'); END;",
            )
            .unwrap();
        assert!(commit_sealed_note_sync_event(
            &mut connection,
            &seal_command(&intent.event_id, intent.mutation_generation, envelope(2)),
        )
        .is_err());
        connection
            .execute_batch("DROP TRIGGER note_sync_test_fail_after_object;")
            .unwrap();

        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM cloud_sync_event_objects", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            0
        );
        let remaining = list_unsealed_note_sync_intents(&mut connection, 10, false)
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(remaining.snapshot_json, snapshot);
    }

    #[test]
    fn note_sync_sealing_rolls_back_temporary_sidecar_delete() {
        let mut connection = database();
        let (intent, snapshot) = persist_note_intent(&mut connection, "n", "rollback-sidecar");
        connection
            .execute_batch(
                "CREATE TRIGGER note_sync_test_fail_lifecycle
             BEFORE UPDATE OF lifecycle ON cloud_sync_outbox
             WHEN NEW.lifecycle='sealed'
             BEGIN SELECT RAISE(ABORT,'injected_before_lifecycle_update'); END;",
            )
            .unwrap();
        assert!(commit_sealed_note_sync_event(
            &mut connection,
            &seal_command(&intent.event_id, intent.mutation_generation, envelope(3)),
        )
        .is_err());
        connection
            .execute_batch("DROP TRIGGER note_sync_test_fail_lifecycle;")
            .unwrap();

        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM cloud_sync_event_objects", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            0
        );
        let remaining = list_unsealed_note_sync_intents(&mut connection, 10, false)
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(remaining.snapshot_json, snapshot);
    }

    #[test]
    fn note_sync_duplicate_sealing_is_idempotent_only_for_matching_envelope() {
        let mut connection = database();
        let (intent, _) = persist_note_intent(&mut connection, "n", "duplicate");
        let command = seal_command(&intent.event_id, intent.mutation_generation, envelope(4));
        assert_eq!(
            commit_sealed_note_sync_event(&mut connection, &command).unwrap(),
            CommitSealedNoteSyncEventResult::Sealed
        );
        assert_eq!(
            commit_sealed_note_sync_event(&mut connection, &command).unwrap(),
            CommitSealedNoteSyncEventResult::AlreadySealed
        );
        let conflicting = seal_command(&intent.event_id, intent.mutation_generation, envelope(5));
        assert!(matches!(
            commit_sealed_note_sync_event(&mut connection, &conflicting),
            Err(NoteSyncError::ConflictingEncryptedObject)
        ));
        let stored: Vec<u8> = connection
            .query_row(
                "SELECT ciphertext FROM cloud_sync_event_objects WHERE event_id=?1",
                [&intent.event_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, vec![5_u8; 16]);
    }

    #[test]
    fn note_sync_rejected_commit_preserves_intent_then_retries_idempotently() {
        let mut connection = database();
        let (intent, snapshot) = persist_note_intent(&mut connection, "n", "retry-after-reject");
        let command = seal_command(&intent.event_id, intent.mutation_generation, envelope(11));
        connection
            .execute_batch(
                "CREATE TRIGGER note_sync_test_reject_commit
                 BEFORE DELETE ON cloud_sync_note_intents
                 BEGIN SELECT RAISE(ABORT,'injected_commit_rejection'); END;",
            )
            .unwrap();

        assert!(commit_sealed_note_sync_event(&mut connection, &command).is_err());
        assert_eq!(
            connection
                .query_row(
                    "SELECT snapshot_json FROM cloud_sync_note_intents WHERE event_id=?1",
                    [&intent.event_id],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            snapshot
        );
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM cloud_sync_event_objects", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            0
        );

        connection
            .execute_batch("DROP TRIGGER note_sync_test_reject_commit;")
            .unwrap();
        assert_eq!(
            commit_sealed_note_sync_event(&mut connection, &command).unwrap(),
            CommitSealedNoteSyncEventResult::Sealed
        );
        assert_eq!(
            commit_sealed_note_sync_event(&mut connection, &command).unwrap(),
            CommitSealedNoteSyncEventResult::AlreadySealed
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM cloud_sync_note_intents WHERE event_id=?1",
                    [&intent.event_id],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
    }

    #[test]
    fn note_sync_rejects_invalid_nonce_and_ciphertext_lengths() {
        let mut connection = database();
        let (intent, _) = persist_note_intent(&mut connection, "n", "invalid-envelope");
        let invalid_envelopes = [
            EncryptedNoteSyncEnvelope {
                crypto_version: 1,
                aad_version: 1,
                nonce: encode_base64url(&[0_u8; 23]),
                ciphertext: encode_base64url(&[0_u8; 16]),
            },
            EncryptedNoteSyncEnvelope {
                crypto_version: 1,
                aad_version: 1,
                nonce: encode_base64url(&[0_u8; 24]),
                ciphertext: encode_base64url(&[0_u8; 15]),
            },
            EncryptedNoteSyncEnvelope {
                crypto_version: 2,
                aad_version: 1,
                nonce: encode_base64url(&[0_u8; 24]),
                ciphertext: encode_base64url(&[0_u8; 16]),
            },
        ];
        for invalid in invalid_envelopes {
            assert!(matches!(
                commit_sealed_note_sync_event(
                    &mut connection,
                    &seal_command(&intent.event_id, intent.mutation_generation, invalid),
                ),
                Err(NoteSyncError::InvalidEnvelope(_))
            ));
        }
        let oversized = EncryptedNoteSyncEnvelope {
            crypto_version: 1,
            aad_version: 1,
            nonce: encode_base64url(&[0_u8; 24]),
            ciphertext: encode_base64url(&vec![0_u8; MAX_ENCRYPTED_SYNC_CIPHERTEXT_BYTES + 1]),
        };
        assert!(matches!(
            commit_sealed_note_sync_event(
                &mut connection,
                &seal_command(&intent.event_id, intent.mutation_generation, oversized),
            ),
            Err(NoteSyncError::InvalidEnvelope(_))
        ));
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM cloud_sync_event_objects", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            0
        );
    }

    #[test]
    fn note_sync_sealed_event_without_object_is_an_invariant_error() {
        let mut connection = database();
        let (intent, _) = persist_note_intent(&mut connection, "n", "missing-object");
        connection
            .execute(
                "DELETE FROM cloud_sync_note_intents WHERE event_id=?1",
                [&intent.event_id],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE cloud_sync_outbox SET lifecycle='sealed' WHERE event_id=?1",
                [&intent.event_id],
            )
            .unwrap();

        assert!(matches!(
            commit_sealed_note_sync_event(
                &mut connection,
                &seal_command(&intent.event_id, intent.mutation_generation, envelope(6)),
            ),
            Err(NoteSyncError::SealedObjectMissing)
        ));
        assert!(matches!(
            record_note_sync_seal_failure(
                &mut connection,
                &failure_command(
                    &intent.event_id,
                    intent.mutation_generation,
                    NoteSyncSealErrorCode::RuntimeUnavailable,
                ),
            ),
            Err(NoteSyncError::SealedObjectMissing)
        ));
    }

    #[test]
    fn note_sync_deleted_project_tombstone_can_be_sealed() {
        let mut connection = database();
        let (_, snapshot) = persist_note_intent(&mut connection, "n", "delete-me");
        let note_value: serde_json::Value = serde_json::from_str(&snapshot).unwrap();
        let transaction = connection.transaction().unwrap();
        let tombstone =
            prepare_note_delete_intent(&transaction, "p", "n", &note_value, "2026-09-22T00:01:00Z")
                .unwrap()
                .unwrap();
        transaction
            .execute("DELETE FROM notes WHERE id='n'", [])
            .unwrap();
        transaction
            .execute("DELETE FROM project_order WHERE project_id='p'", [])
            .unwrap();
        transaction
            .execute(
                "DELETE FROM cloud_sync_project_bindings WHERE project_id='p'",
                [],
            )
            .unwrap();
        transaction
            .execute("DELETE FROM projects WHERE id='p'", [])
            .unwrap();
        transaction.commit().unwrap();

        let listed = list_unsealed_note_sync_intents(&mut connection, 10, false)
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(listed.operation, NoteSyncOperation::Delete);
        assert_eq!(listed.event_id, tombstone.event_id);
        assert_eq!(listed.account_id, "account");
        assert_eq!(
            commit_sealed_note_sync_event(
                &mut connection,
                &seal_command(
                    &tombstone.event_id,
                    tombstone.mutation_generation,
                    envelope(8),
                ),
            )
            .unwrap(),
            CommitSealedNoteSyncEventResult::Sealed
        );
    }

    #[test]
    fn note_sync_sealed_head_creates_next_revision_and_parent() {
        let mut connection = database();
        let first_snapshot = note("first", 0);
        let transaction = connection.transaction().unwrap();
        let first = prepare_unsealed_note_intent(&transaction, prepare(&first_snapshot))
            .unwrap()
            .unwrap();
        transaction.execute("INSERT INTO notes(id,project_id,stage_id,updated_at,payload_json) VALUES('n','p',NULL,'2026-09-22T00:00:00Z',?1)", [&first_snapshot]).unwrap();
        transaction.commit().unwrap();
        connection
            .execute(
                "DELETE FROM cloud_sync_note_intents WHERE event_id=?1",
                [&first.event_id],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE cloud_sync_outbox SET lifecycle='sealed' WHERE event_id=?1",
                [&first.event_id],
            )
            .unwrap();

        let second_snapshot = note("second", 1);
        let transaction = connection.transaction().unwrap();
        let second = prepare_unsealed_note_intent(&transaction, prepare(&second_snapshot))
            .unwrap()
            .unwrap();
        transaction
            .execute(
                "UPDATE notes SET updated_at='2026-09-22T00:00:00Z',payload_json=?1 \
                 WHERE id='n'",
                [&second_snapshot],
            )
            .unwrap();
        transaction.commit().unwrap();

        assert_ne!(second.event_id, first.event_id);
        assert_eq!(second.revision, 2);
        assert_eq!(
            second.parent_event_id.as_deref(),
            Some(first.event_id.as_str())
        );
        assert_eq!(second.local_ordinal, first.local_ordinal + 1);
        assert_eq!(second.mutation_generation, 1);
        assert!(!second.coalesced);
    }

    #[test]
    fn note_sync_failed_mutation_rolls_back_prepared_intent() {
        let mut connection = database();
        let snapshot = note("prepared", 0);
        {
            let transaction = connection.transaction().unwrap();
            prepare_unsealed_note_intent(&transaction, prepare(&snapshot))
                .unwrap()
                .unwrap();
            let mismatched = note("different", 0);
            assert!(transaction
                .execute(
                    "INSERT INTO notes(id,project_id,stage_id,updated_at,payload_json) \
                     VALUES('n','p',NULL,'now',?1)",
                    [&mismatched],
                )
                .is_err());
        }

        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM cloud_sync_outbox", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            0
        );
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM cloud_sync_note_intents", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            0
        );
    }

    fn direct_database(bound: bool) -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        crate::sqlite::apply_migrations(&connection).unwrap();
        configure_direct_database(&connection, bound);
        connection
    }

    fn configure_direct_database(connection: &Connection, bound: bool) {
        connection
            .execute(
                "INSERT INTO mirror_state(
                    id,source_format,source_schema_version,sync_status
                 ) VALUES(1,'test','1','healthy')",
                [],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE storage_ownership SET owner='sqlite' WHERE subsystem='notes'",
                [],
            )
            .unwrap();
        connection.execute("INSERT INTO projects(id,name,infinite,unit,status,payload_json) VALUES('project','Project',0,'symbols','active','{}')", []).unwrap();
        connection
            .execute(
                "INSERT INTO project_order(project_id,position) VALUES('project',0)",
                [],
            )
            .unwrap();
        if bound {
            bind_direct_project(connection);
        }
    }

    fn bind_direct_project(connection: &Connection) {
        connection.execute("INSERT INTO cloud_sync_state(account_id,device_id,pull_cursor,ack_cursor,created_at,updated_at) VALUES('account',?1,0,0,'now','now')", [DEVICE_ID]).unwrap();
        connection.execute("INSERT INTO cloud_sync_project_bindings(project_id,account_id,created_at,updated_at) VALUES('project','account','now','now')", []).unwrap();
    }

    fn add_stage(connection: &Connection) {
        connection.execute("INSERT INTO stages(id,project_id,name,infinite,unit,status,payload_json) VALUES('stage','project','Stage',0,'symbols','active','{}')", []).unwrap();
        connection
            .execute(
                "INSERT INTO stage_order(stage_id,project_id,position) VALUES('stage','project',0)",
                [],
            )
            .unwrap();
    }

    fn outbox_identity(
        connection: &Connection,
        note_id: &str,
    ) -> (String, i64, Option<String>, i64, String) {
        connection
            .query_row(
                "SELECT event.event_id,event.revision,event.parent_event_id,
                        intent.mutation_generation,event.operation
                 FROM cloud_sync_outbox AS event
                 JOIN cloud_sync_note_intents AS intent USING(event_id)
                 WHERE event.entity_id=?1",
                [note_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap()
    }

    #[test]
    fn note_sync_direct_create_survives_database_reopen() {
        let root = std::env::temp_dir().join(format!(
            "nfprogress-note-sync-restart-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("nfprogress.db");
        let mut connection = crate::sqlite::open_database(&path).unwrap();
        configure_direct_database(&connection, true);
        let snapshot =
            crate::create_note_in_connection(&mut connection, "project", None, "restart-note")
                .unwrap();
        drop(connection);

        let connection = crate::sqlite::open_database(&path).unwrap();
        let stored: String = connection
            .query_row(
                "SELECT payload_json FROM notes WHERE id='restart-note'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let event: (i64, Option<String>, i64, String, String, String, String) = connection
            .query_row(
                "SELECT event.revision,event.parent_event_id,intent.mutation_generation,
                        event.lifecycle,intent.seal_state,event.operation,intent.snapshot_json
                 FROM cloud_sync_outbox AS event
                 JOIN cloud_sync_note_intents AS intent USING(event_id)
                 WHERE event.entity_id='restart-note'",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(stored, snapshot);
        assert_eq!(
            event,
            (
                1,
                None,
                1,
                "unsealed".to_string(),
                "pending".to_string(),
                "upsert".to_string(),
                snapshot,
            )
        );
        drop(connection);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn note_sync_direct_updates_coalesce_without_advancing_sync_revision() {
        let mut connection = direct_database(true);
        crate::create_note_in_connection(&mut connection, "project", None, "note").unwrap();
        let first = outbox_identity(&connection, "note");
        crate::update_note_in_connection(
            &mut connection,
            "project",
            "note",
            &serde_json::json!({"title":"First","pinned":true,"color":"blue"}),
            None,
        )
        .unwrap();
        crate::update_note_in_connection(
            &mut connection,
            "project",
            "note",
            &serde_json::json!({"title":"Latest","tags":["tag"],"archived":true}),
            None,
        )
        .unwrap();
        let latest = outbox_identity(&connection, "note");
        let stored: String = connection
            .query_row(
                "SELECT payload_json FROM notes WHERE id='note'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let intent: String = connection
            .query_row(
                "SELECT snapshot_json FROM cloud_sync_note_intents",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let payload: serde_json::Value = serde_json::from_str(&stored).unwrap();

        assert_eq!(latest.0, first.0);
        assert_eq!(latest.1, 1);
        assert_eq!(latest.2, None);
        assert_eq!(latest.3, 3);
        assert_eq!(latest.4, "upsert");
        assert_eq!(payload["revision"], 2);
        assert_eq!(payload["title"], "Latest");
        assert_eq!(stored, intent);
    }

    #[test]
    fn cloud_applied_note_remains_editable_and_queues_the_local_change() {
        const REMOTE_HEAD: &str = "123e4567-e89b-42d3-a456-426614174120";
        let mut connection = direct_database(false);
        let received = serde_json::json!({
            "id":"received-note","project_id":"project","stage_id":null,
            "source_type":"project","source_map_id":null,"source_node_id":null,
            "content_format":"html","title":"Received","content":"<p>Remote</p>",
            "checklist":[],"color":"default","pinned":false,"archived":false,
            "sort_order":0,"tags":[],"created_at":"2026-01-01T00:00:00Z",
            "updated_at":"2026-01-01T00:00:00Z","revision":0,"metadata":{}
        });
        connection
            .execute(
                "INSERT INTO notes(id,project_id,stage_id,updated_at,payload_json)
             VALUES('received-note','project',NULL,'2026-01-01T00:00:00Z',?1)",
                [received.to_string()],
            )
            .unwrap();
        bind_direct_project(&connection);
        connection
            .execute(
                "INSERT INTO cloud_sync_entities(
                account_id,project_id,entity_id,entity_type,head_event_id,
                head_sync_revision,updated_at
             ) VALUES('account','project','received-note','note',?1,1,'2026-01-01T00:00:00Z')",
                [REMOTE_HEAD],
            )
            .unwrap();

        crate::update_note_in_connection(
            &mut connection,
            "project",
            "received-note",
            &serde_json::json!({"content":"<p>Local edit</p>"}),
            None,
        )
        .unwrap();

        let stored: String = connection
            .query_row(
                "SELECT payload_json FROM notes WHERE id='received-note'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let payload: serde_json::Value = serde_json::from_str(&stored).unwrap();
        let event = outbox_identity(&connection, "received-note");
        assert_eq!(payload["content"], "<p>Local edit</p>");
        assert_eq!(event.1, 2);
        assert_eq!(event.2.as_deref(), Some(REMOTE_HEAD));
        assert_eq!(event.4, "upsert");
    }

    #[test]
    fn note_sync_direct_delete_coalesces_or_advances_from_sealed_head() {
        let mut coalesced = direct_database(true);
        crate::create_note_in_connection(&mut coalesced, "project", None, "note").unwrap();
        let before = outbox_identity(&coalesced, "note");
        crate::delete_note_in_connection(&mut coalesced, "project", "note", None).unwrap();
        let after = outbox_identity(&coalesced, "note");
        let tombstone: serde_json::Value = serde_json::from_str(
            &coalesced
                .query_row(
                    "SELECT snapshot_json FROM cloud_sync_note_intents",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
        )
        .unwrap();
        assert_eq!(after.0, before.0);
        assert_eq!(after.1, 1);
        assert_eq!(after.3, 2);
        assert_eq!(after.4, "delete");
        assert_eq!(tombstone["id"], "note");
        assert_eq!(tombstone["source_type"], "project");
        let delete_times: (String, String) = coalesced
            .query_row(
                "SELECT updated_at,deleted_at FROM cloud_sync_outbox WHERE event_id=?1",
                [&after.0],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(delete_times.0, delete_times.1);
        assert_eq!(tombstone["deleted_at"], delete_times.0);
        assert_eq!(
            coalesced
                .query_row("SELECT count(*) FROM notes", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            0
        );

        let mut after_seal = direct_database(true);
        crate::create_note_in_connection(&mut after_seal, "project", None, "note").unwrap();
        let sealed = outbox_identity(&after_seal, "note");
        after_seal
            .execute(
                "DELETE FROM cloud_sync_note_intents WHERE event_id=?1",
                [&sealed.0],
            )
            .unwrap();
        after_seal
            .execute(
                "UPDATE cloud_sync_outbox SET lifecycle='sealed' WHERE event_id=?1",
                [&sealed.0],
            )
            .unwrap();
        crate::delete_note_in_connection(&mut after_seal, "project", "note", None).unwrap();
        let next = outbox_identity(&after_seal, "note");
        assert_ne!(next.0, sealed.0);
        assert_eq!(next.1, 2);
        assert_eq!(next.2.as_deref(), Some(sealed.0.as_str()));
        assert_eq!(next.3, 1);
        assert_eq!(next.4, "delete");
    }

    #[test]
    fn note_sync_direct_reorder_is_exact_and_rolls_back_as_one_unit() {
        let mut connection = direct_database(true);
        crate::create_note_in_connection(&mut connection, "project", None, "first").unwrap();
        crate::create_note_in_connection(&mut connection, "project", None, "second").unwrap();
        let before_first: String = connection
            .query_row(
                "SELECT payload_json FROM notes WHERE id='first'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let before_second: String = connection
            .query_row(
                "SELECT payload_json FROM notes WHERE id='second'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let generations_before: i64 = connection
            .query_row(
                "SELECT SUM(mutation_generation) FROM cloud_sync_note_intents",
                [],
                |row| row.get(0),
            )
            .unwrap();
        connection.execute_batch("CREATE TRIGGER note_sync_test_fail_second BEFORE UPDATE ON notes WHEN NEW.id='second' BEGIN SELECT RAISE(ABORT,'injected_note_failure'); END;").unwrap();
        assert!(crate::reorder_notes_in_connection(
            &mut connection,
            "project",
            &["second".to_string(), "first".to_string()],
            None,
        )
        .is_err());
        connection
            .execute_batch("DROP TRIGGER note_sync_test_fail_second;")
            .unwrap();
        assert_eq!(
            connection
                .query_row(
                    "SELECT payload_json FROM notes WHERE id='first'",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            before_first
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT payload_json FROM notes WHERE id='second'",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            before_second
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT SUM(mutation_generation) FROM cloud_sync_note_intents",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            generations_before
        );

        crate::reorder_notes_in_connection(
            &mut connection,
            "project",
            &[
                "second".to_string(),
                "missing".to_string(),
                "first".to_string(),
            ],
            None,
        )
        .unwrap();
        for (id, order) in [("second", 0), ("first", 2)] {
            let (stored, intent): (String, String) = connection
                .query_row(
                    "SELECT note.payload_json,intent.snapshot_json FROM notes AS note
                 JOIN cloud_sync_outbox AS event ON event.entity_id=note.id
                 JOIN cloud_sync_note_intents AS intent USING(event_id)
                 WHERE note.id=?1",
                    [id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            let payload: serde_json::Value = serde_json::from_str(&stored).unwrap();
            let stored_updated_at: String = connection
                .query_row("SELECT updated_at FROM notes WHERE id=?1", [id], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(stored, intent);
            assert_eq!(payload["sort_order"], order);
            assert_eq!(payload["revision"], 1);
            assert_eq!(
                payload["updated_at"].as_str(),
                Some(stored_updated_at.as_str())
            );
        }
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM cloud_sync_outbox", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            2
        );
    }

    #[test]
    fn note_sync_direct_stage_and_mindmap_stay_local_before_explicit_migration() {
        let mut stage_connection = direct_database(true);
        add_stage(&stage_connection);
        crate::create_note_in_connection(
            &mut stage_connection,
            "project",
            Some("stage"),
            "stage-note",
        )
        .unwrap();
        crate::update_note_in_connection(
            &mut stage_connection,
            "project",
            "stage-note",
            &serde_json::json!({"title":"Stage changed"}),
            Some("stage"),
        )
        .unwrap();
        assert_eq!(
            local_note_payload(&stage_connection, "stage-note")["title"],
            "Stage changed"
        );
        assert_no_note_outbox(&stage_connection);

        let mut map_connection = direct_database(false);
        let map = serde_json::json!({
            "nodeData":{"id":"root","topic":"Root","children":[]},
            "freeNodes":[{"id":"map-note","topic":"Old","children":[],"nfprogressNote":true}]
        });
        let mut project: serde_json::Value = serde_json::json!({"mindmap":map});
        map_connection
            .execute("UPDATE projects SET payload_json=?1", [project.to_string()])
            .unwrap();
        let note = serde_json::json!({
            "id":"map-note","project_id":"project","stage_id":null,"title":"",
            "content":"Old","content_format":"plain","checklist":[],"color":"default",
            "pinned":false,"archived":false,"sort_order":0,"tags":[],
            "source_type":"mindmap","source_map_id":"root","source_node_id":"map-note",
            "created_at":"now","updated_at":"now","revision":0,"metadata":{}
        });
        map_connection.execute("INSERT INTO notes(id,project_id,stage_id,updated_at,payload_json) VALUES('map-note','project',NULL,'now',?1)", [note.to_string()]).unwrap();
        crate::update_note_in_connection(
            &mut map_connection,
            "project",
            "map-note",
            &serde_json::json!({"content":"Local"}),
            None,
        )
        .unwrap();
        assert_eq!(
            map_connection
                .query_row("SELECT count(*) FROM cloud_sync_outbox", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        bind_direct_project(&map_connection);
        crate::update_note_in_connection(
            &mut map_connection,
            "project",
            "map-note",
            &serde_json::json!({"content":"New"}),
            None,
        )
        .unwrap();
        project = serde_json::from_str(
            &map_connection
                .query_row(
                    "SELECT payload_json FROM projects WHERE id='project'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
        )
        .unwrap();
        assert_eq!(project["mindmap"]["freeNodes"][0]["topic"], "New");
        assert_no_note_outbox(&map_connection);
        crate::delete_note_in_connection(&mut map_connection, "project", "map-note", None).unwrap();
        project = serde_json::from_str(
            &map_connection
                .query_row(
                    "SELECT payload_json FROM projects WHERE id='project'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
        )
        .unwrap();
        assert!(project["mindmap"]["freeNodes"]
            .as_array()
            .unwrap()
            .is_empty());
        assert_no_note_outbox(&map_connection);
    }

    #[test]
    fn note_sync_direct_local_only_and_failed_note_write_leave_no_outbox() {
        let mut local = direct_database(false);
        add_stage(&local);
        crate::create_note_in_connection(&mut local, "project", None, "local").unwrap();
        crate::create_note_in_connection(&mut local, "project", Some("stage"), "stage-local")
            .unwrap();
        crate::update_note_in_connection(
            &mut local,
            "project",
            "local",
            &serde_json::json!({"checklist":[],"tags":["x"]}),
            None,
        )
        .unwrap();
        crate::reorder_notes_in_connection(
            &mut local,
            "project",
            &["local".to_string(), "stage-local".to_string()],
            None,
        )
        .unwrap();
        crate::delete_note_in_connection(&mut local, "project", "local", None).unwrap();
        local
            .execute(
                "UPDATE projects SET status='завершен' WHERE id='project'",
                [],
            )
            .unwrap();
        assert!(
            crate::create_note_in_connection(&mut local, "project", None, "read-only").is_err()
        );
        assert_eq!(
            local
                .query_row("SELECT count(*) FROM cloud_sync_outbox", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );

        let mut failing = direct_database(true);
        failing.execute_batch("CREATE TRIGGER note_sync_test_fail_create BEFORE INSERT ON notes BEGIN SELECT RAISE(ABORT,'injected_note_failure'); END;").unwrap();
        assert!(crate::create_note_in_connection(&mut failing, "project", None, "failed").is_err());
        assert_eq!(
            failing
                .query_row("SELECT count(*) FROM notes", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            failing
                .query_row("SELECT count(*) FROM cloud_sync_outbox", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            failing
                .query_row("SELECT count(*) FROM cloud_sync_note_intents", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            0
        );
    }

    fn test_map(notes: &[(&str, &str)]) -> serde_json::Value {
        serde_json::json!({
            "nodeData":{"id":"map-root","topic":"Project","children":[]},
            "freeNodes": notes.iter().map(|(id, topic)| serde_json::json!({
                "id":id,"topic":topic,"children":[],"nfprogressNote":true
            })).collect::<Vec<_>>()
        })
    }

    fn save_test_map(connection: &mut Connection, map: serde_json::Value) -> Result<(), String> {
        let normalized = crate::mindmap::normalize(map.clone())?;
        crate::save_map_in_connection(
            connection,
            &crate::MapCommand {
                expected_heads: None,
                project_id: "project".to_string(),
                stage_id: None,
                data: map,
            },
            &normalized,
        )
    }

    fn local_note_payload(connection: &Connection, id: &str) -> serde_json::Value {
        let raw: String = connection
            .query_row("SELECT payload_json FROM notes WHERE id=?1", [id], |r| {
                r.get(0)
            })
            .unwrap();
        serde_json::from_str(&raw).unwrap()
    }
    fn assert_no_note_outbox(connection: &Connection) {
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM cloud_sync_outbox WHERE entity_type='note'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
    }

    #[test]
    fn note_sync_map_projection_create_update_delete_has_no_independent_authority() {
        let mut connection = direct_database(true);
        let id = crate::mindmap::linked_note_id("node");
        save_test_map(&mut connection, test_map(&[("node", "First")])).unwrap();
        assert_eq!(local_note_payload(&connection, &id)["content"], "First");
        assert_no_note_outbox(&connection);
        save_test_map(&mut connection, test_map(&[("node", "Latest")])).unwrap();
        assert_eq!(local_note_payload(&connection, &id)["content"], "Latest");
        assert_no_note_outbox(&connection);
        save_test_map(&mut connection, test_map(&[])).unwrap();
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM notes WHERE id=?1", [&id], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_no_note_outbox(&connection);
    }

    #[test]
    fn note_sync_load_map_reconciliation_preserves_local_projection() {
        let mut connection = direct_database(true);
        let map = test_map(&[("loaded-node", "Loaded")]);
        connection
            .execute(
                "UPDATE projects SET payload_json=?1 WHERE id='project'",
                [serde_json::json!({"mindmap":map}).to_string()],
            )
            .unwrap();

        crate::reconcile_loaded_map_view_in_connection(&mut connection, "project", None).unwrap();

        let note_id = crate::mindmap::linked_note_id("loaded-node");
        assert_eq!(
            local_note_payload(&connection, &note_id)["content"],
            "Loaded"
        );
        assert_no_note_outbox(&connection);
    }

    #[test]
    fn note_sync_local_only_load_and_save_map_create_no_outbox() {
        let mut connection = direct_database(false);
        let loaded = test_map(&[("local-node", "Loaded locally")]);
        connection
            .execute(
                "UPDATE projects SET payload_json=?1 WHERE id='project'",
                [serde_json::json!({"mindmap":loaded}).to_string()],
            )
            .unwrap();
        crate::reconcile_loaded_map_view_in_connection(&mut connection, "project", None).unwrap();
        save_test_map(
            &mut connection,
            test_map(&[("local-node", "Saved locally")]),
        )
        .unwrap();

        let note_id = crate::mindmap::linked_note_id("local-node");
        let payload: String = connection
            .query_row(
                "SELECT payload_json FROM notes WHERE id=?1",
                [&note_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&payload).unwrap()["content"],
            "Saved locally"
        );
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM cloud_sync_outbox", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            0
        );
    }

    #[test]
    fn note_sync_save_map_rolls_back_all_reconciliation_changes() {
        let mut connection = direct_database(true);
        save_test_map(
            &mut connection,
            test_map(&[("first-node", "First"), ("second-node", "Second")]),
        )
        .unwrap();
        let first_id = crate::mindmap::linked_note_id("first-node");
        let second_id = crate::mindmap::linked_note_id("second-node");
        let owner_before: String = connection
            .query_row(
                "SELECT payload_json FROM projects WHERE id='project'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let first_before = local_note_payload(&connection, &first_id);
        let second_before = local_note_payload(&connection, &second_id);
        connection
            .execute_batch(&format!(
                "CREATE TRIGGER note_sync_test_fail_map_second BEFORE UPDATE ON notes
                 WHEN NEW.id='{second_id}' BEGIN SELECT RAISE(ABORT,'injected_note_failure'); END;"
            ))
            .unwrap();

        assert!(save_test_map(
            &mut connection,
            test_map(&[
                ("first-node", "Changed first"),
                ("second-node", "Changed second")
            ]),
        )
        .is_err());
        connection
            .execute_batch("DROP TRIGGER note_sync_test_fail_map_second")
            .unwrap();

        let owner_after: String = connection
            .query_row(
                "SELECT payload_json FROM projects WHERE id='project'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(owner_after, owner_before);
        for (id, before) in [(&first_id, first_before), (&second_id, second_before)] {
            let after = local_note_payload(&connection, id);
            assert_eq!(after, before);
        }
        assert_no_note_outbox(&connection);
    }

    #[test]
    fn note_sync_combined_map_failure_rolls_back_prior_owner_and_note_changes() {
        let mut connection = direct_database(true);
        add_stage(&connection);
        connection
            .execute(
                "UPDATE projects SET payload_json=?1 WHERE id='project'",
                [serde_json::json!({
                    "combine_stage_mindmaps":true,
                    "mindmap":test_map(&[("project-existing", "Project old")])
                })
                .to_string()],
            )
            .unwrap();
        let stage_map = serde_json::json!({
            "nodeData":{"id":"stage-root","topic":"Stage","children":[]},
            "freeNodes":[
                {"id":"stage-existing","topic":"Stage old","children":[],"nfprogressNote":true}
            ]
        });
        connection
            .execute(
                "UPDATE stages SET payload_json=?1 WHERE id='stage'",
                [serde_json::json!({"mindmap":stage_map}).to_string()],
            )
            .unwrap();
        crate::reconcile_loaded_map_view_in_connection(&mut connection, "project", None).unwrap();

        let mut changed_stage_map: serde_json::Value = serde_json::from_str::<serde_json::Value>(
            &connection
                .query_row(
                    "SELECT payload_json FROM stages WHERE id='stage'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
        )
        .unwrap()["mindmap"]
            .clone();
        changed_stage_map["freeNodes"][0]["topic"] = "Stage changed".into();
        connection
            .execute(
                "UPDATE stages SET payload_json=?1 WHERE id='stage'",
                [serde_json::json!({"mindmap":changed_stage_map}).to_string()],
            )
            .unwrap();

        let (project, stages) = {
            let repository = crate::ProjectsRepository::new(&mut connection);
            (
                repository.get_project("project").unwrap().unwrap(),
                repository.list_stages("project").unwrap(),
            )
        };
        let mut combined = crate::compose_combined_map(&project, &stages).unwrap();
        combined["freeNodes"][0]["topic"] = "Project changed".into();
        let normalized = crate::mindmap::normalize(combined.clone()).unwrap();
        let project_owner_before: String = connection
            .query_row(
                "SELECT payload_json FROM projects WHERE id='project'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let stage_owner_before: String = connection
            .query_row(
                "SELECT payload_json FROM stages WHERE id='stage'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let project_note_id = crate::mindmap::linked_note_id("project-existing");
        let stage_note_id = crate::mindmap::linked_note_id("stage-existing");
        let note_state = |connection: &Connection, id: &str| local_note_payload(connection, id);
        let project_note_before = note_state(&connection, &project_note_id);
        let stage_note_before = note_state(&connection, &stage_note_id);
        connection
            .execute_batch(&format!(
                "CREATE TRIGGER note_sync_test_fail_combined_stage BEFORE UPDATE ON notes
                 WHEN NEW.id='{stage_note_id}' BEGIN SELECT RAISE(ABORT,'injected_note_failure'); END;"
            ))
            .unwrap();

        assert!(crate::save_map_in_connection(
            &mut connection,
            &crate::MapCommand {
                expected_heads: None,
                project_id: "project".to_string(),
                stage_id: None,
                data: combined,
            },
            &normalized,
        )
        .is_err());
        connection
            .execute_batch("DROP TRIGGER note_sync_test_fail_combined_stage")
            .unwrap();

        assert_eq!(
            connection
                .query_row(
                    "SELECT payload_json FROM projects WHERE id='project'",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            project_owner_before
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT payload_json FROM stages WHERE id='stage'",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            stage_owner_before
        );
        assert_eq!(
            note_state(&connection, &project_note_id),
            project_note_before
        );
        assert_eq!(note_state(&connection, &stage_note_id), stage_note_before);
        assert_no_note_outbox(&connection);
    }

    #[test]
    fn note_sync_combined_map_keeps_project_and_stage_notes_local() {
        let mut connection = direct_database(true);
        add_stage(&connection);
        let project_map = test_map(&[]);
        let stage_map = serde_json::json!({
            "nodeData":{"id":"stage-root","topic":"Stage","children":[]},
            "freeNodes":[
                {"id":"stage-node","topic":"Stage note","children":[],"nfprogressNote":true}
            ]
        });
        connection
            .execute(
                "UPDATE projects SET payload_json=?1 WHERE id='project'",
                [serde_json::json!({
                    "combine_stage_mindmaps":true,
                    "mindmap":project_map
                })
                .to_string()],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE stages SET payload_json=?1 WHERE id='stage'",
                [serde_json::json!({"mindmap":stage_map}).to_string()],
            )
            .unwrap();
        let (project, stages) = {
            let repository = crate::ProjectsRepository::new(&mut connection);
            (
                repository.get_project("project").unwrap().unwrap(),
                repository.list_stages("project").unwrap(),
            )
        };
        let mut combined = crate::compose_combined_map(&project, &stages).unwrap();
        combined["freeNodes"] = serde_json::json!([
            {"id":"project-node","topic":"Project note","children":[],"nfprogressNote":true}
        ]);
        let normalized = crate::mindmap::normalize(combined.clone()).unwrap();
        crate::save_map_in_connection(
            &mut connection,
            &crate::MapCommand {
                expected_heads: None,
                project_id: "project".to_string(),
                stage_id: None,
                data: combined,
            },
            &normalized,
        )
        .unwrap();

        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM notes", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM cloud_sync_note_intents", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            0
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM notes WHERE stage_id='stage'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
        assert_no_note_outbox(&connection);
    }
}

/// Local diagnostic evidence only; no apply/ACK authority. Account/game blocker
/// CHECK allowlists are historical, so precise frame failures use the existing
/// generic Class-C application_metadata alongside their compatible blocker.
pub(crate) fn record_frame_compression_blocker(
    tx: &rusqlite::Transaction<'_>,
    account: &str,
    event: &str,
    code: &str,
) -> Result<(), rusqlite::Error> {
    if !crate::frame_compression::CODES.contains(&code) {
        return Err(rusqlite::Error::InvalidQuery);
    }
    let key = format!(
        "frame_compression_blocker:{}",
        serde_json::json!([account, event])
    );
    tx.execute("INSERT INTO application_metadata(key,value,updated_at) VALUES(?1,?2,datetime('now')) ON CONFLICT(key) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at",rusqlite::params![key,serde_json::json!({"code":code}).to_string()])?;
    Ok(())
}
