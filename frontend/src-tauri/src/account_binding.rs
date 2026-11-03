//! Authoritative local-account to backend-user binding primitives.

use std::fmt::Write as _;

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};

#[derive(Debug)]
pub(crate) enum AccountBindingError {
    Database(rusqlite::Error),
    InvalidIdentity,
    MissingLocalAccount,
    IdentityMismatch,
    UserAlreadyBound,
    CorruptIdentityState,
    Random(String),
}

impl std::fmt::Display for AccountBindingError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Database(error) => write!(formatter, "SQLite error: {error}"),
            Self::InvalidIdentity => write!(formatter, "Invalid cloud account binding identity"),
            Self::MissingLocalAccount => write!(formatter, "Local cloud account does not exist"),
            Self::IdentityMismatch => write!(formatter, "Cloud account binding identity mismatch"),
            Self::UserAlreadyBound => write!(
                formatter,
                "Backend user is already bound to another local account"
            ),
            Self::CorruptIdentityState => write!(formatter, "Corrupt cloud identity state"),
            Self::Random(error) => write!(formatter, "Could not generate cloud identity: {error}"),
        }
    }
}

impl From<rusqlite::Error> for AccountBindingError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Database(error)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EnsureCloudAccountBindingCommand {
    pub local_account_id: String,
    pub canonical_user_id: String,
}

/// Narrow authenticated-session input for durable identity provisioning/read.
/// The caller obtains this canonical user id from its current auth context.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CloudIdentityCommand {
    pub canonical_user_id: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EnsureCloudAccountBindingResult {
    Created,
    Validated,
}

/// Durable local cloud identity. `local_account_id` is an opaque local scope,
/// intentionally distinct from the authenticated backend user UUID.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct CloudIdentity {
    pub local_account_id: String,
    pub device_id: String,
}

fn canonical_backend_user_id(value: &str) -> bool {
    if value.len() != 36 || value != value.to_ascii_lowercase() {
        return false;
    }
    value.bytes().enumerate().all(|(index, byte)| {
        if matches!(index, 8 | 13 | 18 | 23) {
            byte == b'-'
        } else {
            byte.is_ascii_hexdigit()
        }
    })
}

fn canonical_uuid(value: &str) -> bool {
    value.len() == 36
        && value == value.to_ascii_lowercase()
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

fn canonical_uuid_v4() -> Result<String, AccountBindingError> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|error| AccountBindingError::Random(error.to_string()))?;
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

fn read_cloud_identity_in_transaction(
    transaction: &Transaction<'_>,
    canonical_user_id: &str,
) -> Result<Option<CloudIdentity>, AccountBindingError> {
    let row = transaction
        .query_row(
            "SELECT binding.local_account_id,state.device_id,state.pull_cursor,state.ack_cursor
             FROM cloud_account_bindings AS binding
             LEFT JOIN cloud_sync_state AS state ON state.account_id=binding.local_account_id
             WHERE binding.canonical_user_id=?1",
            [canonical_user_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                ))
            },
        )
        .optional()?;
    let Some((local_account_id, device_id, pull_cursor, ack_cursor)) = row else {
        return Ok(None);
    };
    let (Some(device_id), Some(pull_cursor), Some(ack_cursor)) =
        (device_id, pull_cursor, ack_cursor)
    else {
        return Err(AccountBindingError::CorruptIdentityState);
    };
    if local_account_id.is_empty()
        || local_account_id.len() > 512
        || !canonical_uuid(&device_id)
        || pull_cursor < 0
        || ack_cursor < 0
    {
        return Err(AccountBindingError::CorruptIdentityState);
    }
    Ok(Some(CloudIdentity {
        local_account_id,
        device_id,
    }))
}

/// Reads an already-provisioned cloud identity for an authenticated canonical
/// user. A binding without a complete, valid durable state is fail-closed.
pub(crate) fn read_cloud_identity(
    connection: &mut Connection,
    canonical_user_id: &str,
) -> Result<Option<CloudIdentity>, AccountBindingError> {
    if !canonical_backend_user_id(canonical_user_id) {
        return Err(AccountBindingError::InvalidIdentity);
    }
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let identity = read_cloud_identity_in_transaction(&transaction, canonical_user_id)?;
    transaction.commit()?;
    Ok(identity)
}

/// Explicitly provisions a fresh local-account/device pair for an already
/// authenticated backend user, or returns that user's durable pair on replay.
/// It never adopts an unrelated unbound `cloud_sync_state` row.
pub(crate) fn provision_cloud_identity(
    connection: &mut Connection,
    canonical_user_id: &str,
) -> Result<CloudIdentity, AccountBindingError> {
    if !canonical_backend_user_id(canonical_user_id) {
        return Err(AccountBindingError::InvalidIdentity);
    }
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Some(identity) = read_cloud_identity_in_transaction(&transaction, canonical_user_id)? {
        transaction.commit()?;
        return Ok(identity);
    }

    let identity = CloudIdentity {
        local_account_id: canonical_uuid_v4()?,
        device_id: canonical_uuid_v4()?,
    };
    transaction.execute(
        "INSERT INTO cloud_sync_state(
            account_id,device_id,pull_cursor,ack_cursor,created_at,updated_at
         ) VALUES(?1,?2,0,0,strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        rusqlite::params![identity.local_account_id, identity.device_id],
    )?;
    transaction.execute(
        "INSERT INTO cloud_account_bindings(
            local_account_id,canonical_user_id,created_at,validated_at
         ) VALUES(?1,?2,strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        rusqlite::params![identity.local_account_id, canonical_user_id],
    )?;
    transaction.commit()?;
    Ok(identity)
}

pub(crate) fn ensure_cloud_account_binding(
    connection: &mut Connection,
    command: &EnsureCloudAccountBindingCommand,
) -> Result<EnsureCloudAccountBindingResult, AccountBindingError> {
    if command.local_account_id.is_empty()
        || command.local_account_id.len() > 512
        || !canonical_backend_user_id(&command.canonical_user_id)
    {
        return Err(AccountBindingError::InvalidIdentity);
    }
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let local_exists = transaction
        .query_row(
            "SELECT 1 FROM cloud_sync_state WHERE account_id=?1",
            [&command.local_account_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !local_exists {
        return Err(AccountBindingError::MissingLocalAccount);
    }
    let existing = transaction
        .query_row(
            "SELECT canonical_user_id FROM cloud_account_bindings WHERE local_account_id=?1",
            [&command.local_account_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if let Some(existing) = existing {
        if existing != command.canonical_user_id {
            return Err(AccountBindingError::IdentityMismatch);
        }
        transaction.execute(
            "UPDATE cloud_account_bindings
             SET validated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
             WHERE local_account_id=?1 AND canonical_user_id=?2",
            rusqlite::params![command.local_account_id, command.canonical_user_id],
        )?;
        transaction.commit()?;
        return Ok(EnsureCloudAccountBindingResult::Validated);
    }
    let user_binding = transaction
        .query_row(
            "SELECT local_account_id FROM cloud_account_bindings WHERE canonical_user_id=?1",
            [&command.canonical_user_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if user_binding.is_some() {
        return Err(AccountBindingError::UserAlreadyBound);
    }
    transaction.execute(
        "INSERT INTO cloud_account_bindings(
            local_account_id,canonical_user_id,created_at,validated_at
         ) VALUES(?1,?2,strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        rusqlite::params![command.local_account_id, command.canonical_user_id],
    )?;
    transaction.commit()?;
    Ok(EnsureCloudAccountBindingResult::Created)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Barrier};
    use std::time::Duration;

    use super::*;

    const DEVICE_ID: &str = "123e4567-e89b-42d3-a456-426614174000";
    const USER_ONE: &str = "abcdefab-0000-0000-0000-000000000101";
    const USER_TWO: &str = "00000000-0000-0000-0000-000000000102";
    static TEMP_DATABASE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    fn database() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        crate::sqlite::apply_migrations(&connection).unwrap();
        connection.execute(
            "INSERT INTO cloud_sync_state(account_id,device_id,pull_cursor,ack_cursor,created_at,updated_at)
             VALUES('local-account',?1,0,0,'now','now')",
            [DEVICE_ID],
        ).unwrap();
        connection
    }

    fn command(user_id: &str) -> EnsureCloudAccountBindingCommand {
        EnsureCloudAccountBindingCommand {
            local_account_id: "local-account".to_string(),
            canonical_user_id: user_id.to_string(),
        }
    }

    fn empty_database() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        crate::sqlite::apply_migrations(&connection).unwrap();
        connection
    }

    fn temporary_database_path(label: &str) -> PathBuf {
        let sequence = TEMP_DATABASE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "nfprogress-account-binding-{label}-{}-{sequence}.sqlite",
            std::process::id(),
        ))
    }

    #[test]
    fn authenticated_identity_creates_and_revalidates_an_immutable_binding() {
        let mut connection = database();
        assert_eq!(
            ensure_cloud_account_binding(&mut connection, &command(USER_ONE)).unwrap(),
            EnsureCloudAccountBindingResult::Created
        );
        assert_eq!(
            ensure_cloud_account_binding(&mut connection, &command(USER_ONE)).unwrap(),
            EnsureCloudAccountBindingResult::Validated
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT local_account_id,canonical_user_id FROM cloud_account_bindings",
                    [],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .unwrap(),
            ("local-account".to_string(), USER_ONE.to_string())
        );
    }

    #[test]
    fn mismatched_identity_fails_without_rewriting_the_binding() {
        let mut connection = database();
        ensure_cloud_account_binding(&mut connection, &command(USER_ONE)).unwrap();
        assert!(matches!(
            ensure_cloud_account_binding(&mut connection, &command(USER_TWO)),
            Err(AccountBindingError::IdentityMismatch)
        ));
        assert_eq!(connection.query_row(
            "SELECT canonical_user_id FROM cloud_account_bindings WHERE local_account_id='local-account'",
            [], |row| row.get::<_, String>(0),
        ).unwrap(), USER_ONE);
    }

    #[test]
    fn missing_account_and_noncanonical_user_are_rejected() {
        let mut connection = database();
        let missing = EnsureCloudAccountBindingCommand {
            local_account_id: "missing".to_string(),
            canonical_user_id: USER_ONE.to_string(),
        };
        assert!(matches!(
            ensure_cloud_account_binding(&mut connection, &missing),
            Err(AccountBindingError::MissingLocalAccount)
        ));
        assert!(matches!(
            ensure_cloud_account_binding(&mut connection, &command(&USER_ONE.to_ascii_uppercase())),
            Err(AccountBindingError::InvalidIdentity)
        ));
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM cloud_account_bindings", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn provision_creates_one_atomic_identity_without_touching_notes_or_sync_work() {
        let mut connection = empty_database();
        connection
            .execute(
                "INSERT INTO projects(id,name,infinite,unit,status,payload_json)
             VALUES('local-only','Local only',0,'symbols','active','{}')",
                [],
            )
            .unwrap();
        let identity = provision_cloud_identity(&mut connection, USER_ONE).unwrap();

        assert_ne!(identity.local_account_id, USER_ONE);
        assert!(canonical_uuid(&identity.local_account_id));
        assert!(canonical_uuid(&identity.device_id));
        assert_eq!(
            connection.query_row(
                "SELECT device_id,pull_cursor,ack_cursor FROM cloud_sync_state WHERE account_id=?1",
                [&identity.local_account_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?)),
            ).unwrap(),
            (identity.device_id.clone(), 0, 0),
        );
        assert_eq!(
            connection.query_row(
                "SELECT canonical_user_id FROM cloud_account_bindings WHERE local_account_id=?1",
                [&identity.local_account_id],
                |row| row.get::<_, String>(0),
            ).unwrap(),
            USER_ONE,
        );
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM notes", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM cloud_sync_outbox", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM cloud_sync_inbox", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT status FROM projects WHERE id='local-only'",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            "active"
        );
    }

    #[test]
    fn provision_replays_and_read_returns_the_same_durable_identity_after_restart() {
        let path = temporary_database_path("restart");
        let first = {
            let mut connection = Connection::open(&path).unwrap();
            crate::sqlite::apply_migrations(&connection).unwrap();
            let provisioned = provision_cloud_identity(&mut connection, USER_ONE).unwrap();
            assert_eq!(
                provision_cloud_identity(&mut connection, USER_ONE).unwrap(),
                provisioned
            );
            provisioned
        };
        let mut reopened = Connection::open(&path).unwrap();
        assert_eq!(
            read_cloud_identity(&mut reopened, USER_ONE).unwrap(),
            Some(first.clone())
        );
        assert_eq!(
            provision_cloud_identity(&mut reopened, USER_ONE).unwrap(),
            first
        );
        drop(reopened);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn concurrent_provisioning_returns_one_identity() {
        let path = temporary_database_path("concurrent");
        let connection = Connection::open(&path).unwrap();
        crate::sqlite::apply_migrations(&connection).unwrap();
        drop(connection);
        let barrier = Arc::new(Barrier::new(3));
        let mut handles = Vec::new();
        for _ in 0..2 {
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                let mut connection = Connection::open(path).unwrap();
                connection.busy_timeout(Duration::from_secs(5)).unwrap();
                barrier.wait();
                provision_cloud_identity(&mut connection, USER_ONE)
                    .map_err(|error| error.to_string())
            }));
        }
        barrier.wait();
        let first = handles.remove(0).join().unwrap().unwrap();
        let second = handles.remove(0).join().unwrap().unwrap();
        assert_eq!(first, second);
        let connection = Connection::open(&path).unwrap();
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM cloud_sync_state", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM cloud_account_bindings", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        drop(connection);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn provisioning_never_adopts_an_unbound_legacy_state_or_rebinds_it() {
        let mut connection = empty_database();
        connection.execute(
            "INSERT INTO cloud_sync_state(account_id,device_id,pull_cursor,ack_cursor,created_at,updated_at)
             VALUES('unbound-legacy',?1,7,3,'now','now')",
            [DEVICE_ID],
        ).unwrap();
        let identity = provision_cloud_identity(&mut connection, USER_ONE).unwrap();
        assert_ne!(identity.local_account_id, "unbound-legacy");
        assert_eq!(
            connection.query_row(
                "SELECT device_id,pull_cursor,ack_cursor FROM cloud_sync_state WHERE account_id='unbound-legacy'",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?)),
            ).unwrap(),
            (DEVICE_ID.to_string(), 7, 3),
        );
        assert!(matches!(
            ensure_cloud_account_binding(
                &mut connection,
                &EnsureCloudAccountBindingCommand {
                    local_account_id: identity.local_account_id.clone(),
                    canonical_user_id: USER_TWO.to_string(),
                }
            ),
            Err(AccountBindingError::IdentityMismatch)
        ));
        assert_eq!(
            read_cloud_identity(&mut connection, USER_ONE).unwrap(),
            Some(identity)
        );
    }

    #[test]
    fn incomplete_bound_state_is_fail_closed_without_repair_or_rebinding() {
        let mut connection = empty_database();
        let identity = provision_cloud_identity(&mut connection, USER_ONE).unwrap();
        connection
            .execute_batch("PRAGMA ignore_check_constraints = ON;")
            .unwrap();
        connection
            .execute(
                "UPDATE cloud_sync_state SET device_id='corrupt' WHERE account_id=?1",
                [&identity.local_account_id],
            )
            .unwrap();
        connection
            .execute_batch("PRAGMA ignore_check_constraints = OFF;")
            .unwrap();
        assert!(matches!(
            read_cloud_identity(&mut connection, USER_ONE),
            Err(AccountBindingError::CorruptIdentityState)
        ));
        assert!(matches!(
            provision_cloud_identity(&mut connection, USER_ONE),
            Err(AccountBindingError::CorruptIdentityState)
        ));
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM cloud_account_bindings", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT device_id FROM cloud_sync_state WHERE account_id=?1",
                    [&identity.local_account_id],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            "corrupt"
        );
    }

    #[test]
    fn binding_insert_failure_rolls_back_the_new_identity() {
        let mut connection = empty_database();
        connection
            .execute_batch(
                "CREATE TRIGGER cloud_identity_test_fail_binding
             BEFORE INSERT ON cloud_account_bindings
             BEGIN SELECT RAISE(ABORT, 'injected binding failure'); END;",
            )
            .unwrap();
        assert!(provision_cloud_identity(&mut connection, USER_ONE).is_err());
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM cloud_sync_state", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM cloud_account_bindings", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}
