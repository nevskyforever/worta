//! Rust-owned SQLite opening and versioned migration runner.
//!
//! The SQL files are shared with the transitional Python runtime.  Keeping a
//! single set of files prevents the two runtimes from silently creating
//! incompatible databases while Projects ownership is being cut over.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::{
    functions::FunctionFlags, Connection, OpenFlags, OptionalExtension, Transaction,
    TransactionBehavior,
};

pub const CURRENT_SCHEMA_VERSION: i64 = 38;

/// Durable inbox scheduling only: visits cannot prove apply or advance ACK.
/// Reuse schema37's visit records; event IDs are unique across entity families.
/// Call inside the reader's transaction, before handing encrypted rows to TS.
pub(crate) fn record_sync_reader_visits<'a>(
    db: &Connection,
    account: &str,
    ids: impl IntoIterator<Item = &'a str>,
) -> rusqlite::Result<()> {
    let mut ordinal: i64 = db.query_row(
        "SELECT COALESCE(MAX(ordinal),0) FROM cloud_game_reader_visits WHERE account_id=?1",
        [account],
        |r| r.get(0),
    )?;
    for id in ids {
        ordinal = ordinal
            .checked_add(1)
            .filter(|n| *n <= 9007199254740991)
            .ok_or(rusqlite::Error::InvalidQuery)?;
        db.execute(
            "INSERT INTO cloud_game_reader_visits VALUES(?1,?2,?3) ON CONFLICT(account_id,event_id) DO UPDATE SET ordinal=excluded.ordinal",
            rusqlite::params![account,id,ordinal],
        )?;
    }
    Ok(())
}

/// Every ordinary Rust connection is fail-closed.  The remote-apply command
/// installs its scoped verifier only after opening its dedicated connection.
pub(crate) fn register_fail_closed_remote_apply_guard(
    connection: &Connection,
) -> Result<(), StorageError> {
    connection.create_scalar_function(
        "note_sync_remote_apply_authorized",
        1,
        FunctionFlags::SQLITE_UTF8,
        |_context| Ok(false),
    )?;
    Ok(())
}

#[cfg(test)]
mod c16_c17_migration_tests {
    use super::*;

    #[test]
    fn populated_schema_15_advances_to_latest_without_rewriting_sync_data() {
        let connection = Connection::open_in_memory().unwrap();
        register_fail_closed_remote_apply_guard(&connection).unwrap();
        connection.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        connection.execute_batch(DOMAIN_EVENTS_SCHEMA).unwrap();
        connection
            .execute_batch("CREATE TABLE schema_info(schema_version INTEGER NOT NULL);")
            .unwrap();
        for (_, sql) in MIGRATIONS.iter().take(15) {
            connection.execute_batch(sql).unwrap();
        }
        connection
            .execute("INSERT INTO schema_info VALUES(15)", [])
            .unwrap();
        connection.execute(
            "INSERT INTO projects(id,name,infinite,unit,status,payload_json) VALUES('p','Project',1,'symbols','активен','{}')",
            [],
        ).unwrap();
        connection
            .execute(
                "INSERT INTO project_order(project_id,position) VALUES('p',0)",
                [],
            )
            .unwrap();
        connection.execute(
            "INSERT INTO cloud_sync_state(account_id,device_id,pull_cursor,ack_cursor,created_at,updated_at) VALUES('a','123e4567-e89b-42d3-a456-426614174001',0,0,'now','now')",
            [],
        ).unwrap();

        assert_eq!(
            apply_migrations(&connection).unwrap(),
            CURRENT_SCHEMA_VERSION
        );
        assert_eq!(
            connection
                .query_row("SELECT schema_version FROM schema_info", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            CURRENT_SCHEMA_VERSION
        );
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM projects", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM cloud_sync_state", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
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
    }

    #[test]
    fn populated_schema_16_advances_to_latest_with_inbox_intact() {
        let connection = Connection::open_in_memory().unwrap();
        register_fail_closed_remote_apply_guard(&connection).unwrap();
        connection.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        connection.execute_batch(DOMAIN_EVENTS_SCHEMA).unwrap();
        connection
            .execute_batch("CREATE TABLE schema_info(schema_version INTEGER NOT NULL);")
            .unwrap();
        for (_, sql) in MIGRATIONS.iter().take(16) {
            connection.execute_batch(sql).unwrap();
        }
        connection
            .execute("INSERT INTO schema_info VALUES(16)", [])
            .unwrap();
        connection
            .execute(
                "INSERT INTO cloud_sync_inbox(
                account_id,event_id,server_sequence,device_id,project_id,entity_id,
                entity_type,operation,sync_revision,updated_at,deleted_at,state,received_at
             ) VALUES('account','123e4567-e89b-42d3-a456-426614174010',1,
                      '123e4567-e89b-42d3-a456-426614174001','project','note',
                      'note','upsert',1,'2026-09-25T00:00:00.000000Z',NULL,'received','now')",
                [],
            )
            .unwrap();

        assert_eq!(
            apply_migrations(&connection).unwrap(),
            CURRENT_SCHEMA_VERSION
        );
        assert_eq!(connection.query_row(
            "SELECT state FROM cloud_sync_inbox WHERE event_id='123e4567-e89b-42d3-a456-426614174010'",
            [], |row| row.get::<_, String>(0),
        ).unwrap(), "received");
        assert_eq!(
            connection
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
                .query_row(
                    "SELECT count(*) FROM cloud_sync_note_causal_history",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM cloud_sync_note_pending_resolutions",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
    }

    #[test]
    fn populated_schema_17_advances_to_latest_and_reopens() {
        let path = std::env::temp_dir().join(format!(
            "nfprogress-schema19-from17-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        let connection = Connection::open(&path).unwrap();
        register_fail_closed_remote_apply_guard(&connection).unwrap();
        connection.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        connection.execute_batch(DOMAIN_EVENTS_SCHEMA).unwrap();
        connection
            .execute_batch("CREATE TABLE schema_info(schema_version INTEGER NOT NULL);")
            .unwrap();
        for (_, sql) in MIGRATIONS.iter().take(17) {
            connection.execute_batch(sql).unwrap();
        }
        connection
            .execute("INSERT INTO schema_info VALUES(17)", [])
            .unwrap();
        assert_eq!(
            apply_migrations(&connection).unwrap(),
            CURRENT_SCHEMA_VERSION
        );
        drop(connection);
        let reopened = open_database(&path).unwrap();
        assert_eq!(
            reopened
                .query_row("SELECT schema_version FROM schema_info", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            CURRENT_SCHEMA_VERSION
        );
        assert_eq!(
            reopened
                .query_row(
                    "SELECT count(*) FROM cloud_sync_note_pending_resolutions",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        drop(reopened);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn populated_schema_18_advances_to_latest_and_reopens() {
        let path = std::env::temp_dir().join(format!(
            "nfprogress-schema19-from18-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        let connection = Connection::open(&path).unwrap();
        register_fail_closed_remote_apply_guard(&connection).unwrap();
        connection.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        connection.execute_batch(DOMAIN_EVENTS_SCHEMA).unwrap();
        connection
            .execute_batch("CREATE TABLE schema_info(schema_version INTEGER NOT NULL);")
            .unwrap();
        for (_, sql) in MIGRATIONS.iter().take(18) {
            connection.execute_batch(sql).unwrap();
        }
        connection
            .execute("INSERT INTO schema_info VALUES(18)", [])
            .unwrap();
        assert_eq!(
            apply_migrations(&connection).unwrap(),
            CURRENT_SCHEMA_VERSION
        );
        drop(connection);
        let reopened = open_database(&path).unwrap();
        assert_eq!(
            reopened
                .query_row("SELECT schema_version FROM schema_info", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            CURRENT_SCHEMA_VERSION
        );
        assert_eq!(
            reopened
                .query_row(
                    "SELECT count(*) FROM cloud_sync_note_resolution_outbox",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        assert_eq!(
            reopened
                .query_row(
                    "SELECT count(*) FROM cloud_sync_note_resolution_dependencies",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        drop(reopened);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn populated_schema_19_through_23_advance_to_latest_and_reopen() {
        for version in [19usize, 20usize, 21usize, 22usize, 23usize] {
            let path = std::env::temp_dir().join(format!(
                "nfprogress-schema24-from{version}-{}-{}.db",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
            ));
            let connection = Connection::open(&path).unwrap();
            register_fail_closed_remote_apply_guard(&connection).unwrap();
            connection.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
            connection.execute_batch(DOMAIN_EVENTS_SCHEMA).unwrap();
            connection
                .execute_batch("CREATE TABLE schema_info(schema_version INTEGER NOT NULL);")
                .unwrap();
            for (_, sql) in MIGRATIONS.iter().take(version) {
                connection.execute_batch(sql).unwrap();
            }
            connection
                .execute("INSERT INTO schema_info VALUES(?1)", [version as i64])
                .unwrap();
            assert_eq!(
                apply_migrations(&connection).unwrap(),
                CURRENT_SCHEMA_VERSION
            );
            drop(connection);
            let reopened = open_database(&path).unwrap();
            assert_eq!(
                reopened
                    .query_row("SELECT schema_version FROM schema_info", [], |row| row
                        .get::<_, i64>(0))
                    .unwrap(),
                CURRENT_SCHEMA_VERSION
            );
            drop(reopened);
            std::fs::remove_file(path).unwrap();
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct RemoteApplyAuthorization<'a> {
    pub event_id: &'a str,
    pub account_id: &'a str,
    pub project_id: &'a str,
    pub entity_id: &'a str,
    pub operation: &'a str,
    pub payload_json: Option<&'a str>,
    pub prior_payload_json: Option<&'a str>,
}

/// Owned form used when the authorization can only be determined after the
/// remote-apply transaction has re-read the durable inbox and entity head.
#[derive(Clone, Debug)]
pub(crate) struct OwnedRemoteApplyAuthorization {
    pub event_id: String,
    pub account_id: String,
    pub project_id: String,
    pub entity_id: String,
    pub operation: String,
    pub payload_json: Option<String>,
    pub prior_payload_json: Option<String>,
}

/// A dedicated connection whose verifier is scoped to one in-process Rust
/// capability. The capability value is never returned to SQL callers.
pub(crate) struct PrivilegedRemoteApplyConnection {
    connection: Connection,
    capability: Arc<Mutex<Option<HashSet<String>>>>,
}

struct ActiveRemoteApplyCapability {
    state: Arc<Mutex<Option<HashSet<String>>>>,
}

impl Drop for ActiveRemoteApplyCapability {
    fn drop(&mut self) {
        if let Ok(mut value) = self.state.lock() {
            *value = None;
        }
    }
}

impl PrivilegedRemoteApplyConnection {
    pub(crate) fn from_connection(connection: Connection) -> Result<Self, StorageError> {
        let capability = Arc::new(Mutex::new(None::<HashSet<String>>));
        let verifier = capability.clone();
        connection.create_scalar_function(
            "note_sync_remote_apply_authorized",
            1,
            FunctionFlags::SQLITE_UTF8,
            move |context| {
                let candidate = context.get::<String>(0)?;
                Ok(verifier
                    .lock()
                    .map(|active| {
                        active
                            .as_ref()
                            .is_some_and(|values| values.contains(&candidate))
                    })
                    .unwrap_or(false))
            },
        )?;
        Ok(Self {
            connection,
            capability,
        })
    }

    pub(crate) fn authorize_once<T>(
        &mut self,
        authorization: RemoteApplyAuthorization<'_>,
        operation: impl FnOnce(&Transaction<'_>) -> rusqlite::Result<T>,
    ) -> Result<T, StorageError> {
        let capability = random_remote_apply_capability()?;
        {
            let mut active = self.capability.lock().map_err(|_| {
                StorageError::RemoteApplyAuthorization("capability state unavailable".to_string())
            })?;
            if active.is_some() {
                return Err(StorageError::RemoteApplyAuthorization(
                    "capability already active".to_string(),
                ));
            }
            *active = Some(HashSet::from([capability.clone()]));
        }
        let _active = ActiveRemoteApplyCapability {
            state: self.capability.clone(),
        };
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO cloud_sync_remote_apply_authorizations(
                event_id,account_id,project_id,entity_id,operation,
                payload_json,prior_payload_json,capability
             ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            rusqlite::params![
                authorization.event_id,
                authorization.account_id,
                authorization.project_id,
                authorization.entity_id,
                authorization.operation,
                authorization.payload_json,
                authorization.prior_payload_json,
                capability,
            ],
        )?;
        let value = operation(&transaction)?;
        let remaining: i64 = transaction.query_row(
            "SELECT count(*) FROM cloud_sync_remote_apply_authorizations
             WHERE event_id=?1",
            [authorization.event_id],
            |row| row.get(0),
        )?;
        if remaining != 0 {
            return Err(StorageError::RemoteApplyAuthorization(
                "authorization was not consumed".to_string(),
            ));
        }
        transaction.commit()?;
        Ok(value)
    }

    /// Runs a decision and the resulting write in one immediate transaction.
    /// The planner may durably classify a received event without receiving a
    /// capability; a Notes mutation receives one only after its exact scope is
    /// known from the transaction's own reads.
    pub(crate) fn execute_planned_once<P, T>(
        &mut self,
        plan: impl FnOnce(
            &Transaction<'_>,
        ) -> Result<(Option<OwnedRemoteApplyAuthorization>, P), StorageError>,
        operation: impl FnOnce(&Transaction<'_>, P) -> Result<T, StorageError>,
    ) -> Result<T, StorageError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (authorization, plan) = plan(&transaction)?;
        let active = if let Some(authorization) = authorization.as_ref() {
            let capability = random_remote_apply_capability()?;
            let mut state = self.capability.lock().map_err(|_| {
                StorageError::RemoteApplyAuthorization("capability state unavailable".to_string())
            })?;
            if state.is_some() {
                return Err(StorageError::RemoteApplyAuthorization(
                    "capability already active".to_string(),
                ));
            }
            *state = Some(HashSet::from([capability.clone()]));
            // Construct this before any fallible SQL so every error path after
            // activation restores fail-closed connection state.
            let active = ActiveRemoteApplyCapability {
                state: self.capability.clone(),
            };
            transaction.execute(
                "INSERT INTO cloud_sync_remote_apply_authorizations(
                    event_id,account_id,project_id,entity_id,operation,
                    payload_json,prior_payload_json,capability
                 ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                rusqlite::params![
                    authorization.event_id,
                    authorization.account_id,
                    authorization.project_id,
                    authorization.entity_id,
                    authorization.operation,
                    authorization.payload_json,
                    authorization.prior_payload_json,
                    capability,
                ],
            )?;
            Some(active)
        } else {
            None
        };
        let value = operation(&transaction, plan)?;
        if let Some(authorization) = authorization {
            let remaining: i64 = transaction.query_row(
                "SELECT count(*) FROM cloud_sync_remote_apply_authorizations
                 WHERE event_id=?1",
                [&authorization.event_id],
                |row| row.get(0),
            )?;
            if remaining != 0 {
                return Err(StorageError::RemoteApplyAuthorization(
                    "authorization was not consumed".to_string(),
                ));
            }
        }
        transaction.commit()?;
        drop(active);
        Ok(value)
    }

    /// Runs a decision and up to two protected Notes mutations in one
    /// immediate transaction. Each exact mutation receives an independent
    /// capability and must consume its authorization before commit.
    pub(crate) fn execute_planned_many_once<P, T, E>(
        &mut self,
        plan: impl FnOnce(&Transaction<'_>) -> Result<(Vec<OwnedRemoteApplyAuthorization>, P), E>,
        operation: impl FnOnce(&Transaction<'_>, P) -> Result<T, E>,
    ) -> Result<T, E>
    where
        E: From<StorageError> + From<rusqlite::Error>,
    {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (authorizations, plan) = plan(&transaction)?;
        let mut capabilities = Vec::with_capacity(authorizations.len());
        for _ in &authorizations {
            let mut capability = random_remote_apply_capability().map_err(E::from)?;
            while capabilities.contains(&capability) {
                capability = random_remote_apply_capability().map_err(E::from)?;
            }
            capabilities.push(capability);
        }
        let active = if authorizations.is_empty() {
            None
        } else {
            let mut state = self.capability.lock().map_err(|_| {
                E::from(StorageError::RemoteApplyAuthorization(
                    "capability state unavailable".to_string(),
                ))
            })?;
            if state.is_some() {
                return Err(E::from(StorageError::RemoteApplyAuthorization(
                    "capability already active".to_string(),
                )));
            }
            *state = Some(capabilities.iter().cloned().collect());
            let active = ActiveRemoteApplyCapability {
                state: self.capability.clone(),
            };
            for (authorization, capability) in authorizations.iter().zip(&capabilities) {
                transaction.execute(
                    "INSERT INTO cloud_sync_remote_apply_authorizations(
                        event_id,account_id,project_id,entity_id,operation,
                        payload_json,prior_payload_json,capability
                     ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                    rusqlite::params![
                        authorization.event_id,
                        authorization.account_id,
                        authorization.project_id,
                        authorization.entity_id,
                        authorization.operation,
                        authorization.payload_json,
                        authorization.prior_payload_json,
                        capability,
                    ],
                )?;
            }
            Some(active)
        };
        let value = operation(&transaction, plan)?;
        for authorization in &authorizations {
            let remaining: i64 = transaction.query_row(
                "SELECT count(*) FROM cloud_sync_remote_apply_authorizations
                 WHERE event_id=?1",
                [&authorization.event_id],
                |row| row.get(0),
            )?;
            if remaining != 0 {
                return Err(E::from(StorageError::RemoteApplyAuthorization(
                    "authorization was not consumed".to_string(),
                )));
            }
        }
        transaction.commit()?;
        drop(active);
        Ok(value)
    }

    #[cfg(test)]
    pub(crate) fn connection(&self) -> &Connection {
        &self.connection
    }

    #[cfg(test)]
    pub(crate) fn connection_mut_for_test(&mut self) -> &mut Connection {
        &mut self.connection
    }
}

pub(crate) fn open_privileged_remote_apply_database(
    path: &Path,
) -> Result<PrivilegedRemoteApplyConnection, StorageError> {
    PrivilegedRemoteApplyConnection::from_connection(open_database(path)?)
}

fn random_remote_apply_capability() -> Result<String, StorageError> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|error| StorageError::RemoteApplyAuthorization(error.to_string()))?;
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    Ok(encoded)
}

const APPLICATION_VERSION: &str = env!("CARGO_PKG_VERSION");
const VERSION_KEYS: [&str; 2] = ["data_created_by_version", "data_last_written_by_version"];
const USER_DATA_TABLES: [&str; 36] = [
    "projects",
    "stages",
    "progress_entries",
    "notes",
    "settings",
    "game_state",
    "project_order",
    "stage_order",
    "progress_order",
    "project_metadata",
    "project_folders",
    "project_folder_members",
    "project_bindings",
    "project_extensions",
    "documents",
    "document_bindings",
    "cloud_sync_state",
    "cloud_sync_outbox",
    "cloud_sync_event_objects",
    "cloud_sync_inbox",
    "cloud_sync_entities",
    "cloud_sync_project_bindings",
    "cloud_sync_note_intents",
    "cloud_sync_note_intent_cursors",
    "cloud_account_bindings",
    "cloud_sync_upload_receipts",
    "cloud_sync_note_conflict_groups",
    "cloud_sync_note_conflict_versions",
    "cloud_sync_note_conflict_tips",
    "cloud_sync_note_causal_history",
    "cloud_sync_note_pending_resolutions",
    "cloud_sync_note_resolution_outbox",
    "cloud_sync_note_resolution_dependencies",
    "cloud_sync_note_resolution_upload_receipts",
    "cloud_sync_note_applied_resolutions",
    "cloud_sync_note_applied_resolution_parents",
];

#[derive(Debug)]
pub enum StorageError {
    Database(rusqlite::Error),
    UnsupportedSchema(i64),
    CorruptSchema(String),
    RemoteApplyAuthorization(String),
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Database(error) => write!(formatter, "SQLite error: {error}"),
            Self::UnsupportedSchema(version) => {
                write!(formatter, "Unsupported SQLite schema version: {version}")
            }
            Self::CorruptSchema(message) => write!(formatter, "Corrupt SQLite schema: {message}"),
            Self::RemoteApplyAuthorization(message) => {
                write!(formatter, "Remote apply authorization failed: {message}")
            }
        }
    }
}

impl std::error::Error for StorageError {}

impl From<rusqlite::Error> for StorageError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Database(error)
    }
}

const MIGRATIONS: [(i64, &str); 38] = [
    (
        1,
        include_str!("../../../nfprogress/core/sqlite/migrations/001_initial.sql"),
    ),
    (
        2,
        include_str!("../../../nfprogress/core/sqlite/migrations/002_storage_ownership.sql"),
    ),
    (
        3,
        include_str!("../../../nfprogress/core/sqlite/migrations/003_project_order.sql"),
    ),
    (
        4,
        include_str!("../../../nfprogress/core/sqlite/migrations/004_projects_authority.sql"),
    ),
    (
        5,
        include_str!("../../../nfprogress/core/sqlite/migrations/005_game_authority.sql"),
    ),
    (
        6,
        include_str!("../../../nfprogress/core/sqlite/migrations/006_documents_authority.sql"),
    ),
    (
        7,
        include_str!("../../../nfprogress/core/sqlite/migrations/007_application_metadata.sql"),
    ),
    (
        8,
        include_str!("../../../nfprogress/core/sqlite/migrations/008_cloud_sync_protocol.sql"),
    ),
    (
        9,
        include_str!("../../../nfprogress/core/sqlite/migrations/009_encrypted_sync_substrate.sql"),
    ),
    (
        10,
        include_str!("../../../nfprogress/core/sqlite/migrations/010_note_sync_intents.sql"),
    ),
    (
        11,
        include_str!(
            "../../../nfprogress/core/sqlite/migrations/011_note_sync_intent_fairness.sql"
        ),
    ),
    (
        12,
        include_str!("../../../nfprogress/core/sqlite/migrations/012_cloud_account_bindings.sql"),
    ),
    (
        13,
        include_str!(
            "../../../nfprogress/core/sqlite/migrations/013_note_sync_upload_receipts.sql"
        ),
    ),
    (
        14,
        include_str!("../../../nfprogress/core/sqlite/migrations/014_note_sync_upload_fairness.sql"),
    ),
    (15, include_str!("../../../nfprogress/core/sqlite/migrations/015_note_sync_remote_apply.sql")),
    (16, include_str!("../../../nfprogress/core/sqlite/migrations/016_cloud_project_bootstrap.sql")),
    (17, include_str!("../../../nfprogress/core/sqlite/migrations/017_note_sync_conflicts.sql")),
    (18, include_str!("../../../nfprogress/core/sqlite/migrations/018_note_sync_pending_resolutions.sql")),
    (19, include_str!("../../../nfprogress/core/sqlite/migrations/019_note_sync_resolution_outbox.sql")),
    (20, include_str!("../../../nfprogress/core/sqlite/migrations/020_note_sync_resolution_sealing.sql")),
    (21, include_str!("../../../nfprogress/core/sqlite/migrations/021_note_sync_resolution_upload_receipts.sql")),
    (22, include_str!("../../../nfprogress/core/sqlite/migrations/022_note_sync_resolution_inbox.sql")),
    (23, include_str!("../../../nfprogress/core/sqlite/migrations/023_note_sync_applied_resolutions.sql")),
    (24, include_str!("../../../nfprogress/core/sqlite/migrations/024_note_sync_multigeneration_tips.sql")),
    (25, include_str!("../../../nfprogress/core/sqlite/migrations/025_project_metadata_sync.sql")),
    (26, include_str!("../../../nfprogress/core/sqlite/migrations/026_project_metadata_authority.sql")),
    (27, include_str!("../../../nfprogress/core/sqlite/migrations/027_metadata_edge_continuation.sql")),
    (28, include_str!("../../../nfprogress/core/sqlite/migrations/028_stage_structural_sync.sql")),
    (29, include_str!("../../../nfprogress/core/sqlite/migrations/029_stage_structural_integration.sql")),
    (30, include_str!("../../../nfprogress/core/sqlite/migrations/030_account_sync_inbox.sql")),
    (31, include_str!("../../../nfprogress/core/sqlite/migrations/031_account_catalog.sql")),
    (32, include_str!("../../../nfprogress/core/sqlite/migrations/032_content_note_gate.sql")),
    (33, include_str!("../../../nfprogress/core/sqlite/migrations/033_content_note_writers.sql")),
    (34, include_str!("../../../nfprogress/core/sqlite/migrations/034_map_authority.sql")),
    (35, include_str!("../../../nfprogress/core/sqlite/migrations/035_document_authority.sql")),
    (36, include_str!("../../../nfprogress/core/sqlite/migrations/036_progress_authority.sql")),
    (37, include_str!("../../../nfprogress/core/sqlite/migrations/037_game_action_ledgers.sql")),
    (38, include_str!("../../../nfprogress/core/sqlite/migrations/038_cover_transfers.sql")),
];

pub fn open_database(path: &Path) -> Result<Connection, StorageError> {
    let new_database = !path.exists();
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
    )?;
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    register_fail_closed_remote_apply_guard(&connection)?;
    connection.execute_batch("PRAGMA foreign_keys = ON;")?;
    // F3 migration 005 extends the F2 outbox, so it must exist before the
    // versioned scripts are applied on an existing v4 database.
    connection.execute_batch(DOMAIN_EVENTS_SCHEMA)?;
    apply_migrations(&connection)?;
    configure_application_metadata(&connection, APPLICATION_VERSION, new_database)?;
    connection.execute_batch(DOMAIN_EVENTS_SCHEMA)?;
    validate_database(&connection)?;
    Ok(connection)
}

fn valid_application_version(value: &str) -> bool {
    if value.is_empty() || value.len() > 64 || !value.is_ascii() {
        return false;
    }
    let core_end = value.find(['-', '+']).unwrap_or(value.len());
    let core = &value[..core_end];
    if core.split('.').count() != 3
        || core
            .split('.')
            .any(|part| part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return false;
    }
    if core_end == value.len() {
        return true;
    }
    let suffix = &value[core_end + 1..];
    !suffix.is_empty()
        && suffix
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
}

fn configure_application_metadata(
    connection: &Connection,
    application_version: &str,
    new_database: bool,
) -> Result<(), StorageError> {
    if !valid_application_version(application_version) {
        return Err(StorageError::CorruptSchema(format!(
            "invalid nfprogress application version: {application_version}"
        )));
    }
    let transaction = connection.unchecked_transaction()?;
    for key in VERSION_KEYS {
        let saved = transaction
            .query_row(
                "SELECT value FROM application_metadata WHERE key=?1",
                [key],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?;
        if let Some(Some(value)) = saved.as_ref() {
            if !valid_application_version(value) {
                return Err(StorageError::CorruptSchema(format!(
                    "invalid application metadata {key}: {value}"
                )));
            }
        }
        if saved.is_none() {
            let initial = new_database.then_some(application_version);
            transaction.execute(
                "INSERT INTO application_metadata(key,value,updated_at) VALUES(?1,?2,datetime('now'))",
                rusqlite::params![key, initial],
            )?;
        }
    }
    let quoted_version = application_version.replace('\'', "''");
    for table in USER_DATA_TABLES {
        for operation in ["INSERT", "UPDATE", "DELETE"] {
            let trigger = format!(
                "nfprogress_app_version_{table}_{}",
                operation.to_ascii_lowercase()
            );
            transaction.execute_batch(&format!(
                "DROP TRIGGER IF EXISTS {trigger};\
                 CREATE TRIGGER {trigger} AFTER {operation} ON {table} BEGIN \
                 UPDATE application_metadata SET value='{quoted_version}',updated_at=datetime('now') \
                 WHERE key='data_last_written_by_version'; END;"
            ))?;
        }
    }
    transaction.commit()?;
    Ok(())
}

const DOMAIN_EVENTS_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS domain_events (event_id TEXT PRIMARY KEY, event_type TEXT NOT NULL, project_id TEXT NOT NULL, stage_id TEXT, progress_id TEXT, effective_date TEXT, delta_symbols REAL, context_json TEXT NOT NULL, created_at TEXT NOT NULL, processed_at TEXT, consumer TEXT NOT NULL DEFAULT 'game', version INTEGER NOT NULL DEFAULT 1); CREATE INDEX IF NOT EXISTS idx_domain_events_pending ON domain_events(consumer, processed_at, created_at);";

pub fn apply_migrations(connection: &Connection) -> Result<i64, StorageError> {
    register_fail_closed_remote_apply_guard(connection)?;
    connection.execute_batch("PRAGMA foreign_keys = ON;")?;
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_info (schema_version INTEGER NOT NULL);",
    )?;
    let versions: Vec<i64> = {
        let mut statement = connection.prepare("SELECT schema_version FROM schema_info")?;
        let result = statement
            .query_map([], |row| row.get(0))?
            .collect::<Result<Vec<i64>, _>>()?;
        result
    };
    if versions.len() > 1 {
        return Err(StorageError::CorruptSchema(
            "schema_info contains more than one version".to_string(),
        ));
    }
    let version = versions.first().copied().unwrap_or(0);
    if version > CURRENT_SCHEMA_VERSION {
        return Err(StorageError::UnsupportedSchema(version));
    }
    for (next_version, sql) in MIGRATIONS
        .iter()
        .filter(|(migration_version, _)| *migration_version > version)
    {
        let transaction = connection.unchecked_transaction()?;
        transaction.execute_batch(sql)?;
        if *next_version >= 3 {
            validate_project_order(&transaction)?;
        }
        if *next_version >= 4 {
            validate_stage_order(&transaction)?;
            validate_progress_order(&transaction)?;
        }
        transaction.execute("DELETE FROM schema_info", [])?;
        transaction.execute(
            "INSERT INTO schema_info(schema_version) VALUES (?1)",
            [next_version],
        )?;
        transaction.commit()?;
    }
    Ok(CURRENT_SCHEMA_VERSION)
}

pub(crate) fn validate_database(connection: &Connection) -> Result<(), StorageError> {
    let integrity: String = connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if integrity != "ok" {
        return Err(StorageError::CorruptSchema(format!(
            "PRAGMA integrity_check returned {integrity}"
        )));
    }
    let mut foreign_keys = connection.prepare("PRAGMA foreign_key_check")?;
    if foreign_keys.query([])?.next()?.is_some() {
        return Err(StorageError::CorruptSchema(
            "PRAGMA foreign_key_check returned violations".to_string(),
        ));
    }

    let table_names: HashSet<String> = connection
        .prepare("SELECT name FROM sqlite_master WHERE type='table'")?
        .query_map([], |row| row.get(0))?
        .collect::<Result<HashSet<_>, _>>()?;
    let required = [
        "projects",
        "stages",
        "progress_entries",
        "notes",
        "settings",
        "game_state",
        "mirror_state",
        "storage_ownership",
        "project_order",
        "stage_order",
        "progress_order",
        "documents",
        "document_bindings",
        "application_metadata",
        "cloud_sync_state",
        "cloud_sync_outbox",
        "cloud_sync_event_objects",
        "cloud_sync_inbox",
        "cloud_sync_entities",
        "cloud_sync_project_bindings",
        "cloud_sync_note_intents",
        "cloud_sync_note_intent_cursors",
        "cloud_account_bindings",
        "cloud_sync_project_bootstraps",
        "cloud_sync_note_conflict_groups",
        "cloud_sync_note_conflict_versions",
        "cloud_sync_note_conflict_tips",
        "cloud_sync_note_causal_history",
        "cloud_sync_note_pending_resolutions",
        "cloud_sync_note_resolution_outbox",
        "cloud_sync_note_resolution_dependencies",
        "cloud_sync_note_resolution_upload_receipts",
        "cloud_sync_note_applied_resolutions",
        "cloud_sync_note_applied_resolution_parents",
        "cloud_progress_migrations",
        "cloud_progress_candidates",
        "cloud_progress_events",
        "cloud_progress_tips",
        "cloud_progress_projection",
        "cloud_progress_apply_ledger",
        "cloud_progress_local_candidates",
        "cloud_progress_decisions",
        "cloud_progress_capture_cursor",
        "cloud_progress_write_intents",
    ];
    if required.iter().any(|table| !table_names.contains(*table)) {
        return Err(StorageError::CorruptSchema(
            "required SQLite table is missing".to_string(),
        ));
    }

    let schema_rows: Vec<i64> = connection
        .prepare("SELECT schema_version FROM schema_info")?
        .query_map([], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    if schema_rows != vec![CURRENT_SCHEMA_VERSION] {
        return Err(StorageError::CorruptSchema(
            "schema marker is not the current singular version".to_string(),
        ));
    }
    let fairness_cursor_count: i64 = connection.query_row(
        "SELECT count(*) FROM cloud_sync_note_intent_cursors",
        [],
        |row| row.get(0),
    )?;
    if fairness_cursor_count != 2 {
        return Err(StorageError::CorruptSchema(
            "Note sync fairness cursors are incomplete".to_string(),
        ));
    }

    let owners: HashSet<String> = connection
        .prepare("SELECT subsystem FROM storage_ownership")?
        .query_map([], |row| row.get(0))?
        .collect::<Result<HashSet<_>, _>>()?;
    let expected_owners: HashSet<String> = ["projects", "settings", "notes", "game"]
        .into_iter()
        .map(str::to_string)
        .collect();
    if owners != expected_owners {
        return Err(StorageError::CorruptSchema(
            "storage ownership rows are incomplete".to_string(),
        ));
    }

    validate_json_column(connection, "projects", "payload_json")?;
    validate_json_column(connection, "stages", "payload_json")?;
    validate_json_column(connection, "progress_entries", "payload_json")?;
    validate_json_column(connection, "notes", "payload_json")?;
    validate_json_column(connection, "settings", "value_json")?;
    validate_json_column(connection, "game_state", "payload_json")?;
    validate_json_column(connection, "documents", "content_json")?;
    validate_json_column(connection, "documents", "extensions_json")?;
    validate_json_column(connection, "document_bindings", "payload_json")?;
    validate_json_column(connection, "cloud_sync_note_intents", "snapshot_json")?;
    for key in VERSION_KEYS {
        let value = connection
            .query_row(
                "SELECT value FROM application_metadata WHERE key=?1",
                [key],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?;
        if let Some(Some(value)) = value {
            if !valid_application_version(&value) {
                return Err(StorageError::CorruptSchema(format!(
                    "invalid application metadata {key}: {value}"
                )));
            }
        }
    }

    validate_project_order(connection)?;
    validate_stage_order(connection)?;
    validate_progress_order(connection)?;
    Ok(())
}

fn validate_project_order(connection: &Connection) -> Result<(), StorageError> {
    let project_ids: HashSet<String> = connection
        .prepare("SELECT id FROM projects")?
        .query_map([], |row| row.get(0))?
        .collect::<Result<HashSet<_>, _>>()?;
    let order_rows: Vec<(String, i64)> = connection
        .prepare("SELECT project_id, position FROM project_order ORDER BY position, project_id")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<Vec<_>, rusqlite::Error>>()?;
    let positions: Vec<i64> = order_rows.iter().map(|(_, position)| *position).collect();
    let order_ids: HashSet<&str> = order_rows.iter().map(|(id, _)| id.as_str()).collect();
    if positions != (0..positions.len() as i64).collect::<Vec<_>>() {
        return Err(StorageError::CorruptSchema(
            "project ordering positions are not contiguous".to_string(),
        ));
    }
    if order_ids.len() != order_rows.len()
        || order_ids.len() != project_ids.len()
        || !order_ids.iter().all(|id| project_ids.contains(*id))
    {
        return Err(StorageError::CorruptSchema(
            "project ordering is incomplete".to_string(),
        ));
    }
    Ok(())
}

fn validate_stage_order(connection: &Connection) -> Result<(), StorageError> {
    let stages: HashMap<String, String> = connection
        .prepare("SELECT id, project_id FROM stages")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<HashMap<_, _>, rusqlite::Error>>()?;
    let rows: Vec<(String, String, i64)> = connection
        .prepare("SELECT stage_id, project_id, position FROM stage_order ORDER BY project_id, position, stage_id")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<Result<Vec<_>, rusqlite::Error>>()?;
    if rows.len() != stages.len()
        || rows.iter().map(|(id, _, _)| id).collect::<HashSet<_>>() != stages.keys().collect()
        || rows
            .iter()
            .any(|(id, project_id, _)| stages.get(id) != Some(project_id))
    {
        return Err(StorageError::CorruptSchema(
            "stage ordering is incomplete".to_string(),
        ));
    }
    let mut positions: HashMap<&str, Vec<i64>> = HashMap::new();
    for (_, project_id, position) in &rows {
        positions.entry(project_id).or_default().push(*position);
    }
    if positions
        .values()
        .any(|values| values != &(0..values.len() as i64).collect::<Vec<_>>())
    {
        return Err(StorageError::CorruptSchema(
            "stage ordering positions are not contiguous".to_string(),
        ));
    }
    Ok(())
}

pub(crate) fn compact_progress_order(connection: &Connection) -> rusqlite::Result<()> {
    let mut query = connection.prepare("SELECT entry_id FROM progress_order ORDER BY position")?;
    let ids = query
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut update = connection
        .prepare("UPDATE progress_order SET position=?1 WHERE entry_id=?2 AND position<>?1")?;
    for (position, id) in ids.iter().enumerate() {
        update.execute(rusqlite::params![position as i64, id])?;
    }
    Ok(())
}

fn validate_progress_order(connection: &Connection) -> Result<(), StorageError> {
    let progress_ids: HashSet<String> = connection
        .prepare("SELECT id FROM progress_entries")?
        .query_map([], |row| row.get(0))?
        .collect::<Result<HashSet<_>, _>>()?;
    let rows: Vec<(String, i64)> = connection
        .prepare("SELECT entry_id, position FROM progress_order ORDER BY position, entry_id")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<Vec<_>, rusqlite::Error>>()?;
    let positions: Vec<i64> = rows.iter().map(|(_, position)| *position).collect();
    let order_ids: HashSet<&str> = rows.iter().map(|(id, _)| id.as_str()).collect();
    if order_ids != progress_ids.iter().map(String::as_str).collect()
        || order_ids.len() != rows.len()
    {
        return Err(StorageError::CorruptSchema(
            "progress ordering is incomplete".to_string(),
        ));
    }
    if positions != (0..positions.len() as i64).collect::<Vec<_>>() {
        return Err(StorageError::CorruptSchema(
            "progress ordering positions are not contiguous".to_string(),
        ));
    }
    Ok(())
}

fn validate_json_column(
    connection: &Connection,
    table: &str,
    column: &str,
) -> Result<(), StorageError> {
    let query = format!("SELECT {column} FROM {table}");
    let mut statement = connection.prepare(&query)?;
    let values = statement.query_map([], |row| row.get::<_, String>(0))?;
    for value in values {
        let value = value?;
        serde_json::from_str::<serde_json::Value>(&value).map_err(|error| {
            StorageError::CorruptSchema(format!("invalid JSON in {table}.{column}: {error}"))
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {

    #[test]
    fn sqlite_content_reader_rotation_survives_more_than_one_cycle_and_reopen() {
        const USER: &str = "123e4567-e89b-42d3-a456-426614174000";
        const DEVICE: &str = "123e4567-e89b-42d3-a456-426614174001";
        fn listed(db: &Connection, kind: &str, after: i64) -> Vec<serde_json::Value> {
            match kind {
                "map" => crate::map_sync::received(db, "test", after, 8).unwrap(),
                "document" => crate::document_sync::received(db, "test", after, 8).unwrap(),
                "progress" => crate::progress_sync::received(db, "test", after, 8).unwrap(),
                "note" => crate::note_sync::list_received_note_sync_inbox_page(
                    db,
                    &crate::note_sync::ListReceivedNoteSyncInboxPageCommand {
                        account_id: "test".into(),
                        device_id: DEVICE.into(),
                        canonical_user_id: USER.into(),
                        kind: crate::note_sync::ReceivedNoteSyncInboxPageKind::Content,
                        limit: 8,
                        after_server_sequence: after,
                    },
                )
                .unwrap()
                .into_iter()
                .map(|row| serde_json::to_value(row).unwrap())
                .collect(),
                _ => unreachable!(),
            }
        }
        for kind in ["note", "map", "document", "progress"] {
            let path = std::env::temp_dir().join(format!(
                "reader-{kind}-{}.db",
                crate::project_metadata_sync::new_event_id().unwrap()
            ));
            let db = open_database(&path).unwrap();
            db.execute("INSERT INTO cloud_sync_state(account_id,device_id,created_at,updated_at) VALUES('test',?1,'now','now')",[DEVICE]).unwrap();
            db.execute(
                "INSERT INTO cloud_account_bindings VALUES('test',?1,'now','now')",
                [USER],
            )
            .unwrap();
            for sequence in 1..=40 {
                let id = crate::project_metadata_sync::new_event_id().unwrap();
                db.execute("INSERT INTO cloud_sync_inbox(account_id,event_id,server_sequence,device_id,project_id,entity_id,entity_type,operation,sync_revision,updated_at,state,received_at) VALUES('test',?1,?2,?3,'p',?1,?4,'event',1,'2026-10-01T00:00:00.000000Z','orphan','now')",rusqlite::params![id,sequence,DEVICE,kind]).unwrap();
                db.execute(
                    "INSERT INTO cloud_sync_event_objects VALUES('test',?1,1,1,?2,?3,'now')",
                    rusqlite::params![id, vec![4u8; 24], vec![5u8; 32]],
                )
                .unwrap();
            }
            let mut after = 0;
            for _ in 0..4 {
                let rows = listed(&db, kind, after);
                assert_eq!(rows.len(), 8);
                after = rows.last().unwrap()["server_sequence"].as_i64().unwrap();
            }
            assert_eq!(after, 32);
            drop(db);
            let db = open_database(&path).unwrap();
            let next = listed(&db, kind, 0);
            assert_eq!(
                next.iter()
                    .map(|r| r["server_sequence"].as_i64().unwrap())
                    .collect::<Vec<_>>(),
                (33..=40).collect::<Vec<_>>()
            );
            assert_eq!(
                db.query_row("SELECT ack_cursor FROM cloud_sync_state", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                0
            );
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM cloud_sync_inbox WHERE state='orphan'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
                40
            );
            assert_eq!(
                db.query_row("SELECT count(*) FROM cloud_sync_outbox", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                0
            );
            drop(db);
            std::fs::remove_file(path).unwrap();
        }
    }
    use super::*;

    const TEST_DEVICE_ID: &str = "123e4567-e89b-42d3-a456-426614174001";
    const TEST_EVENT_ID: &str = "123e4567-e89b-42d3-a456-426614174002";

    fn bound_note_database() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        apply_migrations(&connection).unwrap();
        connection
            .execute(
                "INSERT INTO projects(id,name,infinite,unit,status,payload_json)
             VALUES('project','Project',0,'symbols','active','{}')",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO cloud_sync_state(
                account_id,device_id,pull_cursor,ack_cursor,created_at,updated_at
             ) VALUES('account',?1,0,0,'now','now')",
                [TEST_DEVICE_ID],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO cloud_sync_project_bindings(
                project_id,account_id,created_at,updated_at
             ) VALUES('project','account','now','now')",
                [],
            )
            .unwrap();
        connection
    }

    fn note_payload(content: &str, updated_at: &str) -> String {
        serde_json::json!({
            "id": "note", "project_id": "project", "stage_id": null,
            "source_type": "project", "source_map_id": null,
            "source_node_id": null, "content_format": "html",
            "title": "Title", "content": content, "checklist": [],
            "color": "default", "pinned": false, "archived": false,
            "sort_order": 0, "tags": [], "created_at": "created",
            "updated_at": updated_at, "revision": 0, "metadata": {}
        })
        .to_string()
    }

    fn upsert_authorization<'a>(
        payload_json: &'a str,
        prior_payload_json: Option<&'a str>,
    ) -> RemoteApplyAuthorization<'a> {
        RemoteApplyAuthorization {
            event_id: TEST_EVENT_ID,
            account_id: "account",
            project_id: "project",
            entity_id: "note",
            operation: "upsert",
            payload_json: Some(payload_json),
            prior_payload_json,
        }
    }

    #[test]
    fn ordinary_connection_and_sql_only_authorization_are_fail_closed() {
        let connection = bound_note_database();
        let payload = note_payload("remote", "one");
        connection
            .execute(
                "INSERT INTO cloud_sync_remote_apply_authorizations VALUES(
                ?1,'account','project','note','upsert',?2,NULL,'forged')",
                rusqlite::params![TEST_EVENT_ID, payload],
            )
            .unwrap();
        assert!(connection
            .execute(
                "INSERT INTO notes(id,project_id,stage_id,updated_at,payload_json)
             VALUES('note','project',NULL,'one',?1)",
                [&payload],
            )
            .is_err());
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM notes", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM cloud_sync_remote_apply_authorizations",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
    }

    #[test]
    fn matching_local_intent_still_allows_write_without_remote_authorization() {
        let connection = bound_note_database();
        let payload = note_payload("local", "one");
        connection
            .execute(
                "INSERT INTO cloud_sync_outbox(
                event_id,account_id,device_id,project_id,entity_id,entity_type,
                operation,revision,updated_at,deleted_at,created_at,parent_event_id,
                local_ordinal,lifecycle
             ) VALUES(?1,'account',?2,'project','note','note','upsert',1,
                'one',NULL,'one',NULL,1,'unsealed')",
                rusqlite::params![TEST_EVENT_ID, TEST_DEVICE_ID],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO cloud_sync_note_intents(
                event_id,mutation_generation,snapshot_json,seal_state,
                seal_attempt_count,state_updated_at
             ) VALUES(?1,1,?2,'pending',0,'one')",
                rusqlite::params![TEST_EVENT_ID, payload],
            )
            .unwrap();

        connection
            .execute(
                "INSERT INTO notes(id,project_id,stage_id,updated_at,payload_json)
             VALUES('note','project',NULL,'one',?1)",
                [&payload],
            )
            .unwrap();

        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM notes", [], |row| row.get::<_, i64>(0),)
                .unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM cloud_sync_note_intents", [], |row| {
                    row.get::<_, i64>(0)
                },)
                .unwrap(),
            1
        );
        assert_eq!(
            connection
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
    fn privileged_connection_authorizes_one_exact_remote_write() {
        let connection = bound_note_database();
        let mut privileged = PrivilegedRemoteApplyConnection::from_connection(connection).unwrap();
        let payload = note_payload("remote", "one");
        privileged
            .authorize_once(upsert_authorization(&payload, None), |transaction| {
                transaction.execute(
                    "INSERT INTO notes(id,project_id,stage_id,updated_at,payload_json)
                 VALUES('note','project',NULL,'one',?1)",
                    [&payload],
                )?;
                Ok(())
            })
            .unwrap();
        assert_eq!(
            privileged
                .connection()
                .query_row("SELECT count(*) FROM notes", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            privileged
                .connection()
                .query_row(
                    "SELECT count(*) FROM cloud_sync_remote_apply_authorizations",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        assert!(privileged.capability.lock().unwrap().is_none());
        assert!(privileged
            .connection()
            .execute("UPDATE notes SET updated_at='direct' WHERE id='note'", [],)
            .is_err());
    }

    #[test]
    fn second_write_rolls_back_and_revokes_capability() {
        let connection = bound_note_database();
        let mut privileged = PrivilegedRemoteApplyConnection::from_connection(connection).unwrap();
        let first = note_payload("first", "one");
        privileged
            .authorize_once(upsert_authorization(&first, None), |transaction| {
                transaction.execute(
                    "INSERT INTO notes VALUES('note','project',NULL,'one',?1)",
                    [&first],
                )?;
                Ok(())
            })
            .unwrap();
        let second = note_payload("second", "two");
        let result =
            privileged.authorize_once(upsert_authorization(&second, Some(&first)), |transaction| {
                transaction.execute(
                    "UPDATE notes SET updated_at='two',payload_json=?1 WHERE id='note'",
                    [&second],
                )?;
                transaction.execute(
                    "UPDATE notes SET updated_at='two',payload_json=?1 WHERE id='note'",
                    [&second],
                )?;
                Ok(())
            });
        assert!(result.is_err());
        assert_eq!(
            privileged
                .connection()
                .query_row(
                    "SELECT payload_json FROM notes WHERE id='note'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            first
        );
        assert_eq!(
            privileged
                .connection()
                .query_row(
                    "SELECT count(*) FROM cloud_sync_remote_apply_authorizations",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        assert!(privileged.capability.lock().unwrap().is_none());
    }

    #[test]
    fn operation_error_rolls_back_authorization_and_restores_default_deny() {
        let connection = bound_note_database();
        let mut privileged = PrivilegedRemoteApplyConnection::from_connection(connection).unwrap();
        let payload = note_payload("remote", "one");
        let result: Result<(), StorageError> =
            privileged.authorize_once(upsert_authorization(&payload, None), |transaction| {
                transaction.execute(
                    "INSERT INTO notes VALUES('note','project',NULL,'one',?1)",
                    [&payload],
                )?;
                Err(rusqlite::Error::ExecuteReturnedResults)
            });
        assert!(result.is_err());
        assert_eq!(
            privileged
                .connection()
                .query_row("SELECT count(*) FROM notes", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            privileged
                .connection()
                .query_row(
                    "SELECT count(*) FROM cloud_sync_remote_apply_authorizations",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        assert!(privileged.capability.lock().unwrap().is_none());
        assert!(privileged
            .connection()
            .execute(
                "INSERT INTO notes VALUES('note','project',NULL,'one',?1)",
                [&payload],
            )
            .is_err());
    }

    #[test]
    fn privileged_guard_is_one_shot_while_ordinary_connection_is_fail_closed() {
        let connection = Connection::open_in_memory().unwrap();
        register_fail_closed_remote_apply_guard(&connection).unwrap();
        connection.execute_batch(
            "CREATE TABLE authorizations(capability TEXT PRIMARY KEY);\
             CREATE TABLE notes(value TEXT NOT NULL);\
             CREATE TRIGGER notes_guard BEFORE INSERT ON notes WHEN NOT EXISTS\
               (SELECT 1 FROM authorizations WHERE note_sync_remote_apply_authorized(capability))\
               BEGIN SELECT RAISE(ABORT, 'not authorized'); END;\
             CREATE TRIGGER notes_consume AFTER INSERT ON notes BEGIN DELETE FROM authorizations; END;",
        ).unwrap();
        connection
            .execute("INSERT INTO authorizations VALUES ('capability')", [])
            .unwrap();
        assert!(connection
            .execute("INSERT INTO notes VALUES ('ordinary')", [])
            .is_err());

        let capability = Arc::new(Mutex::new(Some("capability".to_string())));
        let state = capability.clone();
        connection
            .create_scalar_function(
                "note_sync_remote_apply_authorized",
                1,
                FunctionFlags::SQLITE_UTF8,
                move |ctx| {
                    Ok(state.lock().unwrap().as_deref() == Some(ctx.get::<String>(0)?.as_str()))
                },
            )
            .unwrap();
        assert_eq!(
            connection
                .query_row("PRAGMA trusted_schema", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            1
        );
        connection
            .execute("DELETE FROM authorizations", [])
            .unwrap();
        let rollback = connection.unchecked_transaction().unwrap();
        rollback
            .execute("INSERT INTO authorizations VALUES ('capability')", [])
            .unwrap();
        rollback
            .execute("INSERT INTO notes VALUES ('rolled-back')", [])
            .unwrap();
        rollback.rollback().unwrap();
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM authorizations", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM notes", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            0
        );
        let transaction = connection.unchecked_transaction().unwrap();
        transaction
            .execute("INSERT INTO authorizations VALUES ('capability')", [])
            .unwrap();
        transaction
            .execute("INSERT INTO notes VALUES ('applied')", [])
            .unwrap();
        transaction.commit().unwrap();
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM authorizations", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert!(connection
            .execute("INSERT INTO notes VALUES ('replay')", [])
            .is_err());
    }

    #[test]
    fn fresh_database_reaches_latest_schema() {
        let connection = Connection::open_in_memory().unwrap();
        assert_eq!(
            apply_migrations(&connection).unwrap(),
            CURRENT_SCHEMA_VERSION
        );
        assert_eq!(
            connection
                .query_row("SELECT schema_version FROM schema_info", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            CURRENT_SCHEMA_VERSION
        );
        assert!(
            connection
                .query_row(
                    "SELECT owner FROM storage_ownership WHERE subsystem='projects'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap()
                == "pickle"
        );
        for table in [
            "cloud_sync_event_objects",
            "cloud_sync_inbox",
            "cloud_sync_entities",
            "cloud_sync_project_bindings",
            "cloud_sync_note_intents",
            "cloud_sync_note_intent_cursors",
            "cloud_account_bindings",
        ] {
            assert!(connection
                .query_row(
                    "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
                    [table],
                    |_| Ok(()),
                )
                .is_ok());
        }
    }

    #[test]
    fn v10_upgrade_adds_initialized_note_fairness_cursors() {
        let connection = Connection::open_in_memory().unwrap();
        for (_, sql) in MIGRATIONS.iter().take(10) {
            connection.execute_batch(sql).unwrap();
        }
        connection
            .execute_batch(
                "CREATE TABLE schema_info(schema_version INTEGER NOT NULL);
                 INSERT INTO schema_info VALUES(10);",
            )
            .unwrap();

        assert_eq!(
            apply_migrations(&connection).unwrap(),
            CURRENT_SCHEMA_VERSION
        );
        let cursors = connection
            .prepare(
                "SELECT mode,account_id,device_id,local_ordinal,event_id
                 FROM cloud_sync_note_intent_cursors ORDER BY mode",
            )
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            cursors,
            vec![
                ("regular".to_string(), None, None, None, None),
                ("retry_blocked".to_string(), None, None, None, None),
            ]
        );
        validate_database(&connection).unwrap();
        connection
            .execute(
                "DELETE FROM cloud_sync_note_intent_cursors WHERE mode='regular'",
                [],
            )
            .unwrap();
        assert!(matches!(
            validate_database(&connection),
            Err(StorageError::CorruptSchema(message))
                if message.contains("fairness cursors are incomplete")
        ));
    }

    #[test]
    fn v11_upgrade_adds_empty_cloud_account_bindings() {
        let connection = Connection::open_in_memory().unwrap();
        for (_, sql) in MIGRATIONS.iter().take(11) {
            connection.execute_batch(sql).unwrap();
        }
        connection
            .execute_batch(
                "CREATE TABLE schema_info(schema_version INTEGER NOT NULL);
                 INSERT INTO schema_info VALUES(11);",
            )
            .unwrap();

        assert_eq!(
            apply_migrations(&connection).unwrap(),
            CURRENT_SCHEMA_VERSION
        );
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM cloud_account_bindings", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            0
        );
        validate_database(&connection).unwrap();
    }

    #[test]
    fn application_versions_track_creation_and_successful_user_writes() {
        let connection = Connection::open_in_memory().unwrap();
        apply_migrations(&connection).unwrap();
        configure_application_metadata(&connection, "1.2.3", true).unwrap();
        let read = |key: &str| {
            connection
                .query_row(
                    "SELECT value FROM application_metadata WHERE key=?1",
                    [key],
                    |row| row.get::<_, Option<String>>(0),
                )
                .unwrap()
        };
        assert_eq!(read("data_created_by_version").as_deref(), Some("1.2.3"));
        assert_eq!(
            read("data_last_written_by_version").as_deref(),
            Some("1.2.3")
        );

        // Merely opening with a different valid version changes no persisted
        // compatibility state and never blocks the database.
        configure_application_metadata(&connection, "9.8.7-beta.1", false).unwrap();
        assert_eq!(read("data_created_by_version").as_deref(), Some("1.2.3"));
        assert_eq!(
            read("data_last_written_by_version").as_deref(),
            Some("1.2.3")
        );
        connection
            .execute(
                "INSERT INTO settings(key,value_json) VALUES('theme','\"dark\"')",
                [],
            )
            .unwrap();
        assert_eq!(
            read("data_last_written_by_version").as_deref(),
            Some("9.8.7-beta.1")
        );
        assert_eq!(read("data_created_by_version").as_deref(), Some("1.2.3"));

        connection.execute("INSERT INTO cloud_sync_state(account_id,device_id,pull_cursor,ack_cursor,created_at,updated_at) VALUES('account','123e4567-e89b-42d3-a456-426614174000',0,0,'now','now')", []).unwrap();
        assert_eq!(
            read("data_last_written_by_version").as_deref(),
            Some("9.8.7-beta.1")
        );
        connection.execute("INSERT INTO cloud_sync_event_objects VALUES('account','123e4567-e89b-42d3-a456-426614174001',1,1,x'000000000000000000000000000000000000000000000000',x'00000000000000000000000000000000','now')", []).unwrap();
        connection.execute("INSERT INTO cloud_sync_inbox(account_id,event_id,server_sequence,device_id,project_id,entity_id,entity_type,operation,sync_revision,updated_at,deleted_at,state,received_at) VALUES('account','123e4567-e89b-42d3-a456-426614174002',1,'123e4567-e89b-42d3-a456-426614174000','project','entity','note','upsert',1,'now',NULL,'received','now')", []).unwrap();
        connection.execute("INSERT INTO cloud_sync_entities VALUES('account','project','entity','note','123e4567-e89b-42d3-a456-426614174002',1,NULL,NULL,'now')", []).unwrap();
        connection.execute("INSERT INTO projects(id,name,infinite,unit,status,payload_json) VALUES('project','Project',0,'symbols','active','{}')", []).unwrap();
        connection
            .execute(
                "INSERT INTO project_order(project_id,position) VALUES('project',0)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO cloud_sync_project_bindings VALUES('project','account','now','now')",
                [],
            )
            .unwrap();
        connection.execute("INSERT INTO cloud_sync_outbox(event_id,account_id,device_id,project_id,entity_id,entity_type,operation,revision,updated_at,deleted_at,created_at,parent_event_id,local_ordinal,lifecycle) VALUES('123e4567-e89b-42d3-a456-426614174004','account','123e4567-e89b-42d3-a456-426614174000','project','note','note','upsert',1,'now',NULL,'now',NULL,1,'unsealed')", []).unwrap();
        connection.execute("INSERT INTO cloud_sync_note_intents(event_id,mutation_generation,snapshot_json,seal_state,seal_attempt_count,state_updated_at) VALUES('123e4567-e89b-42d3-a456-426614174004',1,'{\"id\":\"note\",\"project_id\":\"project\"}','pending',0,'now')", []).unwrap();
        assert_eq!(
            read("data_last_written_by_version").as_deref(),
            Some("9.8.7-beta.1")
        );
    }

    #[test]
    fn existing_database_without_version_rows_opens_as_unknown_until_write() {
        let connection = Connection::open_in_memory().unwrap();
        apply_migrations(&connection).unwrap();

        configure_application_metadata(&connection, "5.3.9", false).unwrap();
        let created: Option<String> = connection
            .query_row(
                "SELECT value FROM application_metadata WHERE key='data_created_by_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let last_written: Option<String> = connection
            .query_row(
                "SELECT value FROM application_metadata WHERE key='data_last_written_by_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(created.is_none());
        assert!(last_written.is_none());

        connection
            .execute(
                "INSERT INTO settings(key,value_json) VALUES('language','\"ru\"')",
                [],
            )
            .unwrap();
        let last_written: String = connection
            .query_row(
                "SELECT value FROM application_metadata WHERE key='data_last_written_by_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(last_written, "5.3.9");
    }

    #[test]
    fn future_schema_is_rejected() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute(
                "CREATE TABLE schema_info(schema_version INTEGER NOT NULL)",
                [],
            )
            .unwrap();
        connection
            .execute("INSERT INTO schema_info VALUES(99)", [])
            .unwrap();
        assert!(matches!(
            apply_migrations(&connection),
            Err(StorageError::UnsupportedSchema(99))
        ));
    }

    #[test]
    fn populated_v3_upgrade_preserves_settings_notes_and_pickle_owner() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(MIGRATIONS[0].1).unwrap();
        connection.execute_batch(MIGRATIONS[1].1).unwrap();
        connection.execute_batch(MIGRATIONS[2].1).unwrap();
        connection
            .execute_batch("CREATE TABLE schema_info(schema_version INTEGER NOT NULL);")
            .unwrap();
        connection
            .execute("INSERT INTO schema_info VALUES(3)", [])
            .unwrap();
        connection
            .execute(
                "INSERT INTO projects VALUES('p', 'P', 1, 0, 'symbols', 'активен', NULL, NULL, '{}')",
                [],
            )
            .unwrap();
        connection
            .execute("INSERT INTO project_order VALUES('p', 0)", [])
            .unwrap();
        connection
            .execute("INSERT INTO settings VALUES('preserve', 'true')", [])
            .unwrap();
        connection
            .execute("INSERT INTO notes VALUES('n', 'p', NULL, 'old', '{}')", [])
            .unwrap();

        apply_migrations(&connection).unwrap();

        assert_eq!(
            connection
                .query_row("SELECT schema_version FROM schema_info", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            CURRENT_SCHEMA_VERSION
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT value_json FROM settings WHERE key='preserve'",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            "true"
        );
        assert_eq!(
            connection
                .query_row("SELECT id FROM notes", [], |row| row.get::<_, String>(0))
                .unwrap(),
            "n"
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT owner FROM storage_ownership WHERE subsystem='projects'",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            "pickle"
        );
    }

    #[test]
    fn database_validation_rejects_missing_latest_table() {
        let connection = Connection::open_in_memory().unwrap();
        apply_migrations(&connection).unwrap();
        connection.execute("DROP TABLE documents", []).unwrap();
        assert!(matches!(
            validate_database(&connection),
            Err(StorageError::CorruptSchema(message)) if message.contains("required SQLite table")
        ));
    }

    #[test]
    fn database_validation_rejects_missing_sync_tables() {
        for table in [
            "cloud_sync_state",
            "cloud_sync_outbox",
            "cloud_sync_event_objects",
            "cloud_sync_inbox",
            "cloud_sync_entities",
            "cloud_sync_project_bindings",
            "cloud_sync_note_intents",
            "cloud_sync_note_intent_cursors",
            "cloud_account_bindings",
        ] {
            let connection = Connection::open_in_memory().unwrap();
            apply_migrations(&connection).unwrap();
            connection
                .execute(&format!("DROP TABLE {table}"), [])
                .unwrap();
            assert!(matches!(
                validate_database(&connection),
                Err(StorageError::CorruptSchema(message)) if message.contains("required SQLite table")
            ));
        }
    }

    #[test]
    fn populated_v8_upgrade_preserves_c9_outbox_row_and_validates_latest() {
        let connection = Connection::open_in_memory().unwrap();
        for (_, sql) in MIGRATIONS.iter().take(8) {
            connection.execute_batch(sql).unwrap();
        }
        connection
            .execute_batch("CREATE TABLE schema_info(schema_version INTEGER NOT NULL); INSERT INTO schema_info VALUES(8);")
            .unwrap();
        connection.execute("INSERT INTO projects(id,name,infinite,unit,status,payload_json) VALUES('p','P',0,'symbols','активен','{}')", []).unwrap();
        connection
            .execute(
                "INSERT INTO project_order(project_id,position) VALUES('p',0)",
                [],
            )
            .unwrap();
        connection.execute("INSERT INTO notes(id,project_id,stage_id,updated_at,payload_json) VALUES('n','p',NULL,'old','{\"revision\":7}')", []).unwrap();
        connection.execute("INSERT INTO documents(id,scope_key,project_id,stage_id,title,content_json,content_format,extensions_json) VALUES('d','project:p','p',NULL,'D','{}','json','{}')", []).unwrap();
        connection
            .execute(
                "INSERT INTO settings(key,value_json) VALUES('theme','\"dark\"')",
                [],
            )
            .unwrap();
        connection.execute("INSERT INTO application_metadata(key,value,updated_at) VALUES('data_last_written_by_version','6.0.0','now')", []).unwrap();
        connection.execute("INSERT INTO cloud_sync_state(account_id,device_id,pull_cursor,ack_cursor,created_at,updated_at) VALUES('account','123e4567-e89b-42d3-a456-426614174000',0,0,'created','updated')", []).unwrap();
        connection.execute("INSERT INTO cloud_sync_outbox(event_id,account_id,device_id,project_id,entity_id,entity_type,operation,revision,updated_at,deleted_at,created_at,attempt_count,last_error,next_attempt_at) VALUES('123e4567-e89b-42d3-a456-426614174001','account','123e4567-e89b-42d3-a456-426614174000','p','n','note','upsert',7,'updated',NULL,'created',2,'retry','later')", []).unwrap();

        assert_eq!(
            apply_migrations(&connection).unwrap(),
            CURRENT_SCHEMA_VERSION
        );
        assert_eq!(
            connection
                .query_row("SELECT payload_json FROM notes WHERE id='n'", [], |row| row
                    .get::<_, String>(0))
                .unwrap(),
            "{\"revision\":7}"
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT parent_event_id,local_ordinal,lifecycle FROM cloud_sync_outbox",
                    [],
                    |row| Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?
                    ))
                )
                .unwrap(),
            (None, 0, "legacy".to_string())
        );
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM cloud_sync_event_objects", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            0
        );
        validate_database(&connection).unwrap();
    }

    #[test]
    fn database_validation_rejects_invalid_json_payload() {
        let connection = Connection::open_in_memory().unwrap();
        apply_migrations(&connection).unwrap();
        connection
            .execute(
                "INSERT INTO projects(id,name,infinite,unit,status,payload_json) VALUES('p','P',0,'symbols','активен','not-json')",
                [],
            )
            .unwrap();
        assert!(matches!(
            validate_database(&connection),
            Err(StorageError::CorruptSchema(message)) if message.contains("invalid JSON")
        ));
    }

    #[test]
    fn populated_pre_order_schema_cannot_advance_through_order_migration() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(MIGRATIONS[0].1).unwrap();
        connection.execute_batch(MIGRATIONS[1].1).unwrap();
        connection
            .execute_batch("CREATE TABLE schema_info(schema_version INTEGER NOT NULL);")
            .unwrap();
        connection
            .execute("INSERT INTO schema_info VALUES(2)", [])
            .unwrap();
        connection
            .execute(
                "INSERT INTO projects VALUES('p', 'P', 1, 0, 'symbols', 'активен', NULL, NULL, '{}')",
                [],
            )
            .unwrap();

        assert!(matches!(
            apply_migrations(&connection),
            Err(StorageError::CorruptSchema(message)) if message.contains("project ordering is incomplete")
        ));
        assert_eq!(
            connection
                .query_row("SELECT schema_version FROM schema_info", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            2
        );
    }
}
